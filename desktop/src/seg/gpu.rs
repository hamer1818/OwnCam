//! Segmentasyon aginin GPU calisma zamani (wgpu, penceresiz).
//!
//! Neden kendi calisma zamanimiz: hazir cikarim kutuphaneleri ya islemcide
//! kaliyor ya da devasa. Olctuk - `tract` bu modeli **32 ms**'de cozuyor
//! (30 fps butcesi 33 ms) ve ikiliye 35 MB bindiriyor. Kendi cekirdeklerimiz
//! ~106 bin agirlikla calisiyor, modelle birlikte 460 KB yer kapliyor ve
//! Vulkan/GL uzerinden her ekran kartinda calisiyor - NVIDIA'ya bagli degil.
//!
//! Cihaz **penceresiz** aciliyor: isleme arayuzun cizim dongusunden bagimsiz
//! bir is parcaciginda yurusun diye. Pencere kucultulunce sanal kameranin
//! durmasi boyle onleniyor.

use std::borrow::Cow;

use super::onnx;
use super::plan::{self, Kind, Plan, Shape};

/// Agin bekledigi girdi olcusu (modelde sabit).
pub const INPUT: Shape = Shape {
    c: 3,
    h: 256,
    w: 256,
};

const MODEL: &[u8] = include_bytes!("../../assets/selfie_segmentation.onnx");

/// `seg.wgsl` icindeki butun `@workgroup_size` degerleriyle ayni olmali.
const WORKGROUP: u32 = 64;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    a: u32,
    b: u32,
    out: u32,
    weight: u32,
    bias: u32,
    in_c: u32,
    in_h: u32,
    in_w: u32,
    out_c: u32,
    out_h: u32,
    out_w: u32,
    kh: u32,
    kw: u32,
    stride: u32,
    pad_t: u32,
    pad_l: u32,
    groups: u32,
    _pad: [u32; 3],
}

impl From<&plan::Step> for Params {
    fn from(s: &plan::Step) -> Self {
        Params {
            a: s.a,
            b: s.b,
            out: s.out,
            weight: s.weight,
            bias: s.bias,
            in_c: s.in_shape.c as u32,
            in_h: s.in_shape.h as u32,
            in_w: s.in_shape.w as u32,
            out_c: s.out_shape.c as u32,
            out_h: s.out_shape.h as u32,
            out_w: s.out_shape.w as u32,
            kh: s.kh,
            kw: s.kw,
            stride: s.stride,
            pad_t: s.pad_t,
            pad_l: s.pad_l,
            groups: s.group,
            _pad: [0; 3],
        }
    }
}

fn entry_point(kind: Kind) -> &'static str {
    match kind {
        Kind::Conv => "conv",
        Kind::ConvTranspose => "conv_transpose",
        Kind::Resize => "resize",
        Kind::ReduceMean => "reduce_mean",
        Kind::Relu => "relu",
        Kind::HardSwish => "hard_swish",
        Kind::Sigmoid => "sigmoid",
        Kind::Add => "add",
        Kind::MulChannel => "mul_channel",
    }
}

/// Bir adimin kac **is grubu** gerektirdigi.
///
/// Cogu cekirdek cikti elemani basina bir parcacik calistiriyor. Kuresel
/// ortalama farkli: is grubu basina bir kanal alip 64 parcacikla indirgiyor,
/// bu yuzden grup sayisi dogrudan kanal sayisi.
fn workgroups(step: &plan::Step) -> u32 {
    match step.kind {
        Kind::ReduceMean => step.out_shape.c as u32,
        _ => (step.out_shape.len() as u32).div_ceil(WORKGROUP),
    }
}

pub struct Segmenter {
    pub(super) device: wgpu::Device,
    pub(super) queue: wgpu::Queue,
    pub(super) arena: wgpu::Buffer,
    /// Yalnizca `mask()` dogrulama yolunda kullaniliyor.
    #[allow(dead_code)]
    readback: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    pipelines: Vec<wgpu::ComputePipeline>,
    /// Adim -> (boru hatti indeksi, uniform dinamik kayma, is grubu sayisi)
    dispatch: Vec<(usize, u32, u32)>,
    plan: Plan,
    pub adapter_name: String,
}

