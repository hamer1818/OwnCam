//! Graftan **calisma plani** cikarma.
//!
//! Girdi olcusu yuklemede belli. Bu yuzden butun sekiller, dolgu degerleri ve
//! tampon kaymalari bir kez hesaplaniyor; kare basina is yalnizca hazir
//! parametrelerle sevk etmek oluyor.
//!
//! Yalnizca sekil hesabi yapan dugumler (`Shape`/`Slice`/`Concat`/`Constant`)
//! GPU'ya hic gitmiyor: plan kurulurken sayi olarak katlaniyorlar.
//!
//! Iki model destekleniyor ve ikisi de ayni yoldan geciyor:
//!
//! - **MediaPipe Selfie Segmentation** - tek girdi, tek cikti, durumsuz.
//! - **Robust Video Matting** - bes girdi (kare + dort gizli durum), alti
//!   cikti (on plan, alfa, dort yeni durum). Gizli durumlar arenada kalici
//!   yer tutuyor; kare sonunda `rNo` bolgesi `rNi` bolgesine kopyalaniyor.

use std::collections::HashMap;

use super::onnx::{self, Graph, Tensor};

/// Toplu is boyutu her zaman 1; kanal, yukseklik, genislik.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    pub c: usize,
    pub h: usize,
    pub w: usize,
}

impl Shape {
    pub fn len(&self) -> usize {
        self.c * self.h * self.w
    }
}

/// Yayinli ikili islemler. Dort islem de ayni cekirdek govdesini paylasiyor,
/// yalnizca giris noktalari ayri.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Conv,
    ConvTranspose,
    /// Iki dogrusal olcekleme, half_pixel. Buyutme ve kucultme ayni cekirdek.
    Resize,
    /// H ve W uzerinden ortalama -> [1,C,1,1]
    ReduceMean,
    /// C uzerinden ortalama -> [1,1,H,W]
    ReduceChannel,
    /// Pencere ortalamasi; `ceil_mode` ve kismi pencere sayimi dahil.
    AvgPool,
    Relu,
    HardSwish,
    Sigmoid,
    Tanh,
    /// clamp(alpha*x + beta, 0, 1)
    HardSigmoid,
    /// clamp(x, alpha, beta)
    Clip,
    /// Yayinli ikili islem. Her iki isleneninin sekli ayri tutuluyor.
    Binary(BinOp),
    /// Duz kopya. `Concat`in her parcasi ve gizli durum geri beslemesi.
    Copy,
    /// Uzamsal kirpma; baslangic `pad_t`/`pad_l` alanlarinda.
    Crop,
}

/// Tek bir sevk. Butun alanlar dogrudan shader'a gidiyor.
#[derive(Debug, Clone)]
pub struct Step {
    pub kind: Kind,
    /// Arena icindeki eleman kaymalari
    pub a: u32,
    pub b: u32,
    pub out: u32,
    /// Agirlik tamponundaki eleman kaymalari
    pub weight: u32,
    pub bias: u32,
    pub in_shape: Shape,
    /// Ikinci islenenin sekli; yayin bunun uzerinden yapiliyor.
    pub b_shape: Shape,
    pub out_shape: Shape,
    pub kh: u32,
    pub kw: u32,
    pub stride: u32,
    pub pad_t: u32,
    pub pad_l: u32,
    pub group: u32,
    pub dilation: u32,
    pub alpha: f32,
    pub beta: f32,
}

impl Step {
    fn new(a: u32, in_shape: Shape) -> Self {
        Step {
            kind: Kind::Relu,
            a,
            b: 0,
            out: 0,
            weight: 0,
            bias: 0,
            in_shape,
            b_shape: in_shape,
            out_shape: in_shape,
            kh: 1,
            kw: 1,
            stride: 1,
            pad_t: 0,
            pad_l: 0,
            group: 1,
            dilation: 1,
            alpha: 0.0,
            beta: 0.0,
        }
    }
}

/// Kareler arasi tasinan gizli durum.
#[derive(Debug, Clone)]
pub struct State {
    pub offset: u32,
    pub shape: Shape,
}

pub struct Plan {
    pub steps: Vec<Step>,
    pub weights: Vec<f32>,
    pub arena_len: usize,
    /// Yuklemede arenaya yazilacak sabitler (normalizasyon ortalamasi gibi).
    pub constants: Vec<(u32, Vec<f32>)>,
    /// Yinelemeli gizli durumlar; yayin baslarken sifirlanmalari gerekiyor.
    pub states: Vec<State>,
    /// Agin kullanmadigi, arenanin sonundaki kucuk calisma alani. Kompozit
    /// gecisi maske kapsamini buraya yaziyor; boylece yeni bir depolama
    /// tamponu baglamak gerekmiyor (sinir zaten 8'de).
    pub scratch_off: u32,
    pub input_off: u32,
    /// Planin hangi girdi olcusu icin kuruldugu; teshis icin duruyor.
    #[allow(dead_code)]
    pub input_shape: Shape,
    /// Ad -> (kayma, sekil). Maske disindaki ciktilar (RVM'de `fgr`) buradan.
    pub outputs: HashMap<String, (u32, Shape)>,
    pub output_off: u32,
    pub output_shape: Shape,
}

