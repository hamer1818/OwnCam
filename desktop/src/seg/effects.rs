//! Arka plan efektleri ve kare isleyicisi.
//!
//! Bir kare icin **tek** komut tamponu gonderiliyor: kareyi agin girdisine
//! cevir, agi kostur, arka plani hazirla, birlestir, onizlemeyi kucult.
//! Islemciye yalnizca bitmis kare ve kucuk onizleme geri okunuyor.

use super::gpu::{create_init_buffer, storage_entry, Segmenter};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Background {
    /// Efekt kapali - kare oldugu gibi geciyor.
    Off,
    /// Arka plani bulaniklastir. 0..1
    Blur(f32),
    /// Duz renk (0..1 RGB)
    Color([f32; 3]),
    /// Kullanicinin sectigi foto
    Image,
}

impl Background {
    fn mode(&self) -> u32 {
        match self {
            Background::Off => 0,
            Background::Blur(_) => 1,
            Background::Color(_) => 2,
            Background::Image => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    pub background: Background,
    /// Maske kenarinin sertligi. 0 ham maske (sac icin iyi), 1 keskin kesim.
    pub sharpness: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            background: Background::Off,
            sharpness: 0.35,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    width: u32,
    height: u32,
    small_w: u32,
    small_h: u32,
    mask_off: u32,
    mask_w: u32,
    mask_h: u32,
    mode: u32,
    input_off: u32,
    net_w: u32,
    net_h: u32,
    bg_w: u32,
    bg_h: u32,
    preview_w: u32,
    preview_h: u32,
    blur_radius: u32,
    color: [f32; 4],
    sharpness: f32,
    coverage_off: u32,
    yuv_word_off: u32,
    _pad: [f32; 1],
}

const PIPELINE_NAMES: [&str; 9] = [
    "to_network_input",
    "shrink",
    "blur_h",
    "blur_v",
    "composite",
    "preview_shrink",
    "mask_coverage",
    "to_yuv_luma",
    "to_yuv_chroma",
];
const TO_INPUT: usize = 0;
const SHRINK: usize = 1;
const BLUR_H: usize = 2;
const BLUR_V: usize = 3;
const COMPOSITE: usize = 4;
const PREVIEW: usize = 5;
const COVERAGE: usize = 6;
const YUV_LUMA: usize = 7;
const YUV_CHROMA: usize = 8;

/// Sanal kameraya giden karenin bicimi.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Piksel basina 4 bayt. Her olcude calisir.
    Rgba,
    /// Planar YUV420 (piksel basina 1,5 bayt). Geri okumayi 2,7 kat
    /// kucultuyor ve ikinci ffmpeg'i donusumden kurtariyor.
    Yuv420,
}

impl Format {
    pub fn ffmpeg_pix_fmt(self) -> &'static str {
        match self {
            Format::Rgba => "rgba",
            Format::Yuv420 => "yuv420p",
        }
    }

    fn frame_bytes(self, w: u32, h: u32) -> u64 {
        let px = (w as u64) * (h as u64);
        match self {
            Format::Rgba => px * 4,
            Format::Yuv420 => px * 3 / 2,
        }
    }
}

/// YUV yolu her is parcaciginin tam bir u32 yazmasina dayaniyor; bayt
/// duzeyinde okuma-degistirme-yazma yarisini boyle onluyoruz. Bu, duzlem
/// uzunluklarina 4'un kati olma sarti getiriyor. Kodlayici kareleri zaten
/// 16'ya hizali oldugu icin pratikte hep saglaniyor; saglanmazsa RGBA'ya
/// duselim - sessizce bozulmaktansa biraz daha pahali calissin.
pub fn output_format(w: u32, h: u32) -> Format {
    let luma_ok = w % 4 == 0 && (w * h) % 4 == 0;
    let chroma_ok = w % 2 == 0 && h % 2 == 0 && ((w / 2) * (h / 2)) % 4 == 0;
    if luma_ok && chroma_ok {
        Format::Yuv420
    } else {
        Format::Rgba
    }
}