impl Segmenter {
    pub fn new() -> Result<Self, String> {
        let graph = onnx::parse(MODEL)?;
        let plan = plan::build(&graph, INPUT)?;

        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))
        .ok_or("uygun bir ekran karti bulunamadi")?;
        let info = adapter.get_info();
        let adapter_name = format!("{} ({:?})", info.name, info.backend);

        let arena_bytes = (plan.arena_len * 4) as u64;
        let weight_bytes = (plan.weights.len() * 4) as u64;
        let mut limits = wgpu::Limits::downlevel_defaults();
        limits.max_storage_buffer_binding_size = arena_bytes.max(weight_bytes).max(
            wgpu::Limits::downlevel_defaults().max_storage_buffer_binding_size as u64,
        ) as u32;
        limits.max_buffer_size = limits.max_buffer_size.max(arena_bytes);
        // Kompozit gecisi 7 depolama tamponu bagliyor (arena, kare, cikti,
        // iki bulanik ara tamponu, arka plan, onizleme). `downlevel_defaults`
        // 4 veriyor; masaustu OpenGL 4.3 en az 8 garanti ediyor, Vulkan cok
        // daha fazlasini.
        limits.max_storage_buffers_per_shader_stage =
            limits.max_storage_buffers_per_shader_stage.max(8);

        let adapter_limits = adapter.limits();
        let eksik = |ad: &str, istenen: u64, var: u64| -> Option<String> {
            (istenen > var).then(|| format!("{ad}: {istenen} gerekiyor, ekran karti {var} veriyor"))
        };
        for sorun in [
            eksik(
                "tampon boyutu",
                limits.max_storage_buffer_binding_size as u64,
                adapter_limits.max_storage_buffer_binding_size as u64,
            ),
            eksik(
                "depolama tamponu sayisi",
                limits.max_storage_buffers_per_shader_stage as u64,
                adapter_limits.max_storage_buffers_per_shader_stage as u64,
            ),
        ]
        .into_iter()
        .flatten()
        {
            return Err(sorun);
        }

        let (device, queue) = pollster::block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("owncam-segmentasyon"),
                required_features: wgpu::Features::empty(),
                required_limits: limits,
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))
        .map_err(|e| format!("ekran karti acilamadi: {e}"))?;

        let align = device.limits().min_uniform_buffer_offset_alignment as u64;

        let arena = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("arena"),
            size: arena_bytes,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        let weights = create_init_buffer(
            &device,
            "agirliklar",
            bytemuck::cast_slice(&plan.weights),
            wgpu::BufferUsages::STORAGE,
        );

        // Butun adimlarin parametreleri tek uniform tamponda; her adim
        // dinamik kayma ile kendi bloguna bakiyor.
        let stride = align.max(std::mem::size_of::<Params>() as u64);
        let mut params_bytes = vec![0u8; stride as usize * plan.steps.len()];
        let mut dispatch = Vec::with_capacity(plan.steps.len());
        let mut kinds: Vec<Kind> = Vec::new();
        for (i, step) in plan.steps.iter().enumerate() {
            let p = Params::from(step);
            let at = i * stride as usize;
            params_bytes[at..at + std::mem::size_of::<Params>()]
                .copy_from_slice(bytemuck::bytes_of(&p));
            if !kinds.contains(&step.kind) {
                kinds.push(step.kind);
            }
            let pipeline = kinds.iter().position(|k| *k == step.kind).unwrap();
            dispatch.push((pipeline, at as u32, workgroups(step)));
        }
        let params = create_init_buffer(
            &device,
            "parametreler",
            &params_bytes,
            wgpu::BufferUsages::UNIFORM,
        );

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("segmentasyon"),
            entries: &[
                storage_entry(0, false),
                storage_entry(1, true),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(
                            std::mem::size_of::<Params>() as u64
                        ),
                    },
                    count: None,
                },
            ],
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("segmentasyon"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: arena.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: weights.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &params,
                        offset: 0,
                        size: wgpu::BufferSize::new(std::mem::size_of::<Params>() as u64),
                    }),
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("seg.wgsl"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!("seg.wgsl"))),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let pipelines = kinds
            .iter()
            .map(|k| {
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(entry_point(*k)),
                    layout: Some(&pipeline_layout),
                    module: &shader,
                    entry_point: Some(entry_point(*k)),
                    compilation_options: Default::default(),
                    cache: None,
                })
            })
            .collect();

        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("maske-okuma"),
            size: (plan.output_shape.len() * 4) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Ok(Self {
            device,
            queue,
            arena,
            readback,
            bind_group,
            pipelines,
            dispatch,
            plan,
            adapter_name,
        })
    }

    /// Olcum ve teshis icin; uretim yolunda cagrilmiyor.
    #[allow(dead_code)]
    pub fn arena_bytes(&self) -> usize {
        self.plan.arena_len * 4
    }

    #[allow(dead_code)]
    pub fn steps(&self) -> usize {
        self.plan.steps.len()
    }

    /// Arena icinde agin girdisinin ve uretilen maskenin yeri. Kompozit
    /// gecisi ayni tampona baktigi icin ara kopya gerekmiyor.
    pub fn input_offset(&self) -> u32 {
        self.plan.input_off
    }

    pub fn mask_offset(&self) -> u32 {
        self.plan.output_off
    }

    /// Arenanin sonundaki calisma alani; kompozit maske kapsamini buraya yaziyor.
    pub fn scratch_offset(&self) -> u32 {
        self.plan.scratch_off
    }

    pub fn mask_shape(&self) -> Shape {
        self.plan.output_shape
    }

    /// Agin butun adimlarini verilen hesap gecisine yaz. Kompozitle tek
    /// komut tamponunu paylasmak icin ayri duruyor: kare basina tek gonderim.
    pub fn encode(&self, pass: &mut wgpu::ComputePass<'_>) {
        let mut current = usize::MAX;
        for (pipeline, offset, groups) in &self.dispatch {
            if *pipeline != current {
                pass.set_pipeline(&self.pipelines[*pipeline]);
                current = *pipeline;
            }
            pass.set_bind_group(0, &self.bind_group, &[*offset]);
            pass.dispatch_workgroups(*groups, 1, 1);
        }
    }

    /// Girdi NCHW f32, 3x256x256, 0..1. Cikti 256x256 maske, 0..1.
    ///
    /// Tek kare, tek gonderim. Uretimde kullanilmiyor - orada kompozitle ayni
    /// komut tamponunu paylasan `encode()` yolu var. Bu yol bagimsiz referansa
    /// karsi sayisal dogrulama icin duruyor (bkz. `tests/fixtures/`).
    #[allow(dead_code)]
    pub fn mask(&self, input: &[f32]) -> Result<Vec<f32>, String> {
        if input.len() != INPUT.len() {
            return Err(format!(
                "girdi {} eleman olmali, {} geldi",
                INPUT.len(),
                input.len()
            ));
        }
        self.queue.write_buffer(
            &self.arena,
            self.plan.input_off as u64 * 4,
            bytemuck::cast_slice(input),
        );

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("segmentasyon"),
                timestamp_writes: None,
            });
            self.encode(&mut pass);
        }
        let out_bytes = (self.plan.output_shape.len() * 4) as u64;
        encoder.copy_buffer_to_buffer(
            &self.arena,
            self.plan.output_off as u64 * 4,
            &self.readback,
            0,
            out_bytes,
        );
        self.queue.submit(Some(encoder.finish()));

        let slice = self.readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::Maintain::Wait);
        rx.recv()
            .map_err(|_| "maske okunamadi".to_string())?
            .map_err(|e| format!("maske eslenemedi: {e}"))?;
        let data = slice.get_mapped_range();
        let out: Vec<f32> = bytemuck::cast_slice(&data).to_vec();
        drop(data);
        self.readback.unmap();
        Ok(out)
    }
}

