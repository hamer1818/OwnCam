// Arka plan efektleri - tam cozunurlukte, ekran kartinda.
//
// Kare hic islemciye inmeden isleniyor: cozucuden gelen RGBA dogrudan
// tampona yukleniyor, ag 256x256 maskeyi uretiyor, kompozit tam cozunurlukte
// birlestiriyor. Islemciye yalnizca bitmis kare geri okunuyor.
//
// Bulanik arka plan **cyrek cozunurlukte** hesaplaniyor: ayrilabilir Gauss
// iki gecisle, sonra kompozitte iki dogrusal buyutuluyor. Tam cozunurlukte
// buyuk yaricapli bulanik 16 kat daha pahali ve gozle ayirt edilmiyor.

struct Params {
    width: u32,
    height: u32,
    small_w: u32,
    small_h: u32,
    mask_off: u32,       // arena icinde maskenin yeri
    mask_w: u32,
    mask_h: u32,
    mode: u32,           // 0 kapali, 1 bulanik, 2 duz renk, 3 foto
    input_off: u32,      // arena icinde agin girdisi
    net_w: u32,
    net_h: u32,
    bg_w: u32,
    bg_h: u32,
    preview_w: u32,
    preview_h: u32,
    blur_radius: u32,
    color: vec4<f32>,
    // Maske kenarini sertlestirme: 0 ham maske, buyudukce daha keskin gecis.
    sharpness: f32,
    // Arenadaki calisma alani: maske kapsami buraya yaziliyor.
    coverage_off: u32,
    // `out_buf` icinde YUV420 duzleminin basladigi kelime indeksi. RGBA
    // bolgesinden sonra geliyor, bu yuzden ustune yazma riski yok.
    yuv_word_off: u32,
    // Arenada agin on plan tahmini (RVM `fgr`); 0 ise yok. Tam cozunurlukte
    // NCHW duruyor, yani kare indeksiyle dogrudan adreslenebiliyor.
    fgr_off: u32,
};

@group(0) @binding(0) var<storage, read_write> arena: array<f32>;
@group(0) @binding(1) var<storage, read> frame: array<u32>;
@group(0) @binding(2) var<storage, read_write> out_buf: array<u32>;
@group(0) @binding(3) var<storage, read_write> blur_a: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> blur_b: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read> background: array<u32>;
@group(0) @binding(6) var<storage, read_write> preview: array<u32>;
@group(0) @binding(7) var<uniform> p: Params;

fn unpack(v: u32) -> vec4<f32> {
    return vec4<f32>(
        f32(v & 0xffu),
        f32((v >> 8u) & 0xffu),
        f32((v >> 16u) & 0xffu),
        f32((v >> 24u) & 0xffu),
    ) / 255.0;
}

fn pack(c: vec4<f32>) -> u32 {
    let q = vec4<u32>(clamp(c, vec4<f32>(0.0), vec4<f32>(1.0)) * 255.0 + 0.5);
    return q.x | (q.y << 8u) | (q.z << 16u) | (q.w << 24u);
}

/// `frame` icinden iki dogrusal ornekleme (0..1 doku koordinati).
fn sample_frame(u: f32, v: f32) -> vec4<f32> {
    let fx = clamp(u * f32(p.width) - 0.5, 0.0, f32(p.width - 1u));
    let fy = clamp(v * f32(p.height) - 0.5, 0.0, f32(p.height - 1u));
    let x0 = u32(floor(fx));
    let y0 = u32(floor(fy));
    let x1 = min(x0 + 1u, p.width - 1u);
    let y1 = min(y0 + 1u, p.height - 1u);
    let tx = fx - floor(fx);
    let ty = fy - floor(fy);
    let c00 = unpack(frame[y0 * p.width + x0]);
    let c01 = unpack(frame[y0 * p.width + x1]);
    let c10 = unpack(frame[y1 * p.width + x0]);
    let c11 = unpack(frame[y1 * p.width + x1]);
    return mix(mix(c00, c01, tx), mix(c10, c11, tx), ty);
}

// ---- 1) kareyi agin girdisine cevir (NCHW f32, 256x256, 0..1) -----------

@compute @workgroup_size(64)
fn to_network_input(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let total = p.net_w * p.net_h;
    if (idx >= total) { return; }
    let x = idx % p.net_w;
    let y = idx / p.net_w;
    // Ag en-boy oranini korumadan 256x256'ya sikistirilmis goruntu bekliyor;
    // referans cikti da boyle uretildi.
    let c = sample_frame((f32(x) + 0.5) / f32(p.net_w), (f32(y) + 0.5) / f32(p.net_h));
    arena[p.input_off + 0u * total + idx] = c.r;
    arena[p.input_off + 1u * total + idx] = c.g;
    arena[p.input_off + 2u * total + idx] = c.b;
}