/// Onizleme piksel butcesi. Alana gore olcekleniyor, genislige gore degil:
/// dik karede genislige gore olceklemek uc kat trafik uretiyordu.
const PREVIEW_PIXELS: u32 = 640 * 360;

struct Sized {
    w: u32,
    h: u32,
    format: Format,
    /// `out` icinde YUV bolgesinin basladigi kelime indeksi.
    yuv_word_off: u32,
    small_w: u32,
    small_h: u32,
    preview_w: u32,
    preview_h: u32,
    frame: wgpu::Buffer,
    out: wgpu::Buffer,
    preview: wgpu::Buffer,
    read_full: wgpu::Buffer,
    read_preview: wgpu::Buffer,
    read_coverage: wgpu::Buffer,
    bind: wgpu::BindGroup,
}

pub struct Processor {
    seg: Segmenter,
    layout: wgpu::BindGroupLayout,
    pipelines: Vec<wgpu::ComputePipeline>,
    params: wgpu::Buffer,
    background: wgpu::Buffer,
    bg_size: (u32, u32),
    sized: Option<Sized>,
}

pub struct Processed {
    /// Sanal kameraya gidecek kare; bicimi [`Processed::format`] soyluyor.
    pub frame: Vec<u8>,
    pub preview: Vec<u8>,
    pub preview_size: (u32, u32),
    /// `rgba` alanindaki verinin bicimi.
    pub format: Format,
    /// Maskenin ortalamasi (0..1). Efekt kapaliyken `None` - ag kosmuyor.
    /// Arayuz bunu "kisi bulunamadi" uyarisi icin kullaniyor.
    pub coverage: Option<f32>,
}