impl Plan {
    /// Adiyla bir cikti tensoru.
    #[allow(dead_code)]
    pub fn output(&self, name: &str) -> Option<(u32, Shape)> {
        self.outputs.get(name).copied()
    }
}

/// Modelin egitildigi girdi olcusu. Selfie modelinde graftaki sabit `Resize`
/// hedefleri buna gore yazilmis; baska bir olcude ayni oranda olcekleniyor.
pub const REFERENCE_INPUT: usize = 256;

/// Arenanin sonunda ayrilan calisma alani (f32 cinsinden).
const SCRATCH: usize = 4;

/// Sekil hesaplarinda kullanilan, GPU'ya gitmeyen sabit vektorler.
enum Folded {
    Ints(Vec<i64>),
    /// `Constant` dugumleri sekil vektoru disinda **veri** de tasiyor
    /// (normalizasyonun ortalama/standart sapmasi gibi).
    Floats(Vec<f32>, Vec<usize>),
}

/// ONNX dort boyutlu calisiyor ama toplu is boyutu hep 1; son uc boyutu
/// aliyoruz, eksik olanlari 1 ile dolduruyoruz. Skaler (`dims == []`) 1x1x1.
fn shape_from_dims(dims: &[usize]) -> Shape {
    let mut d = [1usize; 3];
    let n = dims.len().min(3);
    for i in 0..n {
        d[3 - n + i] = dims[dims.len() - n + i];
    }
    Shape {
        c: d[0],
        h: d[1],
        w: d[2],
    }
}

/// Eksi eksen numaralarini 4 boyutlu tensore gore duzelt.
fn axis4(axis: i64) -> i64 {
    if axis < 0 {
        axis + 4
    } else {
        axis
    }
}

struct Builder<'a> {
    graph: &'a Graph,
    weights: Vec<f32>,
    weight_off: HashMap<String, u32>,
    shapes: HashMap<String, Shape>,
    offsets: HashMap<String, u32>,
    folded: HashMap<String, Folded>,
    constants: Vec<(u32, Vec<f32>)>,
    states: Vec<State>,
    steps: Vec<Step>,
    arena_len: usize,
    /// Girdi olcusu / referans olcu; selfie modelinin sabit `Resize`
    /// hedeflerini olceklemek icin.
    resize_scale: f64,
}

impl<'a> Builder<'a> {
    fn alloc(&mut self, shape: Shape) -> u32 {
        let off = self.arena_len as u32;
        self.arena_len += shape.len();
        off
    }

    /// Bir agirligi tek agirlik tamponuna diz; ayni ad iki kez gelirse
    /// (RVM'nin kutu filtresi) yalnizca bir kez yer kapliyor.
    fn pack(&mut self, name: &str) -> Result<u32, String> {
        if let Some(off) = self.weight_off.get(name) {
            return Ok(*off);
        }
        let t = self
            .graph
            .init
            .get(name)
            .ok_or_else(|| format!("agirlik yok: {name}"))?;
        if t.dtype != onnx::DT_FLOAT {
            return Err(format!("{name}: f32 bekleniyordu"));
        }
        let off = self.weights.len() as u32;
        self.weights.extend(t.floats());
        self.weight_off.insert(name.to_string(), off);
        Ok(off)
    }

    /// Yanliligi olmayan evrisimler icin sifir vektoru. Cekirdekte kosul
    /// tutmaktansa agirlik tamponuna birkac sifir koymak daha ucuz.
    fn zero_bias(&mut self, n: usize) -> u32 {
        let key = format!("\0sifir{n}");
        if let Some(off) = self.weight_off.get(&key) {
            return *off;
        }
        let off = self.weights.len() as u32;
        self.weights.resize(off as usize + n, 0.0);
        self.weight_off.insert(key, off);
        off
    }

    fn tensor(&self, name: &str) -> Option<&'a Tensor> {
        self.graph.init.get(name)
    }

    /// Sabit bir tensorun f32 degerleri - katlanmis dugumden ya da
    /// baslaticidan.
    fn floats_of(&self, name: &str) -> Option<(Vec<f32>, Vec<usize>)> {
        match self.folded.get(name) {
            Some(Folded::Floats(v, d)) => Some((v.clone(), d.clone())),
            _ => self
                .tensor(name)
                .filter(|t| t.dtype == onnx::DT_FLOAT)
                .map(|t| (t.floats(), t.dims.clone())),
        }
    }

    fn ints_of(&self, name: &str) -> Option<Vec<i64>> {
        match self.folded.get(name) {
            Some(Folded::Ints(v)) => Some(v.clone()),
            _ => self
                .tensor(name)
                .filter(|t| t.dtype == onnx::DT_INT64)
                .map(|t| t.i64s()),
        }
    }

    /// Bir hesap isleneni: ya arenada duran bir tensor ya da arenaya
    /// tasinmasi gereken bir sabit.
    fn operand(&mut self, name: &str) -> Result<(u32, Shape), String> {
        if let (Some(off), Some(shape)) = (self.offsets.get(name), self.shapes.get(name)) {
            return Ok((*off, *shape));
        }
        let (values, dims) = self
            .floats_of(name)
            .ok_or_else(|| format!("{name}: ne arenada ne sabit"))?;
        let shape = shape_from_dims(&dims);
        if shape.len() != values.len() {
            return Err(format!("{name}: sekil {dims:?} ile {} deger", values.len()));
        }
        let off = self.alloc(shape);
        self.constants.push((off, values));
        self.offsets.insert(name.to_string(), off);
        self.shapes.insert(name.to_string(), shape);
        Ok((off, shape))
    }

    fn finish_step(&mut self, mut step: Step, out_name: String) {
        step.out = self.alloc(step.out_shape);
        self.shapes.insert(out_name.clone(), step.out_shape);
        self.offsets.insert(out_name, step.out);
        self.steps.push(step);
    }
}

