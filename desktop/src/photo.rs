//! Arka plan fotografini okuma.
//!
//! Goruntu cozucu kutuphanesi eklemiyoruz: ffmpeg zaten zorunlu bagimlilik
//! ve PNG/JPEG/WebP/AVIF hepsini cozuyor. `image` krateri ikiliye birkac yuz
//! KB ve buyuk bir bagimlilik agaci ekleyecekti.

use std::process::Command;

/// GPU'ya yuklenecek fotografin uzun kenari icin ust sinir. Arka plan zaten
/// kare olcusune indirgeniyor; daha buyugu yalnizca bellek yiyor.
const MAX_EDGE: u32 = 1920;

pub struct Photo {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Kaynak olcusunu ust sinira sigdir, en-boy oranini koru, cift sayiya yuvarla.
pub fn fit(width: u32, height: u32) -> (u32, u32) {
    let long = width.max(height);
    if long == 0 {
        return (2, 2);
    }
    if long <= MAX_EDGE {
        return (width.max(1), height.max(1));
    }
    let scale = MAX_EDGE as f64 / long as f64;
    let even = |v: f64| ((v.round() as u32).max(2)) & !1;
    (even(width as f64 * scale), even(height as f64 * scale))
}

/// ffprobe ciktisini ("960,1152") coz.
pub fn parse_size(text: &str) -> Option<(u32, u32)> {
    let line = text.lines().find(|l| l.contains(','))?;
    let (w, h) = line.trim().split_once(',')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

pub fn load(path: &str) -> Result<Photo, String> {
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0"])
        .args(["-show_entries", "stream=width,height"])
        .args(["-of", "csv=p=0"])
        .arg(path)
        .output()
        .map_err(|e| format!("ffprobe calistirilamadi: {e}"))?;
    if !probe.status.success() {
        return Err("fotograf okunamadi (ffprobe)".into());
    }
    let (sw, sh) = parse_size(&String::from_utf8_lossy(&probe.stdout))
        .ok_or("fotografin olcusu anlasilamadi")?;
    let (w, h) = fit(sw, sh);

    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error"])
        .arg("-i")
        .arg(path)
        .args(["-vf", &format!("scale={w}:{h}")])
        .args(["-frames:v", "1"])
        .args(["-pix_fmt", "rgba"])
        .args(["-f", "rawvideo", "-"])
        .output()
        .map_err(|e| format!("ffmpeg calistirilamadi: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "fotograf cozulemedi: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let want = (w as usize) * (h as usize) * 4;
    if out.stdout.len() != want {
        return Err(format!(
            "beklenen {want} bayt, {} geldi",
            out.stdout.len()
        ));
    }
    Ok(Photo {
        width: w,
        height: h,
        rgba: out.stdout,
    })
}

/// Masaustu dosya secicisini ac.
///
/// Kutuphane yerine alt surec: `rfd` ya GTK3'e ya da portal + bir async
/// calisma zamanina baglaniyor; ikisi de bu boyut butcesine agir. Seciciyi
/// bulamazsak arayuzdeki metin kutusu zaten yolu elle almaya devam ediyor.
pub fn pick() -> Option<String> {
    secici(&[
        (
            "zenity",
            &[
                "--file-selection",
                "--title=Arka plan fotografi",
                "--file-filter=Goruntu | *.png *.jpg *.jpeg *.webp *.bmp *.avif",
            ],
        ),
        (
            "kdialog",
            &["--getopenfilename", ".", "Goruntu (*.png *.jpg *.jpeg *.webp *.bmp *.avif)"],
        ),
    ])
}

/// Ag dosyasi (ONNX) secicisi.
pub fn pick_model() -> Option<String> {
    secici(&[
        (
            "zenity",
            &[
                "--file-selection",
                "--title=Segmentasyon agi (ONNX)",
                "--file-filter=ONNX | *.onnx",
            ],
        ),
        ("kdialog", &["--getopenfilename", ".", "ONNX (*.onnx)"]),
    ])
}

fn secici(denemeler: &[(&str, &[&str])]) -> Option<String> {
    for (program, args) in denemeler {
        let Ok(out) = Command::new(program).args(*args).output() else {
            continue;
        };
        if !out.status.success() {
            return None; // secici acildi, kullanici vazgecti
        }
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !path.is_empty() {
            return Some(path);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kucuk_fotograf_oldugu_gibi_kalir() {
        assert_eq!(fit(800, 600), (800, 600));
    }

    #[test]
    fn buyuk_fotograf_sinira_iner_ve_oran_korunur() {
        let (w, h) = fit(4000, 3000);
        assert_eq!(w, MAX_EDGE);
        let oran = (w as f64 / h as f64) / (4000.0 / 3000.0);
        assert!((oran - 1.0).abs() < 0.01, "oran bozuldu: {w}x{h}");
    }

    /// Dik fotografta sinir uzun kenara uygulanmali.
    #[test]
    fn dik_fotografta_uzun_kenar_sinirlanir() {
        let (w, h) = fit(3000, 4000);
        assert_eq!(h, MAX_EDGE);
        assert!(w < h);
    }

    #[test]
    fn olculer_cift_sayi() {
        let (w, h) = fit(3001, 4001);
        assert_eq!(w % 2, 0);
        assert_eq!(h % 2, 0);
    }

    #[test]
    fn ffprobe_ciktisi_cozulur() {
        assert_eq!(parse_size("960,1152\n"), Some((960, 1152)));
        assert_eq!(parse_size("1920,1080"), Some((1920, 1080)));
        assert_eq!(parse_size(""), None);
        assert_eq!(parse_size("bozuk"), None);
    }

    /// ffprobe bazen once bir uyari satiri basiyor; sayisal satiri bulmali.
    #[test]
    fn onceki_satirlar_atlanir() {
        assert_eq!(parse_size("uyari\n1280,720\n"), Some((1280, 720)));
    }

    #[test]
    fn sifir_olcu_cokmez() {
        assert_eq!(fit(0, 0), (2, 2));
    }
}