impl Processor {
    pub fn new() -> Result<Self, String> {
        let seg = Segmenter::new()?;
        let device = &seg.device;

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("efektler"),
            entries: &[
                storage_entry(0, false), // arena
                storage_entry(1, true),  // kare
                storage_entry(2, false), // cikti
                storage_entry(3, false), // bulanik a
                storage_entry(4, false), // bulanik b
                storage_entry(5, true),  // arka plan foto
                storage_entry(6, false), // onizleme
                wgpu::BindGroupLayoutEntry {
                    binding: 7,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("effects.wgsl"),
            source: wgpu::ShaderSource::Wgsl(std::borrow::Cow::Borrowed(include_str!(
                "effects.wgsl"
            ))),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipelines = PIPELINE_NAMES
            .iter()
            .map(|name| {
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(name),
                    layout: Some(&pipeline_layout),
                    module: &shader,
                    entry_point: Some(name),
                    compilation_options: Default::default(),
                    cache: None,
                })
            })
            .collect();

        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("efekt-parametreleri"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Foto secilmeden de baglama grubu kurulabilsin diye 1x1 yer tutucu.
        let background = create_init_buffer(
            device,
            "arka-plan-yer-tutucu",
            &[0u8; 4],
            wgpu::BufferUsages::STORAGE,
        );

        Ok(Self {
            seg,
            layout,
            pipelines,
            params,
            background,
            bg_size: (0, 0),
            sized: None,
        })
    }

    pub fn adapter_name(&self) -> &str {
        &self.seg.adapter_name
    }

    /// Arka plan fotografi. `rgba` uzunlugu w*h*4 olmali.
    pub fn set_background(&mut self, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
        if rgba.len() != (w as usize) * (h as usize) * 4 {
            return Err("arka plan boyutu verisiyle tutmuyor".into());
        }
        self.background = create_init_buffer(
            &self.seg.device,
            "arka-plan",
            rgba,
            wgpu::BufferUsages::STORAGE,
        );
        self.bg_size = (w, h);
        // Baglama grubu arka plan tamponunu tuttugu icin yeniden kurulmali.
        self.sized = None;
        Ok(())
    }

    fn ensure_sized(&mut self, w: u32, h: u32) {
        if let Some(s) = &self.sized {
            if s.w == w && s.h == h {
                return;
            }
        }
        let device = &self.seg.device;
        let px = (w as u64) * (h as u64);
        // Bulanik ceyrek cozunurlukte; en az 1 piksel.
        let (small_w, small_h) = ((w / 4).max(1), (h / 4).max(1));
        let scale = ((PREVIEW_PIXELS as f64) / (w as f64 * h as f64)).sqrt();
        let preview_w = ((w as f64 * scale) as u32).max(2) & !1;
        let preview_h = ((h as f64 * scale) as u32).max(2) & !1;

        let buf = |label: &str, size: u64, usage: wgpu::BufferUsages| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage,
                mapped_at_creation: false,
            })
        };
        let frame = buf(
            "kare",
            px * 4,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        );
        // RGBA bolgesi + (varsa) arkasina YUV bolgesi. Ayni tamponda
        // ayri bolgeler: yeni bir depolama baglamasi gerekmiyor.
        let format = output_format(w, h);
        let yuv_word_off = (px * 4 / 4) as u32; // = px kelime
        let out_words = px + match format {
            Format::Yuv420 => (px * 3 / 2).div_ceil(4),
            Format::Rgba => 0,
        };
        let out = buf(
            "kompozit",
            out_words * 4,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        );
        let blur_a = buf(
            "bulanik-a",
            (small_w as u64) * (small_h as u64) * 16,
            wgpu::BufferUsages::STORAGE,
        );
        let blur_b = buf(
            "bulanik-b",
            (small_w as u64) * (small_h as u64) * 16,
            wgpu::BufferUsages::STORAGE,
        );
        let preview = buf(
            "onizleme",
            (preview_w as u64) * (preview_h as u64) * 4,
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        );
        let read_full = buf(
            "okuma-tam",
            format.frame_bytes(w, h),
            wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        );
        let read_preview = buf(
            "okuma-onizleme",
            (preview_w as u64) * (preview_h as u64) * 4,
            wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        );
        let read_coverage = buf(
            "okuma-kapsam",
            4,
            wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        );

        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("efektler"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.seg.arena.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: frame.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: out.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: blur_a.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: blur_b.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: self.background.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: preview.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: self.params.as_entire_binding(),
                },
            ],
        });

        self.sized = Some(Sized {
            w,
            h,
            format,
            yuv_word_off,
            small_w,
            small_h,
            preview_w,
            preview_h,
            frame,
            out,
            preview,
            read_full,
            read_preview,
            read_coverage,
            bind,
        });
    }

    /// Bir kareyi isle. `rgba` uzunlugu w*h*4 olmali.
    pub fn process(
        &mut self,
        w: u32,
        h: u32,
        rgba: &[u8],
        settings: Settings,
    ) -> Result<Processed, String> {
        if rgba.len() != (w as usize) * (h as usize) * 4 {
            return Err(format!(
                "kare {}x{} icin {} bayt bekleniyordu, {} geldi",
                w,
                h,
                (w as usize) * (h as usize) * 4,
                rgba.len()
            ));
        }
        self.ensure_sized(w, h);
        let s = self.sized.as_ref().expect("tamponlar kuruldu");
        let mask = self.seg.mask_shape();

        let mut background = settings.background;
        if background == Background::Image && self.bg_size.0 == 0 {
            // Foto secilmeden foto modu istenirse sessizce kapali davran.
            background = Background::Off;
        }
        let color = match background {
            Background::Color(c) => [c[0], c[1], c[2], 1.0],
            _ => [0.0, 0.0, 0.0, 1.0],
        };
        let blur_radius = match background {
            Background::Blur(strength) => {
                (2.0 + strength.clamp(0.0, 1.0) * 10.0).round() as u32
            }
            _ => 0,
        };

        let params = Params {
            width: w,
            height: h,
            small_w: s.small_w,
            small_h: s.small_h,
            mask_off: self.seg.mask_offset(),
            mask_w: mask.w as u32,
            mask_h: mask.h as u32,
            mode: background.mode(),
            input_off: self.seg.input_offset(),
            net_w: self.seg.input_size().w as u32,
            net_h: self.seg.input_size().h as u32,
            bg_w: self.bg_size.0,
            bg_h: self.bg_size.1,
            preview_w: s.preview_w,
            preview_h: s.preview_h,
            blur_radius,
            color,
            sharpness: settings.sharpness,
            coverage_off: self.seg.scratch_offset(),
            yuv_word_off: s.yuv_word_off,
            _pad: [0.0; 1],
        };
        self.seg
            .queue
            .write_buffer(&self.params, 0, bytemuck::bytes_of(&params));
        self.seg.queue.write_buffer(&s.frame, 0, rgba);

        let groups = |n: u32| n.div_ceil(64);
        let mut encoder = self
            .seg
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kare"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("kare"),
                timestamp_writes: None,
            });
            // COVERAGE disindaki cekirdekler cikti elemani basina bir
            // parcacik calistiriyor; kapsam tek is grubunda indirgiyor.
            let run = |pass: &mut wgpu::ComputePass<'_>, idx: usize, threads: u32| {
                pass.set_pipeline(&self.pipelines[idx]);
                pass.set_bind_group(0, &s.bind, &[]);
                let n = if idx == COVERAGE { 1 } else { groups(threads) };
                pass.dispatch_workgroups(n, 1, 1);
            };

            if background != Background::Off {
                run(
                    &mut pass,
                    TO_INPUT,
                    (self.seg.input_size().w * self.seg.input_size().h) as u32,
                );
                self.seg.encode(&mut pass);
                // Maske kapsami: modelin kisiyi bulup bulamadigini arayuze
                // bildirmek icin. Tek is grubu.
                run(&mut pass, COVERAGE, 1);
                if blur_radius > 0 {
                    let small = s.small_w * s.small_h;
                    run(&mut pass, SHRINK, small);
                    run(&mut pass, BLUR_H, small);
                    run(&mut pass, BLUR_V, small);
                }
            }
            run(&mut pass, COMPOSITE, w * h);
            run(&mut pass, PREVIEW, s.preview_w * s.preview_h);
            if s.format == Format::Yuv420 {
                run(&mut pass, YUV_LUMA, w * h / 4);
                run(&mut pass, YUV_CHROMA, (w / 2) * (h / 2) / 4 * 2);
            }
        }
        let full_bytes = s.format.frame_bytes(w, h);
        let full_src = match s.format {
            Format::Yuv420 => s.yuv_word_off as u64 * 4,
            Format::Rgba => 0,
        };
        let preview_bytes = (s.preview_w as u64) * (s.preview_h as u64) * 4;
        encoder.copy_buffer_to_buffer(&s.out, full_src, &s.read_full, 0, full_bytes);
        encoder.copy_buffer_to_buffer(&s.preview, 0, &s.read_preview, 0, preview_bytes);
        let effects_on = background != Background::Off;
        if effects_on {
            encoder.copy_buffer_to_buffer(
                &self.seg.arena,
                self.seg.scratch_offset() as u64 * 4,
                &s.read_coverage,
                0,
                4,
            );
        }
        self.seg.queue.submit(Some(encoder.finish()));

        let full = read_back(&self.seg.device, &s.read_full)?;
        let preview = read_back(&self.seg.device, &s.read_preview)?;
        let coverage = if effects_on {
            let bytes = read_back(&self.seg.device, &s.read_coverage)?;
            Some(f32::from_le_bytes(bytes[..4].try_into().unwrap()))
        } else {
            None
        };

        Ok(Processed {
            frame: full,
            preview,
            preview_size: (s.preview_w, s.preview_h),
            format: s.format,
            coverage,
        })
    }
}

