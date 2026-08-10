// Segmentasyon aginin GPU cekirdekleri.
//
// Yerlesim NCHW: bir tensor arena icinde [kanal][satir][sutun] sirasiyla
// duz duruyor. Butun ara tensorler tek bir `arena` tamponunu paylasiyor,
// butun agirliklar tek bir `weights` tamponunda; her sevk yalnizca kayma
// tasiyor. Boylece 136 adim icin tek baglama grubu yetiyor, kare basina
// baglama grubu kurulmuyor.
//
// Her is parcacigi bir cikti elemani hesapliyor. Ag kucuk (~100 MFLOP), bu
// yuzden karo/paylasimli bellek eniyilemesi yapilmadi: olculen sure zaten
// hedefin cok altinda.

struct Params {
    a: u32,          // birinci girdi kaymasi (arena)
    b: u32,          // ikinci girdi kaymasi (arena)
    out: u32,        // cikti kaymasi (arena)
    weight: u32,     // cekirdek kaymasi (weights)
    bias: u32,       // yanlilik kaymasi (weights)
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
    b_c: u32,      // ikinci islenenin sekli; yayin bunun uzerinden
    b_h: u32,
    b_w: u32,
    alpha: f32,    // HardSigmoid egimi / Clip alt siniri
    beta: f32,     // HardSigmoid kaymasi / Clip ust siniri
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var<storage, read_write> arena: array<f32>;
@group(0) @binding(1) var<storage, read> weights: array<f32>;
@group(0) @binding(2) var<uniform> p: Params;

fn out_total() -> u32 {
    return p.out_c * p.out_h * p.out_w;
}

// ---- evrisim (gruplu; group == in_c oldugunda derinlemesine) -------------

@compute @workgroup_size(64)
fn conv(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= out_total()) { return; }

    let ox = idx % p.out_w;
    let oy = (idx / p.out_w) % p.out_h;
    let oc = idx / (p.out_w * p.out_h);

    let ic_per_group = p.in_c / p.groups;
    let oc_per_group = p.out_c / p.groups;
    let ic0 = (oc / oc_per_group) * ic_per_group;

    // Cekirdek duzeni [out_c, in_c/groups, kh, kw]
    let wbase = p.weight + oc * ic_per_group * p.kh * p.kw;

    var acc = weights[p.bias + oc];
    for (var ic = 0u; ic < ic_per_group; ic = ic + 1u) {
        let in_base = p.a + (ic0 + ic) * p.in_h * p.in_w;
        let w_base = wbase + ic * p.kh * p.kw;
        for (var ky = 0u; ky < p.kh; ky = ky + 1u) {
            let iy = i32(oy * p.stride + ky * p.dilation) - i32(p.pad_t);
            if (iy < 0 || iy >= i32(p.in_h)) { continue; }
            for (var kx = 0u; kx < p.kw; kx = kx + 1u) {
                let ix = i32(ox * p.stride + kx * p.dilation) - i32(p.pad_l);
                if (ix < 0 || ix >= i32(p.in_w)) { continue; }
                acc = acc
                    + arena[in_base + u32(iy) * p.in_w + u32(ix)]
                    * weights[w_base + ky * p.kw + kx];
            }
        }
    }
    arena[p.out + idx] = acc;
}

// ---- transpoze evrisim (cekirdek [in_c, out_c/groups, kh, kw]) -----------

@compute @workgroup_size(64)
fn conv_transpose(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= out_total()) { return; }

    let ox = idx % p.out_w;
    let oy = (idx / p.out_w) % p.out_h;
    let oc = idx / (p.out_w * p.out_h);

    var acc = weights[p.bias + oc];
    for (var ky = 0u; ky < p.kh; ky = ky + 1u) {
        let ty = i32(oy) + i32(p.pad_t) - i32(ky);
        if (ty < 0 || ty % i32(p.stride) != 0) { continue; }
        let iy = ty / i32(p.stride);
        if (iy >= i32(p.in_h)) { continue; }
        for (var kx = 0u; kx < p.kw; kx = kx + 1u) {
            let tx = i32(ox) + i32(p.pad_l) - i32(kx);
            if (tx < 0 || tx % i32(p.stride) != 0) { continue; }
            let ix = tx / i32(p.stride);
            if (ix >= i32(p.in_w)) { continue; }
            for (var ic = 0u; ic < p.in_c; ic = ic + 1u) {
                let w = weights[p.weight + ((ic * p.out_c + oc) * p.kh + ky) * p.kw + kx];
                acc = acc + arena[p.a + (ic * p.in_h + u32(iy)) * p.in_w + u32(ix)] * w;
            }
        }
    }
    arena[p.out + idx] = acc;
}