// ---- 2) bulanik arka plan (ceyrek cozunurluk, ayrilabilir Gauss) --------

@compute @workgroup_size(64)
fn shrink(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= p.small_w * p.small_h) { return; }
    let x = idx % p.small_w;
    let y = idx / p.small_w;
    blur_a[idx] = sample_frame(
        (f32(x) + 0.5) / f32(p.small_w),
        (f32(y) + 0.5) / f32(p.small_h),
    );
}

fn blur_pass(idx: u32, horizontal: bool) -> vec4<f32> {
    let x = i32(idx % p.small_w);
    let y = i32(idx / p.small_w);
    let r = i32(p.blur_radius);
    // sigma = r/2 -> pencerenin kenarinda agirlik ~%1'e iniyor
    let sigma = max(f32(r) * 0.5, 0.5);
    var sum = vec4<f32>(0.0);
    var wsum = 0.0;
    for (var i = -r; i <= r; i = i + 1) {
        let w = exp(-0.5 * f32(i * i) / (sigma * sigma));
        var sx = x;
        var sy = y;
        if (horizontal) {
            sx = clamp(x + i, 0, i32(p.small_w) - 1);
        } else {
            sy = clamp(y + i, 0, i32(p.small_h) - 1);
        }
        let at = u32(sy) * p.small_w + u32(sx);
        if (horizontal) {
            sum = sum + blur_a[at] * w;
        } else {
            sum = sum + blur_b[at] * w;
        }
        wsum = wsum + w;
    }
    return sum / wsum;
}

@compute @workgroup_size(64)
fn blur_h(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= p.small_w * p.small_h) { return; }
    blur_b[idx] = blur_pass(idx, true);
}

@compute @workgroup_size(64)
fn blur_v(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= p.small_w * p.small_h) { return; }
    blur_a[idx] = blur_pass(idx, false);
}

// ---- 3) kompozit -------------------------------------------------------

/// Maskeyi tam cozunurlukte iki dogrusal ornekle. 256x256'lik maske
/// dogrudan buyutulunce kenarlar basamakli cikiyor; ara deger sart.
fn sample_mask(u: f32, v: f32) -> f32 {
    let fx = clamp(u * f32(p.mask_w) - 0.5, 0.0, f32(p.mask_w - 1u));
    let fy = clamp(v * f32(p.mask_h) - 0.5, 0.0, f32(p.mask_h - 1u));
    let x0 = u32(floor(fx));
    let y0 = u32(floor(fy));
    let x1 = min(x0 + 1u, p.mask_w - 1u);
    let y1 = min(y0 + 1u, p.mask_h - 1u);
    let tx = fx - floor(fx);
    let ty = fy - floor(fy);
    let m00 = arena[p.mask_off + y0 * p.mask_w + x0];
    let m01 = arena[p.mask_off + y0 * p.mask_w + x1];
    let m10 = arena[p.mask_off + y1 * p.mask_w + x0];
    let m11 = arena[p.mask_off + y1 * p.mask_w + x1];
    return mix(mix(m00, m01, tx), mix(m10, m11, tx), ty);
}

fn sample_blur(u: f32, v: f32) -> vec4<f32> {
    let fx = clamp(u * f32(p.small_w) - 0.5, 0.0, f32(p.small_w - 1u));
    let fy = clamp(v * f32(p.small_h) - 0.5, 0.0, f32(p.small_h - 1u));
    let x0 = u32(floor(fx));
    let y0 = u32(floor(fy));
    let x1 = min(x0 + 1u, p.small_w - 1u);
    let y1 = min(y0 + 1u, p.small_h - 1u);
    let tx = fx - floor(fx);
    let ty = fy - floor(fy);
    let c00 = blur_a[y0 * p.small_w + x0];
    let c01 = blur_a[y0 * p.small_w + x1];
    let c10 = blur_a[y1 * p.small_w + x0];
    let c11 = blur_a[y1 * p.small_w + x1];
    return mix(mix(c00, c01, tx), mix(c10, c11, tx), ty);
}

