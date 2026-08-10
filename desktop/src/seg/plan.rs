//! Graftan **calisma plani** cikarma.
//!
//! Model sabit: girdi her zaman 1x3x256x256. Bu yuzden butun sekiller, dolgu
//! degerleri ve tampon kaymalari yuklemede bir kez hesaplaniyor; kare basina
//! is yalnizca hazir parametrelerle sevk etmek oluyor.
//!
//! `Shape`/`Slice`/`Concat` dugumleri yalnizca `Resize`'in hedef boyutunu
//! kuruyor. Bunlar GPU'ya hic gitmiyor: plan kurulurken sayi olarak
//! katlaniyorlar (sabit katlama).

use std::collections::HashMap;

use super::onnx::{self, Graph};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Conv,
    ConvTranspose,
    Relu,
    HardSwish,
    Sigmoid,
    Add,
    /// [1,C,H,W] * [1,C,1,1] - sikistir-uyar blogunun kanal agirliklandirmasi.
    MulChannel,
    /// H ve W uzerinden ortalama -> [1,C,1,1]
    ReduceMean,
    /// Iki dogrusal buyutme, half_pixel koordinat donusumu
    Resize,
}

/// Tek bir sevk. Butun alanlar f32/u32 olarak dogrudan shader'a gidiyor.
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
    pub out_shape: Shape,
    pub kh: u32,
    pub kw: u32,
    pub stride: u32,
    pub pad_t: u32,
    pub pad_l: u32,
    pub group: u32,
}

pub struct Plan {
    pub steps: Vec<Step>,
    pub weights: Vec<f32>,
    pub arena_len: usize,
    /// Agin kullanmadigi, arenanin sonundaki kucuk calisma alani. Kompozit
    /// gecisi maske kapsamini buraya yaziyor; boylece yeni bir depolama
    /// tamponu baglamak gerekmiyor (sinir zaten 8'de).
    pub scratch_off: u32,
    pub input_off: u32,
    /// Planin hangi girdi olcusu icin kuruldugu; teshis icin duruyor.
    #[allow(dead_code)]
    pub input_shape: Shape,
    pub output_off: u32,
    pub output_shape: Shape,
}

/// Modelin egitildigi girdi olcusu. Graftaki sabit `Resize` hedefleri buna
/// gore yazilmis; baska bir olcude calisirken ayni oranda olcekleniyor.
pub const REFERENCE_INPUT: usize = 256;

/// Arenanin sonunda ayrilan calisma alani (f32 cinsinden).
const SCRATCH: usize = 4;

/// Sekil hesaplarinda kullanilan, GPU'ya gitmeyen sabit vektorler.
enum Folded {
    Ints(Vec<i64>),
}