// ---- olcek buyutme: iki dogrusal, half_pixel ----------------------------

@compute @workgroup_size(64)
fn resize(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= out_total()) { return; }

    let ox = idx % p.out_w;
    let oy = (idx / p.out_w) % p.out_h;
    let oc = idx / (p.out_w * p.out_h);

    // half_pixel: kaynak = (hedef + 0.5) * olcek - 0.5
    let sy = clamp((f32(oy) + 0.5) * (f32(p.in_h) / f32(p.out_h)) - 0.5,
                   0.0, f32(p.in_h - 1u));
    let sx = clamp((f32(ox) + 0.5) * (f32(p.in_w) / f32(p.out_w)) - 0.5,
                   0.0, f32(p.in_w - 1u));

    let y0 = u32(floor(sy));
    let x0 = u32(floor(sx));
    let y1 = min(y0 + 1u, p.in_h - 1u);
    let x1 = min(x0 + 1u, p.in_w - 1u);
    let fy = sy - floor(sy);
    let fx = sx - floor(sx);

    let base = p.a + oc * p.in_h * p.in_w;
    let v00 = arena[base + y0 * p.in_w + x0];
    let v01 = arena[base + y0 * p.in_w + x1];
    let v10 = arena[base + y1 * p.in_w + x0];
    let v11 = arena[base + y1 * p.in_w + x1];

    arena[p.out + idx] = mix(mix(v00, v01, fx), mix(v10, v11, fx), fy);
}

// ---- kuresel ortalama: [1,C,H,W] -> [1,C,1,1] ---------------------------
//
// **Is grubu basina bir kanal.** Onceki surumde kanal basina tek is parcacigi
// vardi ve H*W (128x128'e kadar) seri toplaniyordu; 16-128 parcacikla ekran
// karti neredeyse bos duruyordu. Simdi 64 parcacik once serpistirilmis
// okuyup kismi toplam biriktiriyor, sonra paylasilan bellekte agac indirgeme
// yapiyor.

var<workgroup> partial: array<f32, 64>;

@compute @workgroup_size(64)
fn reduce_mean(
    @builtin(workgroup_id) wg: vec3<u32>,
    @builtin(local_invocation_id) lid: vec3<u32>,
) {
    // Sevk tam olarak kanal sayisi kadar is grubu aciyor, bu yuzden sinir
    // denetimi gerekmiyor - ve gerekmemesi iyi: erken donus, asagidaki
    // bariyerlerin tekduze akista kalmasini bozardi.
    let oc = wg.x;
    let n = p.in_h * p.in_w;
    let base = p.a + oc * n;

    var sum = 0.0;
    for (var i = lid.x; i < n; i = i + 64u) {
        sum = sum + arena[base + i];
    }
    partial[lid.x] = sum;
    workgroupBarrier();

    for (var stride = 32u; stride > 0u; stride = stride >> 1u) {
        if (lid.x < stride) {
            partial[lid.x] = partial[lid.x] + partial[lid.x + stride];
        }
        workgroupBarrier();
    }

    if (lid.x == 0u) {
        arena[p.out + oc] = partial[0] / f32(n);
    }
}

// ---- eleman bazli ------------------------------------------------------

@compute @workgroup_size(64)
fn relu(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= out_total()) { return; }
    arena[p.out + idx] = max(arena[p.a + idx], 0.0);
}

@compute @workgroup_size(64)
fn hard_swish(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= out_total()) { return; }
    let x = arena[p.a + idx];
    arena[p.out + idx] = x * clamp(x + 3.0, 0.0, 6.0) / 6.0;
}

@compute @workgroup_size(64)
fn sigmoid(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= out_total()) { return; }
    arena[p.out + idx] = 1.0 / (1.0 + exp(-arena[p.a + idx]));
}

@compute @workgroup_size(64)
fn tanh_op(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= out_total()) { return; }
    arena[p.out + idx] = tanh(arena[p.a + idx]);
}