pub(super) fn storage_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

pub(super) fn create_init_buffer(
    device: &wgpu::Device,
    label: &str,
    data: &[u8],
    usage: wgpu::BufferUsages,
) -> wgpu::Buffer {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: data.len() as u64,
        usage,
        mapped_at_creation: true,
    });
    buffer
        .slice(..)
        .get_mapped_range_mut()
        .copy_from_slice(data);
    buffer.unmap();
    buffer
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Demirbaslar `tract` (ONNX referans calisma zamani) ile uretildi:
    /// gercek bir portre 256x256'ya olceklenip agdan gecirildi. Boylece
    /// GPU cekirdeklerimizi bagimsiz bir uygulamaya karsi olcuyoruz.
    const GIRDI: &[u8] = include_bytes!("../../tests/fixtures/girdi_u8.bin");
    const MASKE: &[u8] = include_bytes!("../../tests/fixtures/maske_u8.bin");

    fn nchw() -> Vec<f32> {
        let mut out = vec![0.0f32; INPUT.len()];
        for y in 0..INPUT.h {
            for x in 0..INPUT.w {
                for c in 0..INPUT.c {
                    out[(c * INPUT.h + y) * INPUT.w + x] =
                        GIRDI[(y * INPUT.w + x) * 3 + c] as f32 / 255.0;
                }
            }
        }
        out
    }

    /// Gecikme butcesi: 30 fps icin kare basina 33 ms. Islemcide `tract`
    /// bu modeli 32 ms'de cozüyordu; olcum buradaki kazanci gosteriyor.
    #[test]
    fn cikarim_sure_butcesinde() {
        let seg = match Segmenter::new() {
            Ok(s) => s,
            Err(e) => {
                eprintln!("ekran karti yok, olcum atlaniyor: {e}");
                return;
            }
        };
        let input = nchw();
        seg.mask(&input).unwrap();                     // isinma
        let n = 50;
        let t0 = std::time::Instant::now();
        for _ in 0..n {
            seg.mask(&input).unwrap();
        }
        let per = t0.elapsed() / n;
        eprintln!(
            "{} | {} adim, arena {:.1} MB | kare basina {:?}",
            seg.adapter_name,
            seg.steps(),
            seg.arena_bytes() as f64 / 1e6,
            per
        );
        assert!(
            per < std::time::Duration::from_millis(33),
            "cikarim 30 fps butcesini asiyor: {per:?}"
        );
    }

    /// Basit, tohumlu gurultu ureteci - `rand` bagimliligi eklemeye degmez.
    fn xorshift(state: &mut u32) -> f32 {
        *state ^= *state << 13;
        *state ^= *state >> 17;
        *state ^= *state << 5;
        // 0..1 araligina indir
        (*state >> 8) as f32 / (1u32 << 24) as f32
    }

    /// **Olcum**: maske ardisik karelerde ne kadar titriyor?
    ///
    /// Gercek bir video dizisi yerine ayni kareye kare basina bagimsiz
    /// gurultu ekleniyor. Titremenin fiziksel sebebi zaten bu: sabit sahnede
    /// bile algilayici gurultusu her kareyi biraz degistiriyor ve ag her
    /// kareyi bagimsiz cozuyor. Gercek bir dizide buna hareket de eklenir,
    /// yani bu **alt sinir**.
    ///
    /// ```text
    /// cargo test --release maske_titremesi -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "olcum; elle calistirilir"]
    fn maske_titremesi() {
        let Ok(seg) = Segmenter::new() else {
            eprintln!("ekran karti yok");
            return;
        };
        let base = nchw();
        // Telefon kamerasinda iyi isikta tipik gurultu ~1-3 seviye (255 uzerinden).
        for sigma_level in [1.0f32, 3.0] {
            let sigma = sigma_level / 255.0;
            let mut state = 0x2545_F491u32;
            let mut previous: Option<Vec<f32>> = None;
            let (mut sum_delta, mut sum_flip, mut frames) = (0.0f64, 0.0f64, 0u32);

            for _ in 0..20 {
                let noisy: Vec<f32> = base
                    .iter()
                    .map(|v| (v + (xorshift(&mut state) - 0.5) * 2.0 * sigma).clamp(0.0, 1.0))
                    .collect();
                let mask = seg.mask(&noisy).expect("maske");
                if let Some(prev) = &previous {
                    let mut delta = 0.0f64;
                    let mut flip = 0u32;
                    for (a, b) in mask.iter().zip(prev.iter()) {
                        delta += (a - b).abs() as f64;
                        if (*a > 0.5) != (*b > 0.5) {
                            flip += 1;
                        }
                    }
                    sum_delta += delta / mask.len() as f64;
                    sum_flip += flip as f64 / mask.len() as f64;
                    frames += 1;
                }
                previous = Some(mask);
            }
            eprintln!(
                "gurultu +-{sigma_level:.0}/255 -> ardisik maske farki ort {:.5}, \
                 esik atlayan piksel orani {:.5}",
                sum_delta / frames as f64,
                sum_flip / frames as f64,
            );
        }
    }

    #[test]
    fn maske_referansla_ortusuyor() {
        let seg = match Segmenter::new() {
            Ok(s) => s,
            // Ekran karti olmayan ortamda (CI) sessizce atla.
            Err(e) => {
                eprintln!("ekran karti yok, test atlaniyor: {e}");
                return;
            }
        };
        let mask = seg.mask(&nchw()).expect("maske uretilmeli");
        assert_eq!(mask.len(), 256 * 256);

        let mut worst = 0.0f32;
        let mut sum = 0.0f64;
        for (got, want) in mask.iter().zip(MASKE.iter()) {
            let want = *want as f32 / 255.0;
            let d = (got - want).abs();
            worst = worst.max(d);
            sum += d as f64;
        }
        let mean = sum / mask.len() as f64;
        eprintln!("en buyuk fark {worst:.4}, ortalama {mean:.5}");
        assert!(mean < 0.01, "ortalama fark cok buyuk: {mean}");
        assert!(worst < 0.10, "en buyuk fark cok buyuk: {worst}");
    }
}
