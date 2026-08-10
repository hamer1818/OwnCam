//! Asgari ONNX okuyucu - yalnizca bu modelin ihtiyac duydugu kadari.
//!
//! Neden kutuphane yok: `tract` ya da `ort` eklemek ikiliye 30 MB'tan fazla
//! bindiriyor (olctuk: tract ile 35 MB) ve ikisi de islemcide calisiyor.
//! Protobuf tel bicimi kendini tarif ediyor - her alan bir varint etiket ve
//! ardindan yuku - bu yuzden sema uretimi olmadan guvenle gezilebiliyor.
//! Okunan agirliklarin dogrulugu yuklemede denetleniyor: sekil carpimi ile
//! ham bayt uzunlugu tutmazsa dosya reddediliyor.

use std::collections::HashMap;

const VARINT: u8 = 0;
const LEN: u8 = 2;
const I32: u8 = 5;
const I64: u8 = 1;

pub const DT_FLOAT: i32 = 1;
pub const DT_INT64: i32 = 7;

#[derive(Debug)]
pub struct Tensor {
    pub dims: Vec<usize>,
    pub dtype: i32,
    pub raw: Vec<u8>,
}

impl Tensor {
    pub fn len(&self) -> usize {
        self.dims.iter().product()
    }

    pub fn floats(&self) -> Vec<f32> {
        self.raw
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }

    pub fn i64s(&self) -> Vec<i64> {
        self.raw
            .chunks_exact(8)
            .map(|c| i64::from_le_bytes(c.try_into().unwrap()))
            .collect()
    }
}

#[derive(Debug, Default)]
pub struct Attr {
    pub name: String,
    pub i: i64,
    pub f: f32,
    pub s: String,
    pub ints: Vec<i64>,
    pub floats: Vec<f32>,
    /// `Constant` dugumlerinin tasidigi tensor (AttributeProto alan 5).
    pub tensor: Option<Tensor>,
}

#[derive(Debug)]
pub struct Node {
    pub op: String,
    pub input: Vec<String>,
    pub output: Vec<String>,
    pub attrs: Vec<Attr>,
}

impl Node {
    pub fn attr(&self, name: &str) -> Option<&Attr> {
        self.attrs.iter().find(|a| a.name == name)
    }

    pub fn ints(&self, name: &str) -> &[i64] {
        self.attr(name).map(|a| a.ints.as_slice()).unwrap_or(&[])
    }

    pub fn int(&self, name: &str, default: i64) -> i64 {
        self.attr(name).map(|a| a.i).unwrap_or(default)
    }

    pub fn text(&self, name: &str) -> &str {
        self.attr(name).map(|a| a.s.as_str()).unwrap_or("")
    }

    pub fn floats(&self, name: &str) -> &[f32] {
        self.attr(name).map(|a| a.floats.as_slice()).unwrap_or(&[])
    }

    /// `Constant` dugumunun degeri.
    pub fn tensor(&self, name: &str) -> Option<&Tensor> {
        self.attr(name).and_then(|a| a.tensor.as_ref())
    }
}

#[derive(Debug)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub init: HashMap<String, Tensor>,
    pub input: String,
    pub output: String,
}

// ---- protobuf tel bicimi -----------------------------------------------

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

enum Val<'a> {
    Num(u64),
    Bytes(&'a [u8]),
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn varint(&mut self) -> Option<u64> {
        let (mut v, mut shift) = (0u64, 0u32);
        loop {
            let byte = *self.buf.get(self.pos)?;
            self.pos += 1;
            v |= ((byte & 0x7F) as u64) << shift;
            if byte & 0x80 == 0 {
                return Some(v);
            }
            shift += 7;
            if shift > 63 {
                return None;
            }
        }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let out = self.buf.get(self.pos..self.pos + n)?;
        self.pos += n;
        Some(out)
    }

    /// Sonraki alani oku: (alan_no, deger).
    fn next(&mut self) -> Option<(u64, Val<'a>)> {
        if self.pos >= self.buf.len() {
            return None;
        }
        let tag = self.varint()?;
        let (field, wire) = (tag >> 3, (tag & 7) as u8);
        let val = match wire {
            VARINT => Val::Num(self.varint()?),
            I64 => Val::Bytes(self.take(8)?),
            LEN => {
                let n = self.varint()? as usize;
                Val::Bytes(self.take(n)?)
            }
            I32 => Val::Bytes(self.take(4)?),
            _ => return None,
        };
        Some((field, val))
    }
}