pub fn build(graph: &Graph, input: Shape) -> Result<Plan, String> {
    let mut b = Builder {
        graph,
        weights: Vec::new(),
        weight_off: HashMap::new(),
        shapes: HashMap::new(),
        offsets: HashMap::new(),
        folded: HashMap::new(),
        constants: Vec::new(),
        states: Vec::new(),
        steps: Vec::new(),
        arena_len: 0,
        resize_scale: input.w as f64 / REFERENCE_INPUT as f64,
    };

    b.shapes.insert(graph.input.clone(), input);
    let input_off = b.alloc(input);
    b.offsets.insert(graph.input.clone(), input_off);

    // Gizli durumlar: ilk girdi disindaki butun graf girdileri. Sekilleri
    // graftan cikiyor (`Expand` dugumu soyluyor), bu yuzden simdilik yalnizca
    // adlarini biliyoruz.
    let state_names: Vec<String> = graph.inputs.iter().skip(1).cloned().collect();

    for node in &graph.nodes {
        let out_name = node
            .output
            .first()
            .ok_or_else(|| format!("{}: cikti yok", node.op))?
            .clone();

        if fold(&mut b, node, &out_name)? {
            continue;
        }

        match node.op.as_str() {
            // --- yer degistirmeyen dugumler: kayma aritmetigi, sevk yok ---
            "Split" => {
                split(&mut b, node)?;
                continue;
            }
            "Expand" => {
                expand(&mut b, node, &out_name, &state_names)?;
                continue;
            }
            "Concat" => {
                concat(&mut b, node, out_name)?;
                continue;
            }
            _ => {}
        }

        let in_name = node
            .input
            .first()
            .ok_or_else(|| format!("{}: girdisi olmayan dugum ({out_name})", node.op))?
            .clone();
        let (a, in_shape) = b.operand(&in_name)?;
        let mut step = Step::new(a, in_shape);

        match node.op.as_str() {
            "Conv" | "ConvTranspose" => {
                let w = b
                    .tensor(&node.input[1])
                    .ok_or_else(|| format!("{}: cekirdek yok", node.op))?;
                let (kh, kw) = (w.dims[2], w.dims[3]);
                let (out_c, _) = (w.dims[0], w.dims[1]);
                let w_dims = w.dims.clone();
                let pads = node.ints("pads");
                let (pt, pl, pb, pr) = if pads.len() == 4 {
                    (
                        pads[0] as usize,
                        pads[1] as usize,
                        pads[2] as usize,
                        pads[3] as usize,
                    )
                } else {
                    (0, 0, 0, 0)
                };
                let strides = node.ints("strides");
                let stride = if strides.is_empty() {
                    1
                } else {
                    strides[0] as usize
                };
                let dilations = node.ints("dilations");
                let dilation = if dilations.is_empty() {
                    1
                } else {
                    dilations[0] as usize
                };
                if dilations.len() == 2 && dilations[0] != dilations[1] {
                    return Err(format!("{}: eksende farkli genisleme", node.op));
                }
                let group = node.int("group", 1) as usize;

                let (ekh, ekw) = ((kh - 1) * dilation + 1, (kw - 1) * dilation + 1);
                let out_shape = if node.op == "Conv" {
                    Shape {
                        c: out_c,
                        h: (in_shape.h + pt + pb - ekh) / stride + 1,
                        w: (in_shape.w + pl + pr - ekw) / stride + 1,
                    }
                } else {
                    Shape {
                        // ConvTranspose cekirdegi [C_in, C_out/group, kh, kw]
                        c: w_dims[1] * group,
                        h: (in_shape.h - 1) * stride + ekh - pt - pb,
                        w: (in_shape.w - 1) * stride + ekw - pl - pr,
                    }
                };

                step.kind = if node.op == "Conv" {
                    Kind::Conv
                } else {
                    Kind::ConvTranspose
                };
                step.weight = b.pack(&node.input[1])?;
                step.bias = match node.input.get(2).filter(|n| !n.is_empty()) {
                    Some(name) => b.pack(name)?,
                    None => b.zero_bias(out_shape.c),
                };
                step.out_shape = out_shape;
                step.kh = kh as u32;
                step.kw = kw as u32;
                step.stride = stride as u32;
                step.pad_t = pt as u32;
                step.pad_l = pl as u32;
                step.group = group as u32;
                step.dilation = dilation as u32;
            }
            "Relu" => step.kind = Kind::Relu,
            "HardSwish" => step.kind = Kind::HardSwish,
            "Sigmoid" => step.kind = Kind::Sigmoid,
            "Tanh" => step.kind = Kind::Tanh,
            "HardSigmoid" => {
                step.kind = Kind::HardSigmoid;
                // ONNX varsayilanlari; PyTorch disa aktarimi alpha=1/6 yaziyor.
                step.alpha = node.float("alpha", 0.2);
                step.beta = node.float("beta", 0.5);
            }
            "Clip" => {
                step.kind = Kind::Clip;
                let bound = |i: usize, d: f32| -> f32 {
                    node.input
                        .get(i)
                        .and_then(|n| b.floats_of(n))
                        .and_then(|(v, _)| v.first().copied())
                        .unwrap_or(d)
                };
                step.alpha = bound(1, f32::MIN);
                step.beta = bound(2, f32::MAX);
            }
            "Add" | "Sub" | "Mul" | "Div" => {
                let op = match node.op.as_str() {
                    "Add" => BinOp::Add,
                    "Sub" => BinOp::Sub,
                    "Mul" => BinOp::Mul,
                    _ => BinOp::Div,
                };
                let (bo, bs) = b.operand(&node.input[1])?;
                let out_shape = broadcast(in_shape, bs)
                    .ok_or_else(|| format!("{}: {in_shape:?} ve {bs:?} yayilmiyor", node.op))?;
                step.kind = Kind::Binary(op);
                step.b = bo;
                step.b_shape = bs;
                step.out_shape = out_shape;
            }
            "ReduceMean" | "GlobalAveragePool" => {
                let axes = node.ints("axes");
                // `GlobalAveragePool`un ekseni yok; tanimi geregi H ve W.
                if node.op == "GlobalAveragePool" || axes == [2, 3] {
                    step.kind = Kind::ReduceMean;
                    step.out_shape = Shape {
                        c: in_shape.c,
                        h: 1,
                        w: 1,
                    };
                } else if axes == [1] {
                    step.kind = Kind::ReduceChannel;
                    step.out_shape = Shape { c: 1, ..in_shape };
                } else {
                    return Err(format!("ReduceMean: beklenmeyen eksenler {axes:?}"));
                }
            }
            "AveragePool" => {
                let k = node.ints("kernel_shape");
                let s = node.ints("strides");
                let (kh, kw) = (k[0] as usize, k[1] as usize);
                let stride = if s.is_empty() { 1 } else { s[0] as usize };
                // `ceil_mode` tasan pencereyi de uretiyor; cekirdek gecerli
                // eleman sayisina bolerek ONNX'in count_include_pad=0
                // davranisini veriyor.
                let boyut = |n: usize, k: usize| -> usize {
                    if node.int("ceil_mode", 0) == 1 {
                        (n - k).div_ceil(stride) + 1
                    } else {
                        (n - k) / stride + 1
                    }
                };
                step.kind = Kind::AvgPool;
                step.kh = kh as u32;
                step.kw = kw as u32;
                step.stride = stride as u32;
                step.out_shape = Shape {
                    c: in_shape.c,
                    h: boyut(in_shape.h, kh),
                    w: boyut(in_shape.w, kw),
                };
            }
            "Resize" => resize(&mut b, node, &mut step, in_shape)?,
            "Slice" => crop(&mut b, node, &mut step, in_shape)?,
            other => return Err(format!("desteklenmeyen operator: {other}")),
        }

        b.finish_step(step, out_name);
    }

    // Gizli durumlarin geri beslemesi: `rNo` -> `rNi`. Adlandirma RVM'nin
    // kurali; eslesme bulunamazsa hata veriyoruz, sessizce durumsuz kosmak
    // en kotu turden bir hata olurdu.
    for name in &state_names {
        let (off, shape) = match (b.offsets.get(name), b.shapes.get(name)) {
            (Some(o), Some(s)) => (*o, *s),
            // Graf bu durumu hic kullanmamis; tasiyacak bir sey yok.
            _ => continue,
        };
        let out = format!("{}o", name.trim_end_matches('i'));
        let (src, src_shape) = b
            .offsets
            .get(&out)
            .zip(b.shapes.get(&out))
            .map(|(o, s)| (*o, *s))
            .ok_or_else(|| format!("gizli durum {name} icin {out} ciktisi yok"))?;
        if src_shape != shape {
            return Err(format!("gizli durum {name}: {shape:?} != {src_shape:?}"));
        }
        b.states.push(State { offset: off, shape });
        let mut step = Step::new(src, shape);
        step.kind = Kind::Copy;
        step.out = off;
        b.steps.push(step);
    }

    let mut outputs = HashMap::new();
    for name in &graph.outputs {
        if let (Some(off), Some(shape)) = (b.offsets.get(name), b.shapes.get(name)) {
            outputs.insert(name.clone(), (*off, *shape));
        }
    }
    // Maske: RVM'de `pha`, selfie modelinde grafin tek ciktisi.
    let mask_name = if outputs.contains_key("pha") {
        "pha".to_string()
    } else {
        graph.output.clone()
    };
    let (output_off, output_shape) = *outputs
        .get(&mask_name)
        .ok_or_else(|| format!("cikti {mask_name} uretilmemis"))?;

    let scratch_off = b.arena_len as u32;
    b.arena_len += SCRATCH;

    Ok(Plan {
        steps: b.steps,
        weights: b.weights,
        arena_len: b.arena_len,
        constants: b.constants,
        states: b.states,
        scratch_off,
        input_off,
        input_shape: input,
        outputs,
        output_off,
        output_shape,
    })
}

