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
use super::plan::{self, BinOp, Kind, Plan, Shape};

/// Agin girdi olcusu.
///
/// Model 256x256 ile egitildi ama tamamen evrisimli: butun `Resize`'lar tam
/// iki kat, kuresel ortalama her olcude calisiyor. Plan hedef boyutlari
/// girdiye gore olcekledigi icin ag baska cozunurluklerde de kosabiliyor.
/// Daha buyuk girdi = daha yuksek cozunurluklu maske = daha az basamakli
/// kenar; bedeli karesel.
pub fn input_shape() -> Shape {
    let n = std::env::var("OWNCAM_SEG_BOYUT")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        // 32'nin kati olmali: ag girdiyi bes kez yariya bolup geri buyutuyor.
        .map(|v| (v / 32).clamp(4, 32) * 32)
        .unwrap_or(INPUT.w);
    Shape { c: 3, h: n, w: n }
}

/// Geriye donuk uyumluluk icin varsayilan olcu.
pub const INPUT: Shape = Shape {
    c: 3,
    h: 256,
    w: 256,
};

const MODEL: &[u8] = include_bytes!("../../assets/selfie_segmentation.onnx");

/// Arka plani ayiran ag.
#[derive(Debug, Clone, PartialEq)]
pub enum Model {
    /// Gomulu MediaPipe Selfie Segmentation. Kare girdi, kaba ikili maske,
    /// kareler arasinda bagimsiz - yani biraz titriyor. 1 ms, 460 KB.
    Hizli,
    /// Robust Video Matting. Tam cozunurlukte **alfa** uretiyor (maske degil,
    /// gercek matting: sac telleri ve yari saydam kenarlar cikiyor) ve gizli
    /// durumla kareler arasinda tutarli kaliyor.
    ///
    /// Agirliklar depoda **degil**: RVM GPL-3.0, OwnCam MIT. Dosyayi kullanici
    /// indirip yolunu veriyor; kod agirliklari dagitmiyor.
    Kaliteli {
        yol: std::path::PathBuf,
        /// Ag govdesinin kareyi kucultme orani. Kucuk deger hizli, buyuk deger
        /// ince ayrinti. RVM'nin onerisi 720p icin ~0.375, 1080p icin 0.25.
        oran: f32,
    },
}

impl Model {
    /// Ortamdan sec: `OWNCAM_MODEL` bir ONNX yolu verirse kaliteli ag,
    /// yoksa gomulu olan.
    pub fn from_env() -> Self {
        match std::env::var("OWNCAM_MODEL") {
            Ok(yol) if !yol.is_empty() => Model::Kaliteli {
                yol: yol.into(),
                oran: std::env::var("OWNCAM_ORAN")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0.375),
            },
            _ => Model::Hizli,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Model::Hizli => "hizli",
            Model::Kaliteli { .. } => "kaliteli",
        }
    }
}

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
    dilation: u32,
    b_c: u32,
    b_h: u32,
    b_w: u32,
    alpha: f32,
    beta: f32,
    _pad: [u32; 2],
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
            dilation: s.dilation,
            b_c: s.b_shape.c as u32,
            b_h: s.b_shape.h as u32,
            b_w: s.b_shape.w as u32,
            alpha: s.alpha,
            beta: s.beta,
            _pad: [0; 2],
        }
    }
}

