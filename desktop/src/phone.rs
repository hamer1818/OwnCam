//! Telefonun :5300 durum/kontrol ucuyla konusan ince katman.
//!
//! Android tarafi el yazimi JSON donduruyor (StreamService.buildStatusJson).
//! Alanlar camelCase; eksik alanlar `Default` ile doluyor ki telefon eski bir
//! surum calistiriyorsa uygulama cokmesin, sadece o alan bos gorunsun.

use serde::Deserialize;
use std::time::Duration;

pub const STATUS_PORT: u16 = 5300;
pub const STREAM_PORT: u16 = 5299;

const TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Status {
    pub streaming: bool,
    /// Kameradan gelen boyut, orn. "1440x1080".
    pub resolution: Option<String>,
    /// Yayina giden kare, orn. "1280x720". Otomatik donusle **degisiyor**.
    pub frame: Option<String>,
    pub fps: u32,
    pub bitrate: u32,
    pub camera: Option<String>,
    pub sensor_orientation: i32,
    pub image_rotation: i32,
    pub preview: bool,
    pub auto_rotate: bool,
    pub mirror: bool,
    /// Telefonun fiziksel yonu; karenin **sekli** bundan geliyor.
    pub device_orientation: i32,
    pub frame_mode: Option<String>,
    pub applied_rotation: i32,
    /// true ise kenarlarda siyah bant var.
    pub narrow: bool,
    pub exposure_locked: bool,
    pub camera_frames: u64,
    pub gl_draws: u64,
    pub encoder_outputs: u64,
    pub frames_sent: u64,
    pub frames_dropped: u64,
    pub frames_skipped: u64,
    pub bytes_sent: u64,
    pub client: Option<String>,
}

impl Status {
    /// Yayina giden kare boyutu. Alici bu boyutla kurulmali.
    pub fn frame_size(&self) -> Option<(u32, u32)> {
        parse_size(self.frame.as_deref()?)
    }
}

fn parse_size(text: &str) -> Option<(u32, u32)> {
    let (w, h) = text.split_once('x')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

#[derive(Debug, Clone)]
pub struct Phone {
    pub host: String,
}

impl Phone {
    pub fn new(host: impl Into<String>) -> Self {
        Self { host: host.into() }
    }

    fn get(&self, path: &str, params: &[(&str, String)]) -> Result<Status, String> {
        let mut url = format!("http://{}:{}{}", self.host, STATUS_PORT, path);
        if !params.is_empty() {
            let query: Vec<String> = params
                .iter()
                .map(|(k, v)| format!("{k}={}", encode(v)))
                .collect();
            url.push('?');
            url.push_str(&query.join("&"));
        }

        let response = ureq::AgentBuilder::new()
            .timeout(TIMEOUT)
            .build()
            .get(&url)
            .call()
            .map_err(|e| format!("telefona ulasilamadi: {e}"))?;

        let body = response
            .into_string()
            .map_err(|e| format!("yanit okunamadi: {e}"))?;

        serde_json::from_str(&body).map_err(|e| format!("JSON cozulemedi: {e}"))
    }

    pub fn status(&self) -> Result<Status, String> {
        self.get("/status", &[])
    }

    pub fn configure(&self, params: &[(&str, String)]) -> Result<Status, String> {
        self.get("/config", params)
    }

    pub fn start(&self) -> Result<Status, String> {
        self.get("/start", &[])
    }

    pub fn stop(&self) -> Result<Status, String> {
        self.get("/stop", &[])
    }
}

/// Sorgu parametreleri icin asgari yuzde kodlamasi. Gonderdigimiz degerler
/// sayi ve kisa anahtarlardan ibaret; tam bir URL kutuphanesi getirmeye degmez.
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kare_boyutu_ayristirilir() {
        let s = Status {
            frame: Some("1280x720".into()),
            ..Default::default()
        };
        assert_eq!(s.frame_size(), Some((1280, 720)));
    }

    #[test]
    fn bozuk_kare_boyutu_none_doner() {
        let s = Status {
            frame: Some("bozuk".into()),
            ..Default::default()
        };
        assert_eq!(s.frame_size(), None);
    }

    /// Telefonun gercek ciktisi cozulebilmeli; alan adlari camelCase.
    #[test]
    fn telefon_json_cozulur() {
        let json = r#"{
          "streaming": true, "resolution": "1440x1080", "frame": "1280x720",
          "fps": 30, "bitrate": 8000000, "camera": "front",
          "sensorOrientation": 270, "imageRotation": 270, "preview": true,
          "autoRotate": true, "deviceOrientation": 270,
          "frameMode": "telefona-uy", "appliedRotation": 270, "narrow": false,
          "exposureLocked": false, "cameraFrames": 10, "glDraws": 10,
          "encoderOutputs": 11, "framesSent": 9, "framesDropped": 0,
          "framesSkipped": 0, "bytesSent": 123, "client": "192.168.1.106"
        }"#;
        let s: Status = serde_json::from_str(json).unwrap();
        assert!(s.streaming);
        assert_eq!(s.frame_size(), Some((1280, 720)));
        assert_eq!(s.device_orientation, 270);
        assert_eq!(s.frame_mode.as_deref(), Some("telefona-uy"));
    }

    /// Eksik alan cokmemeli - eski telefon surumu senaryosu.
    #[test]
    fn eksik_alanlar_varsayilana_duser() {
        let s: Status = serde_json::from_str(r#"{"streaming": true}"#).unwrap();
        assert!(s.streaming);
        assert_eq!(s.fps, 0);
        assert_eq!(s.frame_size(), None);
    }
}