/// Foto arka plani kareyi **kaplayacak** sekilde olcekle (en-boy korunur,
/// tasan kisim kirpilir). Sigdirmak siyah kenar birakirdi.
fn sample_background(u: f32, v: f32) -> vec4<f32> {
    if (p.bg_w == 0u || p.bg_h == 0u) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }
    let frame_aspect = f32(p.width) / f32(p.height);
    let bg_aspect = f32(p.bg_w) / f32(p.bg_h);
    var su = u;
    var sv = v;
    if (bg_aspect > frame_aspect) {
        // Arka plan daha genis: yanlardan kirp
        let scale = frame_aspect / bg_aspect;
        su = 0.5 + (u - 0.5) * scale;
    } else {
        let scale = bg_aspect / frame_aspect;
        sv = 0.5 + (v - 0.5) * scale;
    }
    let x = u32(clamp(su * f32(p.bg_w), 0.0, f32(p.bg_w - 1u)));
    let y = u32(clamp(sv * f32(p.bg_h), 0.0, f32(p.bg_h - 1u)));
    return unpack(background[y * p.bg_w + x]);
}

@compute @workgroup_size(64)
fn composite(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= p.width * p.height) { return; }
    let x = idx % p.width;
    let y = idx / p.width;
    let u = (f32(x) + 0.5) / f32(p.width);
    let v = (f32(y) + 0.5) / f32(p.height);

    let src = unpack(frame[idx]);
    if (p.mode == 0u) {
        out_buf[idx] = pack(src);
        return;
    }

    // On plan rengi: ag veriyorsa onu kullan. Kameranin gordugu piksel yari
    // saydam kenarda eski arka planin rengini de tasiyor; `fgr` ayiklanmis
    // hali. Yeni arka planla harmanlanan sey bu olmali.
    var fg = src.rgb;
    if (p.fgr_off != 0u) {
        let n = p.width * p.height;
        fg = vec3<f32>(
            arena[p.fgr_off + idx],
            arena[p.fgr_off + n + idx],
            arena[p.fgr_off + 2u * n + idx],
        );
    }

    var m = sample_mask(u, v);
    // Kenar sertligi: 0'da ham maske, buyudukce 0.5 esigi cevresinde
    // daha dar bir gecis. Sac gibi ince yapilarda ham maske daha iyi.
    if (p.sharpness > 0.0) {
        let half_width = mix(0.5, 0.02, clamp(p.sharpness, 0.0, 1.0));
        m = smoothstep(0.5 - half_width, 0.5 + half_width, m);
    }

    var bg = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    if (p.mode == 1u) {
        bg = sample_blur(u, v);
    } else if (p.mode == 2u) {
        bg = p.color;
    } else if (p.mode == 3u) {
        bg = sample_background(u, v);
    }

    out_buf[idx] = pack(vec4<f32>(mix(bg.rgb, fg, m), 1.0));
}

// ---- 4) maske kapsami --------------------------------------------------
//
// Modelin kisiyi bulup bulamadigini arayuze bildirmek icin maskenin
// ortalamasi. Tek is grubu yetiyor: 256x256 = 65536 deger.
//
// Sonuc arenanin sonundaki calisma alanina yaziliyor; ayri bir depolama
// tamponu baglamak gerekmiyor (kompozit zaten sinirdaki 8 tamponu kullaniyor).

var<workgroup> kapsam_kismi: array<f32, 64>;

@compute @workgroup_size(64)
fn mask_coverage(@builtin(local_invocation_id) lid: vec3<u32>) {
    let n = p.mask_w * p.mask_h;
    var sum = 0.0;
    for (var i = lid.x; i < n; i = i + 64u) {
        sum = sum + arena[p.mask_off + i];
    }
    kapsam_kismi[lid.x] = sum;
    workgroupBarrier();

    for (var stride = 32u; stride > 0u; stride = stride >> 1u) {
        if (lid.x < stride) {
            kapsam_kismi[lid.x] = kapsam_kismi[lid.x] + kapsam_kismi[lid.x + stride];
        }
        workgroupBarrier();
    }

    if (lid.x == 0u) {
        arena[p.coverage_off] = kapsam_kismi[0] / f32(n);
    }
}

// ---- 5) RGBA -> YUV420 (planar) ---------------------------------------
//
// Sanal kameraya YUV420 gidiyor. Kompozit ciktisini burada cevirince iki sey
// kazaniliyor: geri okunan bayt **2,7 kat** aza iniyor (piksel basina 4 ->
// 1,5) ve ikinci ffmpeg donusum yapmak yerine kareyi oldugu gibi geciriyor.
//
// Yeni bir depolama tamponu baglanmiyor - kompozit zaten sinirdaki 8 tamponu
// kullaniyor. YUV, `out_buf`in RGBA bolgesinden **sonrasina** yaziliyor.
//
// Her is parcacigi tam bir u32 (4 bayt) uretiyor; boylece bayt yazarken
// okuma-degistirme-yazma yarisi olusmuyor. Bu, genislige `% 4 == 0` sarti
// getiriyor - saglanmazsa Rust tarafi RGBA yoluna dusuyor.

