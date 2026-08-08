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
    _pad: [f32; 3],
}

const PIPELINE_NAMES: [&str; 6] = [
    "to_network_input",
    "shrink",
    "blur_h",
    "blur_v",
    "composite",
    "preview_shrink",
];
const TO_INPUT: usize = 0;
const SHRINK: usize = 1;
const BLUR_H: usize = 2;
const BLUR_V: usize = 3;
const COMPOSITE: usize = 4;
const PREVIEW: usize = 5;

/// Onizleme piksel butcesi. Alana gore olcekleniyor, genislige gore degil:
/// dik karede genislige gore olceklemek uc kat trafik uretiyordu.
const PREVIEW_PIXELS: u32 = 640 * 360;

struct Sized {
    w: u32,
    h: u32,
    small_w: u32,
    small_h: u32,
    preview_w: u32,
    preview_h: u32,
    frame: wgpu::Buffer,
    out: wgpu::Buffer,
    preview: wgpu::Buffer,
    read_full: wgpu::Buffer,
    read_preview: wgpu::Buffer,
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
    pub rgba: Vec<u8>,
    pub preview: Vec<u8>,
    pub preview_size: (u32, u32),
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
        let out = buf(
            "kompozit",
            px * 4,
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
            px * 4,
            wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        );
        let read_preview = buf(
            "okuma-onizleme",
            (preview_w as u64) * (preview_h as u64) * 4,
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
            small_w,
            small_h,
            preview_w,
            preview_h,
            frame,
            out,
            preview,
            read_full,
            read_preview,
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
            net_w: super::gpu::INPUT.w as u32,
            net_h: super::gpu::INPUT.h as u32,
            bg_w: self.bg_size.0,
            bg_h: self.bg_size.1,
            preview_w: s.preview_w,
            preview_h: s.preview_h,
            blur_radius,
            color,
            sharpness: settings.sharpness,
            _pad: [0.0; 3],
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
            let run = |pass: &mut wgpu::ComputePass<'_>, idx: usize, threads: u32| {
                pass.set_pipeline(&self.pipelines[idx]);
                pass.set_bind_group(0, &s.bind, &[]);
                pass.dispatch_workgroups(groups(threads), 1, 1);
            };

            if background != Background::Off {
                run(
                    &mut pass,
                    TO_INPUT,
                    (super::gpu::INPUT.w * super::gpu::INPUT.h) as u32,
                );
                self.seg.encode(&mut pass);
                if blur_radius > 0 {
                    let small = s.small_w * s.small_h;
                    run(&mut pass, SHRINK, small);
                    run(&mut pass, BLUR_H, small);
                    run(&mut pass, BLUR_V, small);
                }
            }
            run(&mut pass, COMPOSITE, w * h);
            run(&mut pass, PREVIEW, s.preview_w * s.preview_h);
        }
        let full_bytes = (w as u64) * (h as u64) * 4;
        let preview_bytes = (s.preview_w as u64) * (s.preview_h as u64) * 4;
        encoder.copy_buffer_to_buffer(&s.out, 0, &s.read_full, 0, full_bytes);
        encoder.copy_buffer_to_buffer(&s.preview, 0, &s.read_preview, 0, preview_bytes);
        self.seg.queue.submit(Some(encoder.finish()));

        let full = read_back(&self.seg.device, &s.read_full)?;
        let preview = read_back(&self.seg.device, &s.read_preview)?;

        Ok(Processed {
            rgba: full,
            preview,
            preview_size: (s.preview_w, s.preview_h),
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
        assert_eq!(out.rgba.len(), (w * h * 4) as usize);
        let kirmizi = out
            .rgba
            .chunks_exact(4)
            .filter(|c| c[0] == 255 && c[1] == 0 && c[2] == 0)
            .count();
        assert!(
            kirmizi * 2 > (w * h) as usize,
            "kare cogunlukla arka plan rengi olmaliydi, {kirmizi} piksel"
        );
    }

    /// Efekt kapaliyken kare degismeden gecmeli.
    #[test]
    fn kapaliyken_kare_degismez() {
        let Some(mut p) = islemci() else { return };
        let (w, h) = (64u32, 64u32);
        let frame: Vec<u8> = (0..(w * h * 4))
            .map(|i| if i % 4 == 3 { 255 } else { (i % 199) as u8 })
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
        assert_eq!(out.rgba, frame);
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
            std::fs::write(format!("/tmp/owncam_{ad}.rgba"), &out.rgba).unwrap();
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