@compute @workgroup_size(64)
fn hard_sigmoid(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= out_total()) { return; }
    arena[p.out + idx] = clamp(p.alpha * arena[p.a + idx] + p.beta, 0.0, 1.0);
}

@compute @workgroup_size(64)
fn clip_op(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= out_total()) { return; }
    arena[p.out + idx] = clamp(arena[p.a + idx], p.alpha, p.beta);
}

@compute @workgroup_size(64)
fn copy_op(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= out_total()) { return; }
    arena[p.out + idx] = arena[p.a + idx];
}

/// Uzamsal kirpma; baslangic kosesi pad_t/pad_l.
@compute @workgroup_size(64)
fn crop(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= out_total()) { return; }
    let ox = idx % p.out_w;
    let oy = (idx / p.out_w) % p.out_h;
    let oc = idx / (p.out_w * p.out_h);
    arena[p.out + idx] =
        arena[p.a + (oc * p.in_h + oy + p.pad_t) * p.in_w + ox + p.pad_l];
}

// ---- yayinli ikili islemler ---------------------------------------------
//
// Her eksende islenenin boyutu ya ciktiyla ayni ya da 1. Tek bir cekirdek
// govdesi hem ayni sekilli toplama, hem sikistir-uyar blogunun [1,C,1,1]
// kanal agirliklandirmasini, hem de skaler normalizasyonu karsiliyor.

fn src_index(off: u32, c: u32, h: u32, w: u32, idx: u32) -> u32 {
    let ox = idx % p.out_w;
    let oy = (idx / p.out_w) % p.out_h;
    let oc = idx / (p.out_w * p.out_h);
    let sx = select(ox, 0u, w == 1u);
    let sy = select(oy, 0u, h == 1u);
    let sc = select(oc, 0u, c == 1u);
    return off + (sc * h + sy) * w + sx;
}

fn lhs(idx: u32) -> f32 {
    return arena[src_index(p.a, p.in_c, p.in_h, p.in_w, idx)];
}

fn rhs(idx: u32) -> f32 {
    return arena[src_index(p.b, p.b_c, p.b_h, p.b_w, idx)];
}

@compute @workgroup_size(64)
fn bin_add(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= out_total()) { return; }
    arena[p.out + idx] = lhs(idx) + rhs(idx);
}

@compute @workgroup_size(64)
fn bin_sub(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= out_total()) { return; }
    arena[p.out + idx] = lhs(idx) - rhs(idx);
}

@compute @workgroup_size(64)
fn bin_mul(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= out_total()) { return; }
    arena[p.out + idx] = lhs(idx) * rhs(idx);
}

@compute @workgroup_size(64)
fn bin_div(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= out_total()) { return; }
    arena[p.out + idx] = lhs(idx) / rhs(idx);
}

// ---- havuzlama ----------------------------------------------------------

/// Pencere ortalamasi. `ceil_mode` acikken son pencere kenari tasabiliyor;
/// ONNX'in varsayilani (count_include_pad=0) yalnizca gecerli elemanlari
/// sayiyor, cekirdek de oyle yapiyor.
@compute @workgroup_size(64)
fn avg_pool(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= out_total()) { return; }
    let ox = idx % p.out_w;
    let oy = (idx / p.out_w) % p.out_h;
    let oc = idx / (p.out_w * p.out_h);

    let base = p.a + oc * p.in_h * p.in_w;
    var sum = 0.0;
    var n = 0u;
    for (var ky = 0u; ky < p.kh; ky = ky + 1u) {
        let iy = oy * p.stride + ky;
        if (iy >= p.in_h) { continue; }
        for (var kx = 0u; kx < p.kw; kx = kx + 1u) {
            let ix = ox * p.stride + kx;
            if (ix >= p.in_w) { continue; }
            sum = sum + arena[base + iy * p.in_w + ix];
            n = n + 1u;
        }
    }
    arena[p.out + idx] = sum / f32(n);
}

/// Kanal ekseninde ortalama: [1,C,H,W] -> [1,1,H,W].
@compute @workgroup_size(64)
fn reduce_channel(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= out_total()) { return; }
    let plane = p.in_h * p.in_w;
    var sum = 0.0;
    for (var c = 0u; c < p.in_c; c = c + 1u) {
        sum = sum + arena[p.a + c * plane + idx];
    }
    arena[p.out + idx] = sum / f32(p.in_c);
}