fn entry_point(kind: Kind) -> &'static str {
    match kind {
        Kind::Conv => "conv",
        Kind::ConvTranspose => "conv_transpose",
        Kind::Resize => "resize",
        Kind::ReduceMean => "reduce_mean",
        Kind::ReduceChannel => "reduce_channel",
        Kind::AvgPool => "avg_pool",
        Kind::Relu => "relu",
        Kind::HardSwish => "hard_swish",
        Kind::Sigmoid => "sigmoid",
        // `tanh` ve `clip` WGSL'de yerlesik ad; giris noktalari ayri isimde.
        Kind::Tanh => "tanh_op",
        Kind::HardSigmoid => "hard_sigmoid",
        Kind::Clip => "clip_op",
        Kind::Binary(BinOp::Add) => "bin_add",
        Kind::Binary(BinOp::Sub) => "bin_sub",
        Kind::Binary(BinOp::Mul) => "bin_mul",
        Kind::Binary(BinOp::Div) => "bin_div",
        Kind::Copy => "copy_op",
        Kind::Crop => "crop",
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
    /// Belirli bir girdi olcusuyle kur.
    ///
    /// Referans testi bunu 256'ya sabitliyor: demirbas o olcude uretildi ve
    /// ortam degiskeni testin dogruladigi seyi degistirmemeli.
    pub fn with_size(shape: Shape) -> Result<Self, String> {
        let graph = onnx::parse(MODEL)?;
        Self::from_plan(plan::build(&graph, shape)?)
    }

    /// Secilen agi bu kare olcusu icin kur.
    ///
    /// Hizli ag kare bir girdi kullaniyor ve kareyi ona olcekliyoruz; RVM
    /// kareyi **tam cozunurlukte** aliyor ve kucultmeyi kendi icinde yapiyor,
    /// bu yuzden plan kare olcusune gore kuruluyor.
    pub fn for_frame(model: &Model, frame: (u32, u32)) -> Result<Self, String> {
        match model {
            Model::Hizli => Self::with_size(input_shape()),
            Model::Kaliteli { yol, oran } => {
                let bytes = std::fs::read(yol)
                    .map_err(|e| format!("model okunamadi ({}): {e}", yol.display()))?;
                let mut graph = onnx::parse(&bytes)?;
                if graph.inputs.iter().any(|n| n == "downsample_ratio") {
                    graph.set_input_constant("downsample_ratio", *oran);
                }
                let shape = Shape {
                    c: 3,
                    h: frame.1 as usize,
                    w: frame.0 as usize,
                };
                Self::from_plan(plan::build(&graph, shape)?)
            }
        }
    }

    /// Hazir bir plandan kur. Model dosyasi disaridan geldiginde (RVM gibi)
    /// bu yol kullaniliyor; gomulu model de ayni yerden geciyor.
    pub fn from_plan(plan: Plan) -> Result<Self, String> {
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
        let sorun = [
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
        .next();
        if let Some(sorun) = sorun {
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

        // Graftaki sabitler (normalizasyonun ortalamasi gibi) arenanin
        // kendilerine ayrilan yerine bir kez yaziliyor. Gizli durumlar
        // sifirla basliyor - wgpu tamponlari sifirliyor ama yayin yeniden
        // baslarsa da sifirlanmalari gerektigi icin acikca yaziyoruz.
        for (off, values) in &plan.constants {
            queue.write_buffer(&arena, *off as u64 * 4, bytemuck::cast_slice(values));
        }
        for state in &plan.states {
            queue.write_buffer(
                &arena,
                state.offset as u64 * 4,
                bytemuck::cast_slice(&vec![0.0f32; state.shape.len()]),
            );
        }

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

    /// Agin **on plan** tahmini (RVM'nin `fgr` ciktisi), varsa.
    ///
    /// Maskeleme ile matting'in farki burasi: yari saydam bir kenarda kamera
    /// pikselinde eski arka planin rengi de var. `fgr` o rengi ayiklanmis
    /// halini veriyor, yani sac kenarinda eski oda rengi sizmiyor.
    pub fn foreground_offset(&self) -> Option<u32> {
        self.plan.output("fgr").map(|(off, _)| off)
    }

    /// Arenanin sonundaki calisma alani; kompozit maske kapsamini buraya yaziyor.
    pub fn scratch_offset(&self) -> u32 {
        self.plan.scratch_off
    }

    pub fn mask_shape(&self) -> Shape {
        self.plan.output_shape
    }

    /// Agin bu ornekte kullandigi girdi olcusu.
    pub fn input_size(&self) -> Shape {
        self.plan.input_shape
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
        if input.len() != self.plan.input_shape.len() {
            return Err(format!(
                "girdi {} eleman olmali, {} geldi",
                self.plan.input_shape.len(),
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
        let seg = match Segmenter::with_size(input_shape()) {
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

    /// Kaynaktan iki dogrusal ornekleme (u,v 0..1).
    fn ornekle(rgba: &[u8], w: usize, h: usize, u: f32, v: f32) -> [f32; 3] {
        let fx = (u * w as f32 - 0.5).clamp(0.0, w as f32 - 1.0);
        let fy = (v * h as f32 - 0.5).clamp(0.0, h as f32 - 1.0);
        let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
        let (tx, ty) = (fx - fx.floor(), fy - fy.floor());
        let at = |x: usize, y: usize, c: usize| rgba[(y * w + x) * 4 + c] as f32 / 255.0;
        let mut out = [0.0f32; 3];
        for (c, o) in out.iter_mut().enumerate() {
            let a = at(x0, y0, c) * (1.0 - tx) + at(x1, y0, c) * tx;
            let b = at(x0, y1, c) * (1.0 - tx) + at(x1, y1, c) * tx;
            *o = a * (1.0 - ty) + b * ty;
        }
        out
    }

    /// **Olcum**: agin girdisi nasil hazirlanmali?
    ///
    /// Kare 16:9, agin girdisi 1:1. Su an kare **eziliyor**, yani en-boy
    /// orani 1,78 kat bozuluyor. Ag duzgun oranli selfie'lerle egitildigi
    /// icin bu zarar veriyor olabilir. Uc secenek karsilastiriliyor:
    /// ezme (mevcut), ortadan kare kirpma, ve orani koruyup kutulama.
    ///
    /// Olcut **kararsizlik**: maskenin 0,2-0,8 arasinda kalan piksel orani.
    /// Iyi bir maske iki kutuplu; ortada kalan piksel agin emin olamadigi
    /// yerdir. Yani kucuk deger daha iyi.
    ///
    /// ```text
    /// OWNCAM_KARE=/tmp/kare.rgba OWNCAM_EN=1280 OWNCAM_BOY=720 \
    ///   cargo test --release girdi_hazirlama -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "olcum; gercek kare dosyasi gerektirir"]
    fn girdi_hazirlama() {
        let Ok(seg) = Segmenter::with_size(input_shape()) else {
            eprintln!("ekran karti yok");
            return;
        };
        let Ok(path) = std::env::var("OWNCAM_KARE") else {
            eprintln!("OWNCAM_KARE verilmedi");
            return;
        };
        let w: usize = std::env::var("OWNCAM_EN").unwrap().parse().unwrap();
        let h: usize = std::env::var("OWNCAM_BOY").unwrap().parse().unwrap();
        let rgba = std::fs::read(&path).unwrap();
        assert_eq!(rgba.len(), w * h * 4, "kare boyutu tutmuyor");

        let n = seg.input_size().w;
        let aspect = w as f32 / h as f32;

        for mod_ad in ["ezme", "kirpma", "kutulama"] {
            let mut input = vec![0.0f32; seg.input_size().len()];
            for y in 0..n {
                for x in 0..n {
                    // Hedef pikselin kaynaktaki (u,v) karsiligi
                    let (mut u, mut v) = ((x as f32 + 0.5) / n as f32, (y as f32 + 0.5) / n as f32);
                    let mut disarida = false;
                    match mod_ad {
                        // Oran bozuluyor; butun kare kullaniliyor.
                        "ezme" => {}
                        // Oran dogru; genis kenardan kirpiliyor.
                        "kirpma" => {
                            if aspect >= 1.0 {
                                u = 0.5 + (u - 0.5) / aspect;
                            } else {
                                v = 0.5 + (v - 0.5) * aspect;
                            }
                        }
                        // Oran dogru; bosluk gri kaliyor.
                        _ => {
                            if aspect >= 1.0 {
                                v = 0.5 + (v - 0.5) * aspect;
                            } else {
                                u = 0.5 + (u - 0.5) / aspect;
                            }
                            disarida = !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v);
                        }
                    }
                    let c = if disarida {
                        [0.5, 0.5, 0.5]
                    } else {
                        ornekle(&rgba, w, h, u, v)
                    };
                    for (ch, value) in c.iter().enumerate() {
                        input[(ch * n + y) * n + x] = *value;
                    }
                }
            }

            let mask = seg.mask(&input).expect("maske");
            if let Ok(dir) = std::env::var("OWNCAM_MASKE_DIZIN") {
                let bytes: Vec<u8> =
                    mask.iter().map(|m| (m.clamp(0.0, 1.0) * 255.0) as u8).collect();
                let _ = std::fs::write(format!("{dir}/maske_{mod_ad}.bin"), bytes);
            }
            let kapsam = mask.iter().sum::<f32>() / mask.len() as f32;
            let kararsiz = mask.iter().filter(|m| (0.2..=0.8).contains(*m)).count() as f32
                / mask.len() as f32;
            eprintln!(
                "{mod_ad:9} kapsam {kapsam:.3}  kararsiz piksel {:.4}",
                kararsiz
            );
        }
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
        let Ok(seg) = Segmenter::with_size(input_shape()) else {
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

    /// **Gelistirme kancasi**: harici bir modeli GPU'da kosup maskeyi yaz.
    ///
    /// `plan.rs`'teki kardesi plani kuruyor, bu onu calistiriyor. Cikti ayni
    /// girdiyle bagimsiz bir ONNX calisma zamanindan (tract) alinan maskeyle
    /// karsilastirilmak icin ham f32 olarak yaziliyor.
    ///
    /// ```text
    /// OWNCAM_MODEL=/yol/rvm.onnx OWNCAM_HAM=/tmp/ham_rgb.bin \
    ///   OWNCAM_GIRDI=1280x720 OWNCAM_CIKTI=/tmp/gpu_alpha.f32 \
    ///   cargo test --release yabanci_model_kosusu -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "elle calistirilir; harici model dosyasi gerektirir"]
    fn yabanci_model_kosusu() {
        let (Ok(model), Ok(ham)) = (
            std::env::var("OWNCAM_MODEL"),
            std::env::var("OWNCAM_HAM"),
        ) else {
            eprintln!("OWNCAM_MODEL ve OWNCAM_HAM gerekli");
            return;
        };
        let (w, h) = std::env::var("OWNCAM_GIRDI")
            .ok()
            .and_then(|v| {
                let (a, b) = v.split_once('x')?;
                Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?))
            })
            .expect("OWNCAM_GIRDI=ExB");

        let bytes = std::fs::read(&model).expect("model okunamadi");
        let mut g = onnx::parse(&bytes).expect("ONNX ayristirilamadi");
        if g.inputs.iter().any(|n| n == "downsample_ratio") {
            let r = std::env::var("OWNCAM_ORAN")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.25f32);
            g.set_input_constant("downsample_ratio", r);
        }
        let plan = plan::build(&g, Shape { c: 3, h, w }).expect("plan");
        let seg = Segmenter::from_plan(plan).expect("gpu");

        // Ham dosya HWC u8 RGB; ag NCHW f32 0..1 bekliyor.
        let raw = std::fs::read(&ham).expect("ham kare okunamadi");
        assert_eq!(raw.len(), w * h * 3, "ham kare boyutu tutmuyor");
        let mut input = vec![0f32; 3 * h * w];
        for y in 0..h {
            for x in 0..w {
                for c in 0..3 {
                    input[(c * h + y) * w + x] = raw[(y * w + x) * 3 + c] as f32 / 255.0;
                }
            }
        }

        let mask = seg.mask(&input).expect("maske");
        eprintln!(
            "{} | {} adim, arena {:.1} MB | maske {:?}",
            seg.adapter_name,
            seg.steps(),
            seg.arena_bytes() as f64 / 1e6,
            seg.mask_shape()
        );
        let n = 30;
        let t0 = std::time::Instant::now();
        let mut previous = mask.clone();
        let mut deltas = Vec::new();
        for _ in 0..n {
            let m = seg.mask(&input).unwrap();
            let d: f64 = m
                .iter()
                .zip(&previous)
                .map(|(a, b)| (a - b).abs() as f64)
                .sum::<f64>()
                / m.len() as f64;
            deltas.push(d);
            previous = m;
        }
        eprintln!("kare basina {:?}", t0.elapsed() / n);
        // Gizli durum calisiyorsa ayni kare tekrar verilse bile ilk birkac
        // kare degisir (durum doluyor) ve sonra sabitlenir. Hic degismiyorsa
        // geri besleme kopmus demektir.
        eprintln!(
            "kareler arasi maske farki: 1.{:.6}  2.{:.6}  3.{:.6}  son {:.6}",
            deltas[0],
            deltas[1],
            deltas[2],
            deltas[n as usize - 1]
        );

        if let Ok(out) = std::env::var("OWNCAM_CIKTI") {
            std::fs::write(&out, bytemuck::cast_slice(&mask)).unwrap();
            eprintln!("maske yazildi: {out}");
        }
    }

    /// **Olcum**: iki ag ardisik karelerde ne kadar titriyor?
    ///
    /// RVM'nin gerekcesi yalnizca kenar keskinligi degil, **zamansal
    /// tutarlilik**: gizli durum tasidigi icin ayni sahnede maske kareden
    /// kareye zipladmamali. Hizli ag her kareyi bagimsiz cozuyor.
    ///
    /// Gercek video yerine ayni kareye kare basina bagimsiz gurultu
    /// ekleniyor - titremenin fiziksel sebebi zaten bu. Iki ag farkli
    /// cozunurlukte maske uretiyor, bu yuzden karsilastirma **kare
    /// cozunurlugune buyutulmus** maske uzerinde yapiliyor; aksi halde kucuk
    /// maskenin titremesi oldugundan buyuk gorunurdu.
    ///
    /// ```text
    /// OWNCAM_HAM=/tmp/ham_rgb.bin OWNCAM_GIRDI=1280x720 \
    ///   OWNCAM_MODEL=/tmp/rvm.onnx \
    ///   cargo test --release titreme_karsilastirmasi -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "olcum; harici model ve kare dosyasi gerektirir"]
    fn titreme_karsilastirmasi() {
        let Ok(ham) = std::env::var("OWNCAM_HAM") else {
            eprintln!("OWNCAM_HAM gerekli");
            return;
        };
        let (w, h) = std::env::var("OWNCAM_GIRDI")
            .ok()
            .and_then(|v| {
                let (a, b) = v.split_once('x')?;
                Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?))
            })
            .expect("OWNCAM_GIRDI=ExB");
        let raw = std::fs::read(&ham).expect("ham kare");
        assert_eq!(raw.len(), w * h * 3);

        let mut modeller = vec![Model::Hizli];
        if let Ok(yol) = std::env::var("OWNCAM_MODEL") {
            modeller.push(Model::Kaliteli {
                yol: yol.into(),
                oran: 0.375,
            });
        }

        for model in &modeller {
            let seg = match Segmenter::for_frame(model, (w as u32, h as u32)) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("{}: kurulamadi: {e}", model.label());
                    continue;
                }
            };
            let net = seg.input_size();
            let mask_shape = seg.mask_shape();
            let sigma = 2.0 / 255.0;
            let mut state = 0x2545_F491u32;
            let (mut sum, mut frames) = (0.0f64, 0u32);
            let mut previous: Option<Vec<f32>> = None;
            // Ilk kareler RVM'nin gizli durumu dolarken geciyor; olcumun
            // disinda tutuluyorlar. Hizli agin durumu olmadigi icin bu onu
            // etkilemiyor - yani karsilastirma haksizlik yapmiyor.
            const ISINMA: u32 = 5;

            for i in 0..25u32 {
                // Kareyi agin girdi olcusune ornekle ve gurultu ekle.
                let mut input = vec![0f32; net.len()];
                for y in 0..net.h {
                    for x in 0..net.w {
                        let sx = (x * w / net.w).min(w - 1);
                        let sy = (y * h / net.h).min(h - 1);
                        for c in 0..3 {
                            let v = raw[(sy * w + sx) * 3 + c] as f32 / 255.0;
                            let n = (xorshift(&mut state) - 0.5) * 2.0 * sigma;
                            input[(c * net.h + y) * net.w + x] = (v + n).clamp(0.0, 1.0);
                        }
                    }
                }
                let mask = buyut(&seg.mask(&input).unwrap(), mask_shape.w, mask_shape.h, w, h);
                if let (Some(prev), true) = (&previous, i > ISINMA) {
                    sum += mask
                        .iter()
                        .zip(prev)
                        .map(|(a, b)| (a - b).abs() as f64)
                        .sum::<f64>()
                        / mask.len() as f64;
                    frames += 1;
                }
                previous = Some(mask);
            }
            eprintln!(
                "{:9} maske {}x{} -> kare cozunurlugunde ardisik fark {:.5}",
                model.label(),
                mask_shape.w,
                mask_shape.h,
                sum / frames as f64
            );
        }
    }

    /// Iki dogrusal buyutme - iki agin maskesini ayni olcude karsilastirmak
    /// icin. Kompozit shader'i da ayni seyi yapiyor.
    fn buyut(src: &[f32], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<f32> {
        if (sw, sh) == (dw, dh) {
            return src.to_vec();
        }
        let mut out = vec![0f32; dw * dh];
        for y in 0..dh {
            let fy = ((y as f32 + 0.5) * sh as f32 / dh as f32 - 0.5).clamp(0.0, sh as f32 - 1.0);
            let (y0, ty) = (fy.floor() as usize, fy - fy.floor());
            let y1 = (y0 + 1).min(sh - 1);
            for x in 0..dw {
                let fx =
                    ((x as f32 + 0.5) * sw as f32 / dw as f32 - 0.5).clamp(0.0, sw as f32 - 1.0);
                let (x0, tx) = (fx.floor() as usize, fx - fx.floor());
                let x1 = (x0 + 1).min(sw - 1);
                let a = src[y0 * sw + x0] * (1.0 - tx) + src[y0 * sw + x1] * tx;
                let b = src[y1 * sw + x0] * (1.0 - tx) + src[y1 * sw + x1] * tx;
                out[y * dw + x] = a * (1.0 - ty) + b * ty;
            }
        }
        out
    }

    #[test]
    fn maske_referansla_ortusuyor() {
        // Demirbas 256x256'da uretildi; olcu ortamdan gelmemeli.
        let seg = match Segmenter::with_size(INPUT) {
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
