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
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
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
            let iy = i32(oy * p.stride + ky) - i32(p.pad_t);
            if (iy < 0 || iy >= i32(p.in_h)) { continue; }
            for (var kx = 0u; kx < p.kw; kx = kx + 1u) {
                let ix = i32(ox * p.stride + kx) - i32(p.pad_l);
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

@compute @workgroup_size(64)
fn reduce_mean(@builtin(global_invocation_id) gid: vec3<u32>) {
    let oc = gid.x;
    if (oc >= p.out_c) { return; }
    let n = p.in_h * p.in_w;
    let base = p.a + oc * n;
    var sum = 0.0;
    for (var i = 0u; i < n; i = i + 1u) {
        sum = sum + arena[base + i];
    }
    arena[p.out + oc] = sum / f32(n);
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
fn add(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= out_total()) { return; }
    arena[p.out + idx] = arena[p.a + idx] + arena[p.b + idx];
}

/// [1,C,H,W] * [1,C,1,1] - sikistir-uyar blogunun kanal agirliklandirmasi.
@compute @workgroup_size(64)
fn mul_channel(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= out_total()) { return; }
    let oc = idx / (p.out_w * p.out_h);
    arena[p.out + idx] = arena[p.a + idx] * arena[p.b + oc];
}