pub fn build(graph: &Graph, input: Shape) -> Result<Plan, String> {
    let mut weights: Vec<f32> = Vec::new();
    // Butun agirliklar tek bir tampona diziliyor; adimlar yalnizca kayma
    // tasiyor. Boylece kare basina tek bir baglama grubu yetiyor.
    let pack = |name: &str,
                weights: &mut Vec<f32>,
                weight_off: &mut HashMap<String, u32>|
     -> Result<u32, String> {
        if let Some(off) = weight_off.get(name) {
            return Ok(*off);
        }
        let t = graph
            .init
            .get(name)
            .ok_or_else(|| format!("agirlik yok: {name}"))?;
        if t.dtype != onnx::DT_FLOAT {
            return Err(format!("{name}: f32 bekleniyordu"));
        }
        let off = weights.len() as u32;
        weights.extend(t.floats());
        weight_off.insert(name.to_string(), off);
        Ok(off)
    };
    let mut weight_off_owned: HashMap<String, u32> = HashMap::new();

    let mut shapes: HashMap<String, Shape> = HashMap::new();
    let mut offsets: HashMap<String, u32> = HashMap::new();
    let mut folded: HashMap<String, Folded> = HashMap::new();
    let mut arena_len: usize = 0;

    let alloc = |shape: Shape, arena_len: &mut usize| -> u32 {
        let off = *arena_len as u32;
        *arena_len += shape.len();
        off
    };

    shapes.insert(graph.input.clone(), input);
    let input_off = alloc(input, &mut arena_len);
    offsets.insert(graph.input.clone(), input_off);

    let mut steps = Vec::new();

    for node in &graph.nodes {
        let out_name = node
            .output
            .first()
            .ok_or_else(|| format!("{}: cikti yok", node.op))?
            .clone();

        // --- yalnizca sekil hesabi yapan dugumler: sabit katlama ---
        match node.op.as_str() {
            "Shape" => {
                let s = shapes
                    .get(&node.input[0])
                    .ok_or_else(|| format!("Shape: {} sekli bilinmiyor", node.input[0]))?;
                folded.insert(
                    out_name,
                    Folded::Ints(vec![1, s.c as i64, s.h as i64, s.w as i64]),
                );
                continue;
            }
            "Slice" => {
                let src = match folded.get(&node.input[0]) {
                    Some(Folded::Ints(v)) => v.clone(),
                    None => return Err("Slice yalnizca sekil vektorlerinde destekleniyor".into()),
                };
                let get = |i: usize| -> Result<i64, String> {
                    let name = node
                        .input
                        .get(i)
                        .ok_or_else(|| format!("Slice: {i}. girdi yok"))?;
                    let t = graph
                        .init
                        .get(name)
                        .ok_or_else(|| format!("Slice: {name} sabit degil"))?;
                    Ok(t.i64s()[0])
                };
                let (start, end) = (get(1)?.max(0) as usize, get(2)?);
                let end = if end < 0 || end as usize > src.len() {
                    src.len()
                } else {
                    end as usize
                };
                folded.insert(out_name, Folded::Ints(src[start..end].to_vec()));
                continue;
            }
            "Concat" => {
                let mut all = Vec::new();
                for name in &node.input {
                    if let Some(Folded::Ints(v)) = folded.get(name) {
                        all.extend(v.iter().copied());
                    } else if let Some(t) = graph.init.get(name) {
                        // Sabit hedef boyutlar 256x256 girdiye gore yazilmis;
                        // baska bir olcude calisirken ayni oranda buyutuluyor.
                        let scale = input.w as f64 / REFERENCE_INPUT as f64;
                        all.extend(
                            t.i64s()
                                .into_iter()
                                .map(|v| (v as f64 * scale).round() as i64),
                        );
                    } else {
                        return Err(format!("Concat: {name} sabit degil"));
                    }
                }
                folded.insert(out_name, Folded::Ints(all));
                continue;
            }
            _ => {}
        }

        // --- gercek hesap dugumleri ---
        let in_name = &node.input[0];
        let in_shape = *shapes
            .get(in_name)
            .ok_or_else(|| format!("{}: {} sekli bilinmiyor", node.op, in_name))?;
        let a = *offsets
            .get(in_name)
            .ok_or_else(|| format!("{}: {} kaymasi yok", node.op, in_name))?;

        let mut step = Step {
            kind: Kind::Relu,
            a,
            b: 0,
            out: 0,
            weight: 0,
            bias: 0,
            in_shape,
            out_shape: in_shape,
            kh: 1,
            kw: 1,
            stride: 1,
            pad_t: 0,
            pad_l: 0,
            group: 1,
        };

        match node.op.as_str() {
            "Conv" | "ConvTranspose" => {
                let w = graph
                    .init
                    .get(&node.input[1])
                    .ok_or_else(|| format!("{}: cekirdek yok", node.op))?;
                let (kh, kw) = (w.dims[2], w.dims[3]);
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
                let group = node.int("group", 1) as usize;

                let out_shape = if node.op == "Conv" {
                    Shape {
                        c: w.dims[0],
                        h: (in_shape.h + pt + pb - kh) / stride + 1,
                        w: (in_shape.w + pl + pr - kw) / stride + 1,
                    }
                } else {
                    Shape {
                        // ConvTranspose cekirdegi [C_in, C_out/group, kh, kw]
                        c: w.dims[1] * group,
                        h: (in_shape.h - 1) * stride + kh - pt - pb,
                        w: (in_shape.w - 1) * stride + kw - pl - pr,
                    }
                };

                step.kind = if node.op == "Conv" {
                    Kind::Conv
                } else {
                    Kind::ConvTranspose
                };
                step.weight = pack(&node.input[1], &mut weights, &mut weight_off_owned)?;
                step.bias = pack(&node.input[2], &mut weights, &mut weight_off_owned)?;
                step.out_shape = out_shape;
                step.kh = kh as u32;
                step.kw = kw as u32;
                step.stride = stride as u32;
                step.pad_t = pt as u32;
                step.pad_l = pl as u32;
                step.group = group as u32;
            }
            "Relu" => step.kind = Kind::Relu,
            "HardSwish" => step.kind = Kind::HardSwish,
            "Sigmoid" => step.kind = Kind::Sigmoid,
            "Add" => {
                step.kind = Kind::Add;
                step.b = *offsets
                    .get(&node.input[1])
                    .ok_or_else(|| format!("Add: {} kaymasi yok", node.input[1]))?;
            }
            "Mul" => {
                let s1 = *shapes
                    .get(&node.input[1])
                    .ok_or_else(|| format!("Mul: {} sekli bilinmiyor", node.input[1]))?;
                // Sikistir-uyar: [1,C,H,W] * [1,C,1,1]. Genis olan girdi `a`.
                let (wide, narrow) = if in_shape.len() >= s1.len() {
                    (in_name.clone(), node.input[1].clone())
                } else {
                    (node.input[1].clone(), in_name.clone())
                };
                let wide_shape = shapes[&wide];
                let narrow_shape = shapes[&narrow];
                if narrow_shape.h != 1 || narrow_shape.w != 1 || narrow_shape.c != wide_shape.c {
                    return Err(format!(
                        "Mul: yalnizca kanal yayini destekleniyor ({wide_shape:?} x {narrow_shape:?})"
                    ));
                }
                step.kind = Kind::MulChannel;
                step.a = offsets[&wide];
                step.b = offsets[&narrow];
                step.in_shape = wide_shape;
                step.out_shape = wide_shape;
            }
            "ReduceMean" => {
                let axes = node.ints("axes");
                if axes != [2, 3] {
                    return Err(format!("ReduceMean: beklenmeyen eksenler {axes:?}"));
                }
                step.kind = Kind::ReduceMean;
                step.out_shape = Shape {
                    c: in_shape.c,
                    h: 1,
                    w: 1,
                };
            }
            "Resize" => {
                if node.text("mode") != "linear"
                    || node.text("coordinate_transformation_mode") != "half_pixel"
                {
                    return Err("Resize: yalnizca half_pixel linear destekleniyor".into());
                }
                let sizes_name = node
                    .input
                    .get(3)
                    .ok_or("Resize: hedef boyut girdisi yok")?
                    .clone();
                let sizes = match folded.get(&sizes_name) {
                    Some(Folded::Ints(v)) => v.clone(),
                    None => graph
                        .init
                        .get(&sizes_name)
                        .map(|t| t.i64s())
                        .ok_or("Resize: hedef boyut sabit degil")?,
                };
                if sizes.len() != 4 {
                    return Err(format!("Resize: hedef boyut {sizes:?}"));
                }
                // Hedef boyut grafta **sabit** (32/64/128) ama uctan uca
                // hepsi tam 2 kat buyutme. Sabiti oldugu gibi kullanmak agi
                // 256x256'ya cakiyor; oysa ag tamamen evrisimli ve carpani
                // korursak her cozunurlukte calisiyor.
                let (th, tw) = (sizes[2] as usize, sizes[3] as usize);
                let oran_h = th as f64 / in_shape.h as f64;
                let oran_w = tw as f64 / in_shape.w as f64;
                if (oran_h - 2.0).abs() > 1e-6 || (oran_w - 2.0).abs() > 1e-6 {
                    return Err(format!(
                        "Resize: yalnizca 2 kat destekleniyor ({}x{} -> {th}x{tw})",
                        in_shape.h, in_shape.w
                    ));
                }
                step.kind = Kind::Resize;
                step.out_shape = Shape {
                    c: in_shape.c,
                    h: in_shape.h * 2,
                    w: in_shape.w * 2,
                };
            }
            other => return Err(format!("desteklenmeyen operator: {other}")),
        }

        step.out = alloc(step.out_shape, &mut arena_len);
        shapes.insert(out_name.clone(), step.out_shape);
        offsets.insert(out_name, step.out);
        steps.push(step);
    }

    let output_off = *offsets
        .get(&graph.output)
        .ok_or_else(|| format!("cikti {} uretilmemis", graph.output))?;
    let output_shape = shapes[&graph.output];

    let scratch_off = arena_len as u32;
    arena_len += SCRATCH;

    Ok(Plan {
        steps,
        weights,
        arena_len,
        scratch_off,
        input_off,
        input_shape: input,
        output_off,
        output_shape,
    })
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
}
