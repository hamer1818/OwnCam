//! Sanal kamera cikisi - **takilabilir** katman.
//!
//! Kapsam Linux; tek gerceklemesi `v4l2loopback`. Arayuz yine de duruyor
//! cunku cikis noktasi tek yerde toplaniyor ve alici onu bilmek zorunda
//! kalmiyor. Sanal kamera bulunamazsa uygulama **izleyici** olarak calismaya
//! devam eder - sessizce bozulmaz, sadece sanal kamera sunmaz.

/// Bir sanal kamera hedefi. ffmpeg'in o hedefe yazmasi icin gereken
/// bayraklari uretiyor; boylece tek ffmpeg sureci hem kameraya hem pencereye
/// besleme yapabiliyor.
pub trait Sink: Send + Sync {
    /// Kullaniciya gosterilecek ad.
    fn name(&self) -> String;
    /// `-map 0:v -fps_mode passthrough` sonrasina eklenecek ffmpeg bayraklari.
    fn ffmpeg_output_args(&self) -> Vec<String>;
}

/// Linux: v4l2loopback cikis cihazi.
#[derive(Debug, Clone)]
pub struct V4l2Sink {
    pub device: String,
}

impl Sink for V4l2Sink {
    fn name(&self) -> String {
        format!("v4l2loopback ({})", self.device)
    }

    fn ffmpeg_output_args(&self) -> Vec<String> {
        vec![
            "-pix_fmt".into(),
            "yuv420p".into(),
            "-f".into(),
            "v4l2".into(),
            self.device.clone(),
        ]
    }
}

/// Kullanilabilir sanal kamerayi bul. Yoksa `None` - uygulama izleyici olur.
pub fn detect() -> Option<Box<dyn Sink>> {
    detect_platform()
}

#[cfg(target_os = "linux")]
fn detect_platform() -> Option<Box<dyn Sink>> {
    // Ortam degiskeni her zaman kazanir: kurulumlarda cihaz numarasi degisiyor.
    if let Ok(device) = std::env::var("OWNCAM_DEVICE") {
        if is_char_device(&device) {
            return Some(Box::new(V4l2Sink { device }));
        }
    }

    let entries = std::fs::read_dir("/sys/devices/virtual/video4linux").ok()?;
    let mut candidates: Vec<(u8, String)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|entry| {
            let node = entry.file_name().to_string_lossy().to_string();
            let name = std::fs::read_to_string(entry.path().join("name")).ok()?;
            let path = format!("/dev/{node}");
            if !is_char_device(&path) {
                return None;
            }
            score(&name).map(|s| (s, path))
        })
        .collect();

    // Alfabetik siralamak yanlisti: "/dev/video0" (DroidCam'den kalma
    // "Loopback video device") "/dev/video11" (OwnCam) onune geciyordu.
    // Once isim uygunluguna, sonra cihaz yoluna gore siraliyoruz.
    candidates.sort();
    candidates
        .into_iter()
        .next()
        .map(|(_, device)| Box::new(V4l2Sink { device }) as Box<dyn Sink>)
}

/// Cihaz adina gore oncelik; `None` ise bu cihaz hedef degil.
///
/// OBS'in kendi sanal kamerasi **dislaniyor**: oraya yazmak OBS'in ciktisiyla
/// cakisir. Zincir `telefon -> video11 -> OBS -> video10` seklinde, biz
/// zincirin basindayiz.
#[cfg(target_os = "linux")]
fn score(name: &str) -> Option<u8> {
    let lower = name.to_lowercase();
    if lower.contains("obs") {
        return None;
    }
    if lower.trim() == "owncam" {
        Some(0)
    } else if lower.contains("owncam") {
        Some(1)
    } else if lower.contains("loopback") || lower.contains("dummy") {
        Some(2)
    } else {
        None
    }
}

/// Yalnizca karakter cihazi mi diye bakiyoruz.
///
/// "Yazmayi dene" seklinde yoklamak yanlisti: `exclusive_caps=1` ile cihaz o an
/// baska bir uretici tarafindan tutuluyorsa acilis basarisiz oluyor ve cihaz
/// **hic yokmus gibi** eleniyordu - uygulama sessizce izleyiciye dusuyordu.
/// Mesgulluk geciciDir, varlik degildir.
#[cfg(target_os = "linux")]
fn is_char_device(path: &str) -> bool {
    use std::os::unix::fs::FileTypeExt;
    std::fs::metadata(path)
        .map(|m| m.file_type().is_char_device())
        .unwrap_or(false)
}

#[cfg(not(target_os = "linux"))]
fn detect_platform() -> Option<Box<dyn Sink>> {
    // Linux disi kapsam disi; uygulama izleyici olarak calisir.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v4l2_bayraklari_ffmpeg_icin_dogru_sirada() {
        let sink = V4l2Sink {
            device: "/dev/video11".into(),
        };
        assert_eq!(
            sink.ffmpeg_output_args(),
            vec!["-pix_fmt", "yuv420p", "-f", "v4l2", "/dev/video11"]
        );
    }

    #[test]
    fn sink_adi_cihazi_icerir() {
        let sink = V4l2Sink {
            device: "/dev/video9".into(),
        };
        assert!(sink.name().contains("/dev/video9"));
    }

    /// Bu sistemdeki gercek isimler: video0 "Loopback video device"
    /// (DroidCam kalintisi), video10 "OBS Virtual Camera", video11 "OwnCam".
    #[cfg(target_os = "linux")]
    #[test]
    fn owncam_droidcam_kalintisini_yener() {
        let owncam = super::score("OwnCam").expect("OwnCam hedef olmali");
        let loopback = super::score("Loopback video device").expect("loopback hedef olmali");
        assert!(owncam < loopback, "OwnCam once gelmeli");
    }

    /// OBS'in kendi ciktisina yazmak zinciri tersine cevirir.
    #[cfg(target_os = "linux")]
    #[test]
    fn obs_sanal_kamerasi_dislanir() {
        assert_eq!(super::score("OBS Virtual Camera"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn alakasiz_kamera_secilmez() {
        assert_eq!(super::score("Integrated Webcam"), None);
        assert_eq!(super::score("HD Pro Webcam C920"), None);
    }
}