fn packed_varints(buf: &[u8]) -> Vec<i64> {
    let mut r = Reader::new(buf);
    let mut out = Vec::new();
    while let Some(v) = r.varint() {
        out.push(v as i64);
    }
    out
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

// ---- ONNX yapilari ------------------------------------------------------
// ModelProto: graph=7
// GraphProto: node=1 initializer=5 input=11 output=12
// NodeProto:  input=1 output=2 name=3 op_type=4 attribute=5
// TensorProto: dims=1 data_type=2 name=8 raw_data=9
// AttributeProto: name=1 f=2 i=3 s=4 ints=8

fn parse_tensor(buf: &[u8]) -> Tensor {
    let mut t = Tensor {
        dims: Vec::new(),
        dtype: 0,
        raw: Vec::new(),
    };
    let mut name = String::new();
    let mut r = Reader::new(buf);
    while let Some((field, val)) = r.next() {
        match (field, val) {
            (1, Val::Num(v)) => t.dims.push(v as usize),
            (1, Val::Bytes(b)) => t.dims.extend(packed_varints(b).iter().map(|v| *v as usize)),
            (2, Val::Num(v)) => t.dtype = v as i32,
            // ONNX sayilari ya `raw_data` (alan 9) ya da tipe ozel paketli
            // alanlarda tasiyor. Ikisini de ham bayta indiriyoruz ki geri
            // kalan kod tek bir yol gorsun.
            (4, Val::Bytes(b)) => t.raw.extend_from_slice(b),
            (7, Val::Bytes(b)) => {
                for v in packed_varints(b) {
                    t.raw.extend_from_slice(&v.to_le_bytes());
                }
            }
            (8, Val::Bytes(b)) => name = text(b),
            (9, Val::Bytes(b)) => t.raw = b.to_vec(),
            _ => {}
        }
    }
    let _ = name;
    t
}

fn parse_tensor_named(buf: &[u8]) -> (String, Tensor) {
    let mut name = String::new();
    let mut r = Reader::new(buf);
    while let Some((field, val)) = r.next() {
        if let (8, Val::Bytes(b)) = (field, &val) {
            name = text(b);
        }
    }
    (name, parse_tensor(buf))
}

fn parse_attr(buf: &[u8]) -> Attr {
    let mut a = Attr::default();
    let mut r = Reader::new(buf);
    while let Some((field, val)) = r.next() {
        match (field, val) {
            (1, Val::Bytes(b)) => a.name = text(b),
            (2, Val::Bytes(b)) if b.len() == 4 => {
                a.f = f32::from_le_bytes(b.try_into().unwrap())
            }
            (3, Val::Num(v)) => a.i = v as i64,
            (4, Val::Bytes(b)) => a.s = text(b),
            (5, Val::Bytes(b)) => a.tensor = Some(parse_tensor(b)),
            (7, Val::Bytes(b)) => {
                a.floats = b
                    .chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect()
            }
            (8, Val::Num(v)) => a.ints.push(v as i64),
            (8, Val::Bytes(b)) => a.ints.extend(packed_varints(b)),
            _ => {}
        }
    }
    a
}

fn parse_node(buf: &[u8]) -> Node {
    let mut n = Node {
        op: String::new(),
        input: Vec::new(),
        output: Vec::new(),
        attrs: Vec::new(),
    };
    let mut r = Reader::new(buf);
    while let Some((field, val)) = r.next() {
        match (field, val) {
            (1, Val::Bytes(b)) => n.input.push(text(b)),
            (2, Val::Bytes(b)) => n.output.push(text(b)),
            (4, Val::Bytes(b)) => n.op = text(b),
            (5, Val::Bytes(b)) => n.attrs.push(parse_attr(b)),
            _ => {}
        }
    }
    n
}

/// ValueInfoProto: name=1
fn value_name(buf: &[u8]) -> String {
    let mut r = Reader::new(buf);
    while let Some((field, val)) = r.next() {
        if let (1, Val::Bytes(b)) = (field, val) {
            return text(b);
        }
    }
    String::new()
}

pub fn parse(bytes: &[u8]) -> Result<Graph, String> {
    let mut graph_buf: Option<&[u8]> = None;
    let mut r = Reader::new(bytes);
    while let Some((field, val)) = r.next() {
        if let (7, Val::Bytes(b)) = (field, val) {
            graph_buf = Some(b);
        }
    }
    let gb = graph_buf.ok_or("ONNX dosyasinda graph yok")?;

    let mut nodes = Vec::new();
    let mut init = HashMap::new();
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();

    let mut r = Reader::new(gb);
    while let Some((field, val)) = r.next() {
        match (field, val) {
            (1, Val::Bytes(b)) => nodes.push(parse_node(b)),
            (5, Val::Bytes(b)) => {
                let (name, t) = parse_tensor_named(b);
                init.insert(name, t);
            }
            (11, Val::Bytes(b)) => inputs.push(value_name(b)),
            (12, Val::Bytes(b)) => outputs.push(value_name(b)),
            _ => {}
        }
    }

    // Baslaticilar da `input` listesinde gorunebiliyor; gercek girdi
    // baslatici olmayanidir.
    let input = inputs
        .into_iter()
        .find(|n| !init.contains_key(n))
        .ok_or("ONNX grafinda girdi bulunamadi")?;
    let output = outputs.into_iter().next().ok_or("ONNX grafinda cikti yok")?;

    // Agirliklarin butunlugu: sekil carpimi ham bayt uzunlugunu vermeli.
    // Ayristirma kaymissa burada patlar, sessizce yanlis sonuc uretmez.
    for (name, t) in &init {
        let want = match t.dtype {
            DT_FLOAT => t.len() * 4,
            DT_INT64 => t.len() * 8,
            other => return Err(format!("{name}: desteklenmeyen tip {other}")),
        };
        if want != t.raw.len() {
            return Err(format!(
                "{name}: sekil {:?} {} bayt bekliyor, dosyada {} var",
                t.dims,
                want,
                t.raw.len()
            ));
        }
    }

    if nodes.is_empty() {
        return Err("ONNX grafinda dugum yok".into());
    }
    Ok(Graph {
        nodes,
        init,
        input,
        output,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODEL: &[u8] = include_bytes!("../../assets/selfie_segmentation.onnx");

    #[test]
    fn model_ayristirilir() {
        let g = parse(MODEL).expect("model ayristirilmali");
        assert_eq!(g.input, "pixel_values");
        assert_eq!(g.output, "alphas");
        assert_eq!(g.nodes.len(), 145);
        assert_eq!(g.init.len(), 115);
    }

    /// Ilk evrisim: 3->16 kanal, 3x3, adim 2, TF-SAME dolgusu (sag/alt 1).
    #[test]
    fn ilk_evrisim_beklendigi_gibi() {
        let g = parse(MODEL).unwrap();
        let n = &g.nodes[0];
        assert_eq!(n.op, "Conv");
        assert_eq!(n.ints("kernel_shape"), [3, 3]);
        assert_eq!(n.ints("strides"), [2, 2]);
        assert_eq!(n.ints("pads"), [0, 0, 1, 1]);
        assert_eq!(n.int("group", 1), 1);
        let w = &g.init["conv1.weight"];
        assert_eq!(w.dims, vec![16, 3, 3, 3]);
    }

    /// Derinlemesine evrisim `group == kanal sayisi` ile gosteriliyor.
    #[test]
    fn derinlemesine_evrisim_gruplu() {
        let g = parse(MODEL).unwrap();
        let n = g.nodes.iter().find(|n| n.int("group", 1) > 1).unwrap();
        assert_eq!(n.op, "Conv");
        let w = &g.init[&n.input[1]];
        assert_eq!(w.dims[1], 1, "gruplu evrisimde cekirdek derinligi 1 olmali");
    }

    /// Cozucudeki olcek buyutme half_pixel; TFLite surumunden farkli.
    #[test]
    fn resize_half_pixel() {
        let g = parse(MODEL).unwrap();
        let rs: Vec<_> = g.nodes.iter().filter(|n| n.op == "Resize").collect();
        assert_eq!(rs.len(), 3);
        for n in rs {
            assert_eq!(n.text("coordinate_transformation_mode"), "half_pixel");
            assert_eq!(n.text("mode"), "linear");
        }
    }

    /// Son katman 2x2 adim-2 transpoze evrisim, agirlik [C_in, C_out, kh, kw].
    #[test]
    fn transpoze_evrisim_duzeni() {
        let g = parse(MODEL).unwrap();
        let n = g.nodes.iter().find(|n| n.op == "ConvTranspose").unwrap();
        assert_eq!(n.ints("strides"), [2, 2]);
        let w = &g.init[&n.input[1]];
        assert_eq!(w.dims, vec![16, 1, 2, 2]);
    }
}