fn read_back(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Result<Vec<u8>, String> {
    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::Maintain::Wait);
    rx.recv()
        .map_err(|_| "kare okunamadi".to_string())?
        .map_err(|e| format!("kare eslenemedi: {e}"))?;
    let data = slice.get_mapped_range().to_vec();
    buffer.unmap();
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn islemci() -> Option<Processor> {
        match Processor::new() {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("ekran karti yok, test atlaniyor: {e}");
                None
            }
        }
    }

    /// YUV420'yi RGB'ye geri cevir - shader'daki donusumun **tersi**, ayri
    /// yazilmis. Boylece testler cekirdegin kendi matematigini tekrarlamiyor.
    /// BT.601 sinirli aralik.
    fn yuv420_to_rgb(yuv: &[u8], w: usize, h: usize) -> Vec<[u8; 3]> {
        let luma = w * h;
        let uv_w = w / 2;
        let (u_off, v_off) = (luma, luma + luma / 4);
        let mut out = Vec::with_capacity(luma);
        for y in 0..h {
            for x in 0..w {
                let yy = yuv[y * w + x] as f32;
                let ui = (y / 2) * uv_w + x / 2;
                let u = yuv[u_off + ui] as f32 - 128.0;
                let v = yuv[v_off + ui] as f32 - 128.0;
                let c = 1.164 * (yy - 16.0);
                let px = [
                    (c + 1.596 * v).clamp(0.0, 255.0) as u8,
                    (c - 0.813 * v - 0.391 * u).clamp(0.0, 255.0) as u8,
                    (c + 2.018 * u).clamp(0.0, 255.0) as u8,
                ];
                out.push(px);
            }
        }
        out
    }

    /// Kare olculeri YUV sartini saglamiyorsa RGBA'ya dusulmeli.
    #[test]
    fn bicim_secimi_olcuye_gore() {
        // Kodlayici kareleri 16'ya hizali; hepsi YUV yolunu kullanmali.
        for (w, h) in [(1280, 720), (720, 1280), (1920, 1080), (640, 480)] {
            assert_eq!(output_format(w, h), Format::Yuv420, "{w}x{h}");
        }
        // Tek genislik/yukseklik: bayt duzeyinde yaris riski, RGBA'ya dus.
        for (w, h) in [(641, 480), (640, 481), (2, 2)] {
            let f = output_format(w, h);
            if w % 4 != 0 || h % 2 != 0 {
                assert_eq!(f, Format::Rgba, "{w}x{h} RGBA'ya dusmeliydi");
            }
        }
    }

    /// YUV donusumu bilgiyi korumali: efekt kapaliyken cikti, girdi karenin
    /// ta kendisi olmali (renk uzayinda gidip gelmenin hatasi kadar sapmayla).
    #[test]
    fn yuv_donusumu_kareyi_koruyor() {
        let Some(mut p) = islemci() else { return };
        let (w, h) = (128u32, 96u32);
        // Yumusak gecisli kare: 4:2:0 alt orneklemesi renk detayini kaybeder,
        // yuksek frekansli desende karsilastirma anlamsiz olurdu.
        let mut frame = vec![255u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                frame[i] = (x * 255 / w) as u8;
                frame[i + 1] = (y * 255 / h) as u8;
                frame[i + 2] = 128;
            }
        }
        let out = p
            .process(
                w,
                h,
                &frame,
                Settings {
                    background: Background::Off,
                    sharpness: 0.0,
                },
            )
            .unwrap();
        assert_eq!(out.format, Format::Yuv420);
        assert_eq!(out.frame.len(), (w * h * 3 / 2) as usize);

        let geri = yuv420_to_rgb(&out.frame, w as usize, h as usize);
        let mut toplam = 0u64;
        for (i, px) in geri.iter().enumerate() {
            for k in 0..3 {
                toplam += (px[k] as i32 - frame[i * 4 + k] as i32).unsigned_abs() as u64;
            }
        }
        let ort = toplam as f64 / (geri.len() * 3) as f64;
        eprintln!("YUV gidip gelme ortalama hatasi: {ort:.2}/255");
        assert!(ort < 3.0, "renk uzayi gidip gelmesi cok bozuyor: {ort}");
    }

    /// Duz renkte, maske disinda kalan pikseller tam olarak o renk olmali.
    #[test]
    fn duz_renk_arka_plani_uygulanir() {
        let Some(mut p) = islemci() else { return };
        let (w, h) = (128u32, 96u32);
        // Rastgele olmayan, kisiye benzemeyen bir kare: maske ~0 cikacak,
        // yani neredeyse her piksel arka plan rengine boyanmali.
        let frame: Vec<u8> = (0..(w * h * 4))
            .map(|i| if i % 4 == 3 { 255 } else { (i % 251) as u8 })
            .collect();
        let out = p
            .process(
                w,
                h,
                &frame,
                Settings {
                    background: Background::Color([1.0, 0.0, 0.0]),
                    sharpness: 1.0,
                },
            )
            .expect("islenmeli");
        assert_eq!(out.frame.len(), (w * h * 3 / 2) as usize);
        let geri = yuv420_to_rgb(&out.frame, w as usize, h as usize);
        let kirmizi = geri
            .iter()
            .filter(|c| c[0] > 200 && c[1] < 60 && c[2] < 60)
            .count();
        assert!(
            kirmizi * 2 > (w * h) as usize,
            "kare cogunlukla arka plan rengi olmaliydi, {kirmizi} piksel"
        );
    }

    /// Kapsam degeri gercekten maskeyi olcuyor mu.
    ///
    /// Demirbas portre (bas-omuz cercevesi) yuksek kapsam vermeli; kisiye
    /// benzemeyen sentetik bir kare neredeyse sifir. Ikisi arasindaki fark
    /// arayuzdeki "kisi bulunamadi" uyarisinin dayandigi sey.
    #[test]
    fn kapsam_kisiyi_ayirt_ediyor() {
        let Some(mut p) = islemci() else { return };
        let ayar = Settings {
            background: Background::Color([0.0, 0.0, 1.0]),
            sharpness: 0.35,
        };

        // Demirbas 256x256 HWC u8; RGBA'ya cevir.
        const GIRDI: &[u8] = include_bytes!("../../tests/fixtures/girdi_u8.bin");
        let mut portre = vec![255u8; 256 * 256 * 4];
        for i in 0..256 * 256 {
            portre[i * 4..i * 4 + 3].copy_from_slice(&GIRDI[i * 3..i * 3 + 3]);
        }
        let kisi = p
            .process(256, 256, &portre, ayar)
            .unwrap()
            .coverage
            .expect("efekt acikken kapsam gelmeli");

        // Gercekci "kisi yok" sahnesi: duz bir duvar. (Yapay gurultu deseni
        // uygun degil - ag onu kismen kisi sanip ~0,19 veriyor, oysa gercek
        // bir kamera oyle bir kare uretmiyor.)
        let duvar = vec![190u8; 256 * 256 * 4];
        let bos = p.process(256, 256, &duvar, ayar).unwrap().coverage.unwrap();

        eprintln!("kapsam: portre {kisi:.4}, duz duvar {bos:.4}");
        assert!(kisi > 0.2, "portrede kapsam dusuk cikti: {kisi}");
        // Arayuzdeki esikle ayni sabiti kullaniyoruz: uyari esigi degisirse
        // bu test de onunla birlikte anlamli kalsin.
        assert!(
            bos < crate::app::COVERAGE_WARN,
            "duvarda kapsam yuksek cikti: {bos}"
        );
    }

    /// Efekt kapaliyken kapsam raporlanmamali - ag hic kosmuyor.
    #[test]
    fn kapali_efektte_kapsam_yok() {
        let Some(mut p) = islemci() else { return };
        let kare = vec![128u8; 64 * 64 * 4];
        let out = p
            .process(
                64,
                64,
                &kare,
                Settings {
                    background: Background::Off,
                    sharpness: 0.0,
                },
            )
            .unwrap();
        assert!(out.coverage.is_none());
    }

    /// Efekt kapaliyken kare degismeden gecmeli.
    #[test]
    fn kapaliyken_kare_degismez() {
        let Some(mut p) = islemci() else { return };
        let (w, h) = (64u32, 64u32);
        // Duz gri: 4:2:0 alt ornekleme kayip vermez, karsilastirma temiz olur.
        let frame: Vec<u8> = (0..(w * h * 4))
            .map(|i| if i % 4 == 3 { 255 } else { 137 })
            .collect();
        let out = p
            .process(
                w,
                h,
                &frame,
                Settings {
                    background: Background::Off,
                    sharpness: 0.0,
                },
            )
            .unwrap();
        // Efekt kapali: kare degismeden gecmeli. Cikti YUV oldugu icin geri
        // cevirip karsilastiriyoruz; duz gri surekli oldugundan alt
        // ornekleme kayip vermiyor.
        let geri = yuv420_to_rgb(&out.frame, w as usize, h as usize);
        for (i, px) in geri.iter().enumerate() {
            for k in 0..3 {
                let fark = (px[k] as i32 - frame[i * 4 + k] as i32).abs();
                assert!(fark <= 3, "piksel {i} kanal {k}: {fark} fark");
            }
        }
    }

    /// Gozle denetim icin elle calistirilan kanca. Gercek bir kareyi RGBA
    /// ham olarak alip her efekti ayri dosyaya yaziyor:
    ///
    /// ```text
    /// OWNCAM_KARE=/yol/kare.rgba OWNCAM_EN=1280 OWNCAM_BOY=720 \
    ///   cargo test --release efekt_ornekleri -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "elle calistirilir; gercek kare dosyasi gerektirir"]
    fn efekt_ornekleri() {
        let Ok(path) = std::env::var("OWNCAM_KARE") else {
            eprintln!("OWNCAM_KARE verilmedi");
            return;
        };
        let w: u32 = std::env::var("OWNCAM_EN").unwrap().parse().unwrap();
        let h: u32 = std::env::var("OWNCAM_BOY").unwrap().parse().unwrap();
        let frame = std::fs::read(&path).unwrap();
        let mut p = islemci().expect("ekran karti gerekli");

        if let Ok(bg) = std::env::var("OWNCAM_ARKAPLAN") {
            let bw: u32 = std::env::var("OWNCAM_ARKAPLAN_EN").unwrap().parse().unwrap();
            let bh: u32 = std::env::var("OWNCAM_ARKAPLAN_BOY").unwrap().parse().unwrap();
            p.set_background(bw, bh, &std::fs::read(bg).unwrap()).unwrap();
        }

        for (ad, arka) in [
            ("kapali", Background::Off),
            ("bulanik", Background::Blur(0.6)),
            ("renk", Background::Color([0.05, 0.35, 0.6])),
            ("foto", Background::Image),
        ] {
            let t0 = std::time::Instant::now();
            let out = p
                .process(w, h, &frame, Settings { background: arka, sharpness: 0.35 })
                .unwrap();
            eprintln!("{ad}: {:?}", t0.elapsed());
            std::fs::write(format!("/tmp/owncam_{ad}.rgba"), &out.frame).unwrap();
            std::fs::write(format!("/tmp/owncam_{ad}_onizleme.rgba"), &out.preview).unwrap();
            eprintln!("  onizleme {:?}", out.preview_size);
        }
    }

    /// Onizleme alani butcede kalmali ve en-boy oranini korumali.
    #[test]
    fn onizleme_olcegi_makul() {
        let Some(mut p) = islemci() else { return };
        for (w, h) in [(1280u32, 720u32), (720, 1280)] {
            let frame = vec![255u8; (w * h * 4) as usize];
            let out = p
                .process(w, h, &frame, Settings::default())
                .expect("islenmeli");
            let (pw, ph) = out.preview_size;
            assert!(pw * ph <= PREVIEW_PIXELS * 2, "onizleme cok buyuk {pw}x{ph}");
            let oran = (pw as f64 / ph as f64) / (w as f64 / h as f64);
            assert!((oran - 1.0).abs() < 0.05, "en-boy bozuldu: {pw}x{ph}");
            assert_eq!(out.preview.len(), (pw * ph * 4) as usize);
        }
    }
}