/// BT.601 sinirli aralik - ffmpeg'in `rgba -> yuv420p` varsayilaniyla ayni,
/// boylece renkler bu gecisten once ve sonra birebir ayni kaliyor.
fn rgb_to_y(c: vec3<f32>) -> f32 {
    return 16.0 + 65.481 * c.r + 128.553 * c.g + 24.966 * c.b;
}
fn rgb_to_u(c: vec3<f32>) -> f32 {
    return 128.0 - 37.797 * c.r - 74.203 * c.g + 112.0 * c.b;
}
fn rgb_to_v(c: vec3<f32>) -> f32 {
    return 128.0 + 112.0 * c.r - 93.786 * c.g - 18.214 * c.b;
}

fn pack_byte(packed: u32, value: f32, slot: u32) -> u32 {
    return packed | (u32(clamp(value, 0.0, 255.0) + 0.5) << (slot * 8u));
}

@compute @workgroup_size(64)
fn to_yuv_luma(@builtin(global_invocation_id) gid: vec3<u32>) {
    let word = gid.x;
    if (word >= (p.width * p.height) / 4u) { return; }
    // Genislik 4'un kati oldugundan bu dort piksel hep ayni satirda.
    let base = word * 4u;
    var packed = 0u;
    for (var k = 0u; k < 4u; k = k + 1u) {
        packed = pack_byte(packed, rgb_to_y(unpack(out_buf[base + k]).rgb), k);
    }
    out_buf[p.yuv_word_off + word] = packed;
}

@compute @workgroup_size(64)
fn to_yuv_chroma(@builtin(global_invocation_id) gid: vec3<u32>) {
    let uv_w = p.width / 2u;
    let uv_h = p.height / 2u;
    let plane_words = (uv_w * uv_h) / 4u;
    let word = gid.x;
    if (word >= plane_words * 2u) { return; }

    // Ilk yari U duzlemi, ikinci yari V duzlemi.
    let is_v = word >= plane_words;
    let local = word - select(0u, plane_words, is_v);
    let base = local * 4u;

    var packed = 0u;
    for (var k = 0u; k < 4u; k = k + 1u) {
        let i = base + k;
        let ux = i % uv_w;
        let uy = i / uv_w;
        // 2x2 blogun ortalamasi - 4:2:0 alt orneklemesi.
        var acc = vec3<f32>(0.0);
        for (var dy = 0u; dy < 2u; dy = dy + 1u) {
            for (var dx = 0u; dx < 2u; dx = dx + 1u) {
                acc = acc + unpack(out_buf[(uy * 2u + dy) * p.width + ux * 2u + dx]).rgb;
            }
        }
        acc = acc * 0.25;
        packed = pack_byte(packed, select(rgb_to_u(acc), rgb_to_v(acc), is_v), k);
    }

    let luma_words = (p.width * p.height) / 4u;
    out_buf[p.yuv_word_off + luma_words + word] = packed;
}

// ---- 6) onizleme icin kucult ------------------------------------------

@compute @workgroup_size(64)
fn preview_shrink(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if (idx >= p.preview_w * p.preview_h) { return; }
    let x = idx % p.preview_w;
    let y = idx / p.preview_w;
    // Kaynak/hedef orani tam sayi olmadigi icin kutu ortalamasi yerine
    // en yakin dort komsunun ortalamasi yeterli: onizleme kucuk.
    let sx = (f32(x) + 0.5) / f32(p.preview_w) * f32(p.width);
    let sy = (f32(y) + 0.5) / f32(p.preview_h) * f32(p.height);
    let x0 = u32(clamp(sx - 0.5, 0.0, f32(p.width - 1u)));
    let y0 = u32(clamp(sy - 0.5, 0.0, f32(p.height - 1u)));
    let x1 = min(x0 + 1u, p.width - 1u);
    let y1 = min(y0 + 1u, p.height - 1u);
    let c = (unpack(out_buf[y0 * p.width + x0])
           + unpack(out_buf[y0 * p.width + x1])
           + unpack(out_buf[y1 * p.width + x0])
           + unpack(out_buf[y1 * p.width + x1])) * 0.25;
    preview[idx] = pack(c);
}