/// Iki islenenin ortak cikti sekli. Her eksen ya esit ya da biri 1 olmali.
fn broadcast(a: Shape, b: Shape) -> Option<Shape> {
    let eksen = |x: usize, y: usize| -> Option<usize> {
        if x == y || y == 1 {
            Some(x)
        } else if x == 1 {
            Some(y)
        } else {
            None
        }
    };
    Some(Shape {
        c: eksen(a.c, b.c)?,
        h: eksen(a.h, b.h)?,
        w: eksen(a.w, b.w)?,
    })
}

/// Yalnizca sekil hesabi yapan dugumler. Isini gorduyse `true` donuyor.
fn fold(b: &mut Builder, node: &onnx::Node, out_name: &str) -> Result<bool, String> {
    match node.op.as_str() {
        "Constant" => {
            let t = node
                .tensor("value")
                .ok_or_else(|| format!("Constant {out_name}: deger yok"))?;
            match t.dtype {
                onnx::DT_INT64 => b.folded.insert(out_name.into(), Folded::Ints(t.i64s())),
                onnx::DT_FLOAT => b.folded.insert(
                    out_name.into(),
                    Folded::Floats(t.floats(), t.dims.clone()),
                ),
                other => return Err(format!("Constant {out_name}: tip {other}")),
            };
            Ok(true)
        }
        "Shape" => {
            let s = b
                .shapes
                .get(&node.input[0])
                .ok_or_else(|| format!("Shape: {} sekli bilinmiyor", node.input[0]))?;
            b.folded.insert(
                out_name.into(),
                Folded::Ints(vec![1, s.c as i64, s.h as i64, s.w as i64]),
            );
            Ok(true)
        }
        // Sekil vektorunu dilimleme. Gercek veriyi dilimleyen `Slice`
        // dugumleri asagida, kirpma cekirdegi olarak ele aliniyor.
        "Slice" if matches!(b.folded.get(&node.input[0]), Some(Folded::Ints(_))) => {
            let src = match b.folded.get(&node.input[0]) {
                Some(Folded::Ints(v)) => v.clone(),
                _ => unreachable!(),
            };
            let get = |i: usize| -> Result<i64, String> {
                let name = node
                    .input
                    .get(i)
                    .ok_or_else(|| format!("Slice: {i}. girdi yok"))?;
                b.ints_of(name)
                    .and_then(|v| v.first().copied())
                    .ok_or_else(|| format!("Slice: {name} sabit degil"))
            };
            let (start, end) = (get(1)?.max(0) as usize, get(2)?);
            let end = if end < 0 || end as usize > src.len() {
                src.len()
            } else {
                end as usize
            };
            b.folded
                .insert(out_name.into(), Folded::Ints(src[start..end].to_vec()));
            Ok(true)
        }
        "Concat" => {
            if node.input.iter().all(|n| b.ints_of(n).is_some()) {
                let mut all = Vec::new();
                for n in &node.input {
                    let v = b.ints_of(n).unwrap();
                    // Selfie modelinde sabit hedef boyutlar 256'lik girdiye
                    // gore yazilmis; baska olcude ayni oranda buyutuluyor.
                    // Katlanmis parcalar (batch/kanal, ya da RVM'de Shape'ten
                    // gelen gercek boyutlar) oldugu gibi kaliyor.
                    if b.folded.contains_key(n) {
                        all.extend(v);
                    } else {
                        all.extend(
                            v.into_iter()
                                .map(|x| (x as f64 * b.resize_scale).round() as i64),
                        );
                    }
                }
                b.folded.insert(out_name.into(), Folded::Ints(all));
                return Ok(true);
            }
            // Butun parcalari sabit float ise sekil hesabi (Resize olcegi);
            // degilse gercek kanal birlestirmesi, asagida.
            if node.input.iter().all(|n| {
                b.floats_of(n).is_some() && !b.offsets.contains_key(n)
            }) {
                let mut all = Vec::new();
                for n in &node.input {
                    all.extend(b.floats_of(n).unwrap().0);
                }
                let len = all.len();
                b.folded
                    .insert(out_name.into(), Folded::Floats(all, vec![len]));
                return Ok(true);
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}

/// Kanal ekseninde bolme. NCHW yerlesiminde bu yalnizca kayma aritmetigi -
/// hicbir veri kopyalanmiyor.
fn split(b: &mut Builder, node: &onnx::Node) -> Result<(), String> {
    if axis4(node.int("axis", 0)) != 1 {
        return Err("Split: yalnizca kanal ekseni destekleniyor".into());
    }
    let (a, shape) = b.operand(&node.input[0])?;
    let parts = node.ints("split").to_vec();
    if parts.is_empty() {
        return Err("Split: parca boyutlari yok".into());
    }
    let mut off = a;
    for (part, name) in parts.iter().zip(&node.output) {
        let s = Shape {
            c: *part as usize,
            ..shape
        };
        b.shapes.insert(name.clone(), s);
        b.offsets.insert(name.clone(), off);
        off += s.len() as u32;
    }
    Ok(())
}

/// Gizli durumu tam sekline yayma.
///
/// Durum tamponunu **tam boyutta** tuttugumuz icin bu bir kopya degil, takma
/// ad: `Expand` ciktisi durumun kendisi. RVM ilk karede [1,1,1,1] sifir
/// verilsin diye bu dugumu koymus; bizde durum zaten sifirla basliyor.
fn expand(
    b: &mut Builder,
    node: &onnx::Node,
    out_name: &str,
    states: &[String],
) -> Result<(), String> {
    let src = &node.input[0];
    let dims = b
        .ints_of(&node.input[1])
        .ok_or("Expand: hedef sekil sabit degil")?;
    let shape = shape_from_dims(
        &dims
            .iter()
            .map(|v| *v as usize)
            .collect::<Vec<_>>(),
    );
    if !states.contains(src) {
        return Err(format!("Expand yalnizca gizli durumlar icin: {src}"));
    }
    let off = match b.offsets.get(src) {
        Some(o) => *o,
        None => {
            let o = b.alloc(shape);
            b.offsets.insert(src.clone(), o);
            b.shapes.insert(src.clone(), shape);
            o
        }
    };
    if b.shapes[src] != shape {
        return Err(format!("Expand: {src} sekli degisti"));
    }
    b.offsets.insert(out_name.into(), off);
    b.shapes.insert(out_name.into(), shape);
    Ok(())
}

/// Kanal ekseninde birlestirme: her parca cikti bolgesinin kendi dilimine
/// kopyalaniyor.
fn concat(b: &mut Builder, node: &onnx::Node, out_name: String) -> Result<(), String> {
    if axis4(node.int("axis", 0)) != 1 {
        return Err("Concat: yalnizca kanal ekseni destekleniyor".into());
    }
    let mut parts = Vec::new();
    for name in &node.input {
        parts.push(b.operand(name)?);
    }
    let first = parts[0].1;
    let mut c = 0;
    for (_, s) in &parts {
        if s.h != first.h || s.w != first.w {
            return Err(format!("Concat: uyusmayan olcu {s:?} != {first:?}"));
        }
        c += s.c;
    }
    let out_shape = Shape { c, ..first };
    let out = b.alloc(out_shape);
    let mut off = out;
    for (a, s) in parts {
        let mut step = Step::new(a, s);
        step.kind = Kind::Copy;
        step.out = off;
        b.steps.push(step);
        off += s.len() as u32;
    }
    b.shapes.insert(out_name.clone(), out_shape);
    b.offsets.insert(out_name, out);
    Ok(())
}

fn resize(
    b: &mut Builder,
    node: &onnx::Node,
    step: &mut Step,
    in_shape: Shape,
) -> Result<(), String> {
    // `pytorch_half_pixel`, `half_pixel`ten yalnizca cikti boyutu 1 oldugunda
    // ayriliyor (o durumda 0'a esliyor). Bu aglarda oyle bir olcek yok, ayni
    // cekirdek ikisini de karsiliyor.
    let ctm = node.text("coordinate_transformation_mode");
    if node.text("mode") != "linear" || !matches!(ctm, "half_pixel" | "pytorch_half_pixel") {
        return Err(format!(
            "Resize: yalnizca half_pixel linear destekleniyor (mode={}, ctm={ctm})",
            node.text("mode")
        ));
    }
    step.kind = Kind::Resize;

    // ONNX hedefi ya `scales` (girdi 2) ya da `sizes` (girdi 3) ile veriyor.
    // Bos bir `scales` tensoru de gecerli - o zaman `sizes` bakilir.
    let scales = node
        .input
        .get(2)
        .filter(|n| !n.is_empty())
        .and_then(|n| b.floats_of(n))
        .map(|(v, _)| v)
        .filter(|v| v.len() == 4);
    if let Some(v) = scales {
        step.out_shape = Shape {
            c: in_shape.c,
            h: ((in_shape.h as f32) * v[2]).round() as usize,
            w: ((in_shape.w as f32) * v[3]).round() as usize,
        };
        return Ok(());
    }

    let sizes_name = node.input.get(3).ok_or("Resize: hedef boyut girdisi yok")?;
    let sizes = b
        .ints_of(sizes_name)
        .ok_or("Resize: hedef boyut sabit degil")?;
    if sizes.len() != 4 {
        return Err(format!("Resize: hedef boyut {sizes:?}"));
    }
    step.out_shape = Shape {
        c: in_shape.c,
        h: sizes[2] as usize,
        w: sizes[3] as usize,
    };
    Ok(())
}

/// Gercek veriyi dilimleyen `Slice`: RVM'de tek islevi, buyutulmus oznitelik
/// haritasini tek sayili boyutlarda hedefe kirpmak.
fn crop(
    b: &mut Builder,
    node: &onnx::Node,
    step: &mut Step,
    in_shape: Shape,
) -> Result<(), String> {
    let vec_of = |i: usize| -> Vec<i64> {
        node.input
            .get(i)
            .and_then(|n| b.ints_of(n))
            .unwrap_or_default()
    };
    let starts = vec_of(1);
    let ends = vec_of(2);
    let axes = vec_of(3);
    if node.input.len() > 4 && vec_of(4).iter().any(|s| *s != 1) {
        return Err("Slice: yalnizca adim 1 destekleniyor".into());
    }
    let mut out = in_shape;
    let (mut start_h, mut start_w) = (0usize, 0usize);
    for (i, axis) in axes.iter().enumerate() {
        let start = starts.get(i).copied().unwrap_or(0).max(0) as usize;
        let end = ends.get(i).copied().unwrap_or(i64::MAX);
        match axis4(*axis) {
            2 => {
                start_h = start;
                out.h = (end.max(0) as usize).min(in_shape.h) - start;
            }
            3 => {
                start_w = start;
                out.w = (end.max(0) as usize).min(in_shape.w) - start;
            }
            other => return Err(format!("Slice: eksen {other} veri uzerinde desteklenmiyor")),
        }
    }
    let _ = b;
    step.kind = Kind::Crop;
    step.pad_t = start_h as u32;
    step.pad_l = start_w as u32;
    step.out_shape = out;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODEL: &[u8] = include_bytes!("../../assets/selfie_segmentation.onnx");

    fn plan() -> Plan {
        let g = super::super::onnx::parse(MODEL).unwrap();
        build(
            &g,
            Shape {
                c: 3,
                h: 256,
                w: 256,
            },
        )
        .expect("plan kurulmali")
    }

    #[test]
    fn plan_kurulur_ve_cikti_maske_boyutunda() {
        let p = plan();
        assert_eq!(
            p.output_shape,
            Shape {
                c: 1,
                h: 256,
                w: 256
            }
        );
    }

    /// Sekil dugumleri GPU'ya gitmemeli: 145 dugumun 9'u katlaniyor.
    #[test]
    fn sekil_dugumleri_katlanir() {
        let p = plan();
        assert_eq!(p.steps.len(), 145 - 9);
    }

    /// Ilk adim 3->16 kanal, yariya inen cozunurluk.
    #[test]
    fn ilk_adim_evrisim() {
        let p = plan();
        let s = &p.steps[0];
        assert_eq!(s.kind, Kind::Conv);
        assert_eq!(s.in_shape, Shape { c: 3, h: 256, w: 256 });
        assert_eq!(s.out_shape, Shape { c: 16, h: 128, w: 128 });
        assert_eq!((s.stride, s.pad_t, s.pad_l), (2, 0, 0));
    }

    /// Olcek buyutmeler 16->32->64->128 gitmeli.
    #[test]
    fn olcek_buyutme_zinciri() {
        let p = plan();
        let boyutlar: Vec<_> = p
            .steps
            .iter()
            .filter(|s| s.kind == Kind::Resize)
            .map(|s| (s.in_shape.h, s.out_shape.h))
            .collect();
        assert_eq!(boyutlar, vec![(16, 32), (32, 64), (64, 128)]);
    }

    /// Arena makul olmali: 256x256 girdi icin birkac on MB.
    #[test]
    fn arena_boyutu_makul() {
        let p = plan();
        let mb = (p.arena_len * 4) as f64 / 1e6;
        assert!(mb < 64.0, "arena {mb:.1} MB - fazla buyuk");
        // Dosyadaki 425 668 baytin 64'u sekil hesabinin int64 sabitleri;
        // GPU'ya yalnizca kalan f32 agirliklar gidiyor.
        assert_eq!(p.weights.len(), (425668 - 64) / 4);
        // Selfie modeli durumsuz ve arenaya sabit tasimiyor.
        assert!(p.states.is_empty());
        assert!(p.constants.is_empty());
    }

    /// **Gelistirme kancasi**: baska bir ONNX modelinin plani kurulabiliyor mu?
    ///
    /// Depoya girmeyen buyuk modelleri (or. RVM) denerken hangi operatorun
    /// eksik oldugunu tek satirda soyluyor.
    ///
    /// ```text
    /// OWNCAM_MODEL=/yol/model.onnx OWNCAM_GIRDI=1280x720 \
    ///   cargo test --release yabanci_model_plani -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "elle calistirilir; harici model dosyasi gerektirir"]
    fn yabanci_model_plani() {
        let Ok(path) = std::env::var("OWNCAM_MODEL") else {
            eprintln!("OWNCAM_MODEL verilmedi");
            return;
        };
        let bytes = std::fs::read(&path).expect("model okunamadi");
        let mut g = super::super::onnx::parse(&bytes).expect("ONNX ayristirilamadi");
        if g.inputs.iter().any(|n| n == "downsample_ratio") {
            let r = std::env::var("OWNCAM_ORAN")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.25f32);
            g.set_input_constant("downsample_ratio", r);
            eprintln!("downsample_ratio = {r}");
        }
        eprintln!("dugum {} baslatici {}", g.nodes.len(), g.init.len());
        eprintln!("girdi {:?}", g.inputs);
        eprintln!("cikti {:?}", g.outputs);
        let mut hist: HashMap<&str, usize> = HashMap::new();
        for n in &g.nodes {
            *hist.entry(n.op.as_str()).or_default() += 1;
        }
        let mut hist: Vec<_> = hist.into_iter().collect();
        hist.sort_by_key(|(op, _)| *op);
        for (op, n) in hist {
            eprintln!("  {op:<16} {n}");
        }
        // `OWNCAM_DOKUM` verilirse butun dugum listesi dosyaya yaziliyor;
        // eksik operatorun graftaki baglamini okumanin tek yolu bu.
        if let Ok(dokum) = std::env::var("OWNCAM_DOKUM") {
            use std::fmt::Write;
            let mut s = String::new();
            for (i, n) in g.nodes.iter().enumerate() {
                let sekil = |x: &String| match g.init.get(x) {
                    Some(t) => format!("{x}{:?}", t.dims),
                    None => x.clone(),
                };
                let girdi: Vec<_> = n.input.iter().map(sekil).collect();
                let ozn: Vec<_> = n
                    .attrs
                    .iter()
                    .map(|a| format!("{}={}/{}/{:?}", a.name, a.i, a.s, a.ints))
                    .collect();
                let _ = writeln!(
                    s,
                    "{i:3} {:<18} {:?} -> {:?}  {}",
                    n.op,
                    girdi,
                    n.output,
                    ozn.join(" ")
                );
            }
            std::fs::write(&dokum, s).unwrap();
            eprintln!("dokum yazildi: {dokum}");
        }
        let (w, h) = std::env::var("OWNCAM_GIRDI")
            .ok()
            .and_then(|v| {
                let (a, b) = v.split_once('x')?;
                Some((a.parse().ok()?, b.parse().ok()?))
            })
            .unwrap_or((256usize, 256usize));
        match build(&g, Shape { c: 3, h, w }) {
            Ok(p) => eprintln!(
                "plan kuruldu: {} adim, arena {:.1} MB, agirlik {:.1} MB, \
                 durum {}, maske {:?}",
                p.steps.len(),
                (p.arena_len * 4) as f64 / 1e6,
                (p.weights.len() * 4) as f64 / 1e6,
                p.states.len(),
                p.output_shape
            ),
            Err(e) => eprintln!("PLAN KURULAMADI: {e}"),
        }
    }

    /// Ag tamamen evrisimli: baska girdi olculerinde de plan kurulabilmeli ve
    /// cikti ayni oranda buyumeli.
    #[test]
    fn plan_baska_cozunurluklerde_de_kurulur() {
        let g = super::super::onnx::parse(MODEL).unwrap();
        for n in [128usize, 256, 384, 512] {
            let p = build(&g, Shape { c: 3, h: n, w: n })
                .unwrap_or_else(|e| panic!("{n} icin plan kurulmadi: {e}"));
            assert_eq!(
                p.output_shape,
                Shape { c: 1, h: n, w: n },
                "{n}: cikti olcusu girdiyle ayni olmali"
            );
            // Butun olcek buyutmeler tam iki kat kalmali.
            for s in p.steps.iter().filter(|s| s.kind == Kind::Resize) {
                assert_eq!(s.out_shape.h, s.in_shape.h * 2, "{n}: resize 2 kat degil");
                assert_eq!(s.out_shape.w, s.in_shape.w * 2, "{n}: resize 2 kat degil");
            }
            // Adim sayisi cozunurlukten bagimsiz.
            assert_eq!(p.steps.len(), 136, "{n}: adim sayisi degisti");
        }
    }

    /// Calisma alani agin cikti bolgesiyle cakismamali.
    #[test]
    fn calisma_alani_ciktiyla_cakismaz() {
        let p = plan();
        let cikti_son = p.output_off as usize + p.output_shape.len();
        assert!(
            p.scratch_off as usize >= cikti_son,
            "calisma alani {} cikti bolgesine ({}..{}) giriyor",
            p.scratch_off,
            p.output_off,
            cikti_son
        );
        assert!(p.scratch_off as usize + SCRATCH <= p.arena_len);
    }

    /// Derinlemesine evrisimler grup sayisi kanal sayisina esit olarak gelmeli.
    #[test]
    fn derinlemesine_evrisimler_isaretli() {
        let p = plan();
        let dw: Vec<_> = p
            .steps
            .iter()
            .filter(|s| s.kind == Kind::Conv && s.group > 1)
            .collect();
        assert_eq!(dw.len(), 11);
        for s in dw {
            assert_eq!(s.group as usize, s.in_shape.c);
            assert_eq!(s.out_shape.c, s.in_shape.c);
        }
    }

    #[test]
    fn yayin_sekilleri() {
        let a = Shape { c: 3, h: 8, w: 8 };
        assert_eq!(broadcast(a, Shape { c: 3, h: 1, w: 1 }), Some(a));
        assert_eq!(broadcast(Shape { c: 1, h: 1, w: 1 }, a), Some(a));
        assert_eq!(broadcast(a, a), Some(a));
        assert_eq!(broadcast(a, Shape { c: 4, h: 8, w: 8 }), None);
    }
}
