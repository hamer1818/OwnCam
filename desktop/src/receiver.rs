//! ffmpeg alicisi: telefondan H.264 al, sanal kameraya ve pencereye ver.
//!
//! **Iki yol var, secim efekte gore:**
//!
//! ```text
//! efekt kapali   telefon -> ffmpeg -+-> sanal kamera      (tek surec, iki cikis)
//!                                   +-> ham RGBA -> pencere
//!
//! efekt acik     telefon -> ffmpeg -> ham RGBA -> GPU -+-> ffmpeg -> sanal kamera
//!                                    (segmentasyon +   +-> onizleme -> pencere
//!                                     kompozit)
//! ```
//!
//! Kapali yol degismedi: Faz 0'da olculen gecikme degerleri (3 ms jitter,
//! 0 bosluk) o yola ait ve efekt istemeyen kullanici onu odemesin diye
//! oldugu gibi duruyor. Acik yolda kareler tam cozunurlukte uygulamadan
//! geciyor; olculen ek yuk 720p'de ~2,5 ms (bkz. `seg::effects`).
//!
//! Ayri alici + ayri izleyici calistirmak `/dev/video11` uzerinde okuma-yazma
//! cekismesi yaratip akisi kilitliyordu (Python surumunde olculdu); iki yolda
//! da sanal kameraya yazan tek bir surec var.

use std::io::{ErrorKind, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::seg::effects::{output_format, Processor, Settings as EffectSettings};
use crate::sink::Sink;

/// Pencereye cizilecek en son kare. Kuyruk **yok**: arayuz geride kalirsa
/// eski kare atilir, yenisi yazilir. Gecikme tamponlanarak degil, kare
/// dusurulerek savunuluyor - projenin geri kalaniyla ayni ilke.
#[derive(Default)]
pub struct FrameSlot {
    inner: Mutex<Option<Frame>>,
}

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl FrameSlot {
    pub fn put(&self, frame: Frame) {
        *self.inner.lock().unwrap() = Some(frame);
    }

    /// Kareyi al ve slotu bosalt; ayni kare iki kez dokuya yuklenmesin.
    pub fn take(&self) -> Option<Frame> {
        self.inner.lock().unwrap().take()
    }
}

/// Arayuzden canli degistirilebilen efekt durumu. Ayar degisikligi akisi
/// yeniden kurmuyor; yalnizca efektin acilip kapanmasi boru hattini
/// degistirdigi icin `ReceiverConfig`'te ayrica tutuluyor.
#[derive(Default)]
pub struct EffectShared {
    pub settings: EffectSettings,
    /// Yeni arka plan fotografi; isleyici bir kez alip GPU'ya yukluyor.
    pub pending_background: Option<(u32, u32, Vec<u8>)>,
    /// Ekran karti acilamadiysa sebebi; arayuz gosteriyor.
    pub error: Option<String>,
    /// Kullanilan ekran kartinin adi; arayuzde gosteriliyor.
    pub gpu: Option<String>,
    /// Son karedeki maske kapsami (0..1). Efekt kapaliyken `None`.
    pub coverage: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiverConfig {
    pub host: String,
    pub fps: u32,
    /// Telefonun **bildirdigi** kare boyutu. Tahmin edilemez: otomatik donusle
    /// telefonun fiziksel yonune gore degisiyor.
    pub frame: (u32, u32),
    /// Onizleme yuzeyinin piksel butcesi (genislik x yukseklik ~ bu deger).
    pub preview_pixels: u32,
    pub preview_fps: u32,
    /// Kareler uygulamadan gecsin mi. Boru hattinin seklini degistirdigi
    /// icin degisince alici yeniden kuruluyor.
    pub effects: bool,
}

impl ReceiverConfig {
    /// Onizleme olcusu: **alana** gore kucultuluyor, genislige gore degil.
    /// Genislige gore olceklemek dik karede (720x1280) 640x1138'lik bir
    /// onizleme uretip boru trafigini uce katliyordu.
    pub fn preview_size(&self) -> (u32, u32) {
        let (w, h) = self.frame;
        if w == 0 || h == 0 {
            return (2, 2);
        }
        let scale = ((self.preview_pixels as f64) / (w as f64 * h as f64)).sqrt();
        let even = |v: f64| ((v / 2.0).round() as u32).max(1) * 2;
        (even(w as f64 * scale), even(h as f64 * scale))
    }
}

pub struct Receiver {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
    pub config: ReceiverConfig,
}

impl Receiver {
    pub fn start(
        config: ReceiverConfig,
        sink: Option<Box<dyn Sink>>,
        slot: Arc<FrameSlot>,
        state: Arc<Mutex<String>>,
        effects: Arc<Mutex<EffectShared>>,
        repaint: impl Fn() + Send + 'static,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let stop = stop.clone();
            let config = config.clone();
            thread::spawn(move || run(config, sink, slot, state, effects, stop, repaint))
        };
        Self {
            stop,
            handle: Some(handle),
            config,
        }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn set_state(state: &Mutex<String>, text: impl Into<String>) {
    *state.lock().unwrap() = text.into();
}

fn run(
    config: ReceiverConfig,
    sink: Option<Box<dyn Sink>>,
    slot: Arc<FrameSlot>,
    state: Arc<Mutex<String>>,
    effects: Arc<Mutex<EffectShared>>,
    stop: Arc<AtomicBool>,
    repaint: impl Fn() + Send,
) {
    // Isleyiciyi bir kez kur: ag agirliklarini ve boru hatlarini her yeniden
    // baglanmada yuklemek gereksiz.
    let mut processor = None;
    if config.effects {
        match Processor::new() {
            Ok(p) => {
                let mut shared = effects.lock().unwrap();
                shared.error = None;
                shared.gpu = Some(p.adapter_name().to_string());
                drop(shared);
                processor = Some(p);
            }
            Err(e) => {
                // Ekran karti yoksa efekt olmadan devam et - sessizce bozulma.
                effects.lock().unwrap().error = Some(e.clone());
                set_state(&state, format!("efekt kapali: {e}"));
            }
        }
    }
    let with_effects = processor.is_some();

    while !stop.load(Ordering::Relaxed) {
        set_state(&state, format!("baglaniyor: {}", config.host));

        let mut child = match spawn_decoder(&config, sink.as_deref(), with_effects) {
            Ok(child) => child,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                set_state(&state, "ffmpeg bulunamadi - kurulu mu?");
                return;
            }
            Err(e) => {
                set_state(&state, format!("ffmpeg baslatilamadi: {e}"));
                return;
            }
        };

        // stderr'i okuyan olmazsa boru dolunca ffmpeg yazarken bloklanir ve
        // tum isleme durur. Ayri is parcaciginda surekli bosaltiyoruz, son
        // satiri teshis icin tutuyoruz.
        let last_error = Arc::new(Mutex::new(String::new()));
        if let Some(stderr) = child.stderr.take() {
            let last_error = last_error.clone();
            thread::spawn(move || drain_stderr(stderr, last_error));
        }

        let mut stdout = match child.stdout.take() {
            Some(out) => out,
            None => {
                let _ = child.kill();
                set_state(&state, "ffmpeg cikisi acilamadi");
                return;
            }
        };

        // Efekt yolunda sanal kameraya yazan ikinci ffmpeg; kapali yolda
        // cozucu zaten dogrudan yaziyor.
        // Yazicinin acildigi bicim; isleyicinin urettigiyle birebir tutmali.
        let writer_format = output_format(config.frame.0, config.frame.1);
        let mut writer = if with_effects {
            match spawn_writer(&config, sink.as_deref()) {
                Ok(w) => w,
                Err(e) => {
                    let _ = child.kill();
                    set_state(&state, format!("sanal kamera yazicisi acilmadi: {e}"));
                    return;
                }
            }
        } else {
            None
        };

        let (pw, ph) = config.preview_size();
        let (fw, fh) = config.frame;
        let mut buffer = if with_effects {
            vec![0u8; (fw as usize) * (fh as usize) * 4]
        } else {
            vec![0u8; (pw as usize) * (ph as usize) * 4]
        };

        let mut got_frame = false;
        let preview_gap = Duration::from_micros(1_000_000 / config.preview_fps.max(1) as u64);
        let mut last_preview = Instant::now() - preview_gap;

        while !stop.load(Ordering::Relaxed) {
            if read_exact_or_eof(&mut stdout, &mut buffer).is_err() {
                break;
            }
            if !got_frame {
                got_frame = true;
                set_state(&state, if with_effects { "yayinda (efektli)" } else { "yayinda" });
            }

            match processor.as_mut() {
                None => {
                    slot.put(Frame {
                        width: pw,
                        height: ph,
                        rgba: buffer.clone(),
                    });
                    repaint();
                }
                Some(processor) => {
                    let settings = {
                        let mut shared = effects.lock().unwrap();
                        if let Some((bw, bh, data)) = shared.pending_background.take() {
                            if let Err(e) = processor.set_background(bw, bh, &data) {
                                shared.error = Some(e);
                            }
                        }
                        shared.settings
                    };
                    let done = match processor.process(fw, fh, &buffer, settings) {
                        Ok(done) => done,
                        Err(e) => {
                            set_state(&state, format!("efekt hatasi: {e}"));
                            break;
                        }
                    };
                    if done.coverage != effects.lock().unwrap().coverage {
                        effects.lock().unwrap().coverage = done.coverage;
                    }
                    // Yazici belirli bir piksel bicimiyle acildi. Isleyici
                    // baska bir bicim uretirse bayt sayisi tutsa bile goruntu
                    // bozulur - ve sessizce bozulur. Yuksek sesle dur.
                    if done.format != writer_format {
                        set_state(
                            &state,
                            format!(
                                "cikti bicimi degisti ({:?} -> {:?})",
                                writer_format, done.format
                            ),
                        );
                        break;
                    }
                    if let Some(stdin) = writer.as_mut().and_then(|w| w.stdin.as_mut()) {
                        // Sanal kamera okumayi birakirsa yazma hata verir;
                        // akisi kesmek yerine dongu yeniden baglanir.
                        if stdin.write_all(&done.frame).is_err() {
                            break;
                        }
                    }
                    // Onizleme arayuzun hizinda; sanal kamera her kareyi aliyor.
                    if last_preview.elapsed() >= preview_gap {
                        last_preview = Instant::now();
                        slot.put(Frame {
                            width: done.preview_size.0,
                            height: done.preview_size.1,
                            rgba: done.preview,
                        });
                        repaint();
                    }
                }
            }
        }

        let _ = child.kill();
        let _ = child.wait();
        if let Some(mut w) = writer.take() {
            drop(w.stdin.take());
            let _ = w.kill();
            let _ = w.wait();
        }

        if stop.load(Ordering::Relaxed) {
            return;
        }

        let detail = last_error.lock().unwrap().clone();
        // Sessizce yeniden denemek "baglaniyor" yazip duran bir pencere
        // birakiyordu; son hata satirini gosteriyoruz.
        set_state(
            &state,
            if detail.is_empty() {
                "baglanti koptu, yeniden deneniyor".to_string()
            } else {
                format!("baglanti koptu: {detail}")
            },
        );
        thread::sleep(Duration::from_millis(1500));
    }
}

/// Telefondan gelen H.264'u cozen surec.
fn spawn_decoder(
    config: &ReceiverConfig,
    sink: Option<&dyn Sink>,
    with_effects: bool,
) -> std::io::Result<Child> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error"])
        // Gecikme icin: ffmpeg'in ic tamponunu ve akis analizini kapat
        .args(["-fflags", "nobuffer+discardcorrupt"])
        .args(["-flags", "low_delay"])
        .args(["-avioflags", "direct"])
        // `-vf scale` filtre grafigi kare boyutunu onceden bilmek zorunda;
        // probesize 32 ile ffmpeg SPS'i cozemeden "unspecified size" ile
        // cikiyordu. Bu degerler yalnizca ilk baglanma anini etkiler.
        .args(["-probesize", "500000"])
        .args(["-analyzeduration", "500000"])
        .args(["-max_delay", "0"])
        // Ham Annex-B'de zaman damgasi yok; kare hizini demuxer'a soyle
        .args(["-r", &config.fps.to_string()])
        .args(["-f", "h264"])
        .args([
            "-i",
            &format!("tcp://{}:{}", config.host, crate::phone::STREAM_PORT),
        ]);

    if with_effects {
        // Tek cikis: tam cozunurlukte ham kare. Sanal kameraya yazmak
        // isleyicinin isi - kompozit oradan cikiyor.
        let (w, h) = config.frame;
        cmd.args(["-map", "0:v"])
            .args(["-fps_mode", "passthrough"])
            .args(["-vf", &format!("scale={w}:{h}")])
            .args(["-pix_fmt", "rgba"])
            .args(["-f", "rawvideo", "-"]);
    } else {
        // 1. cikis: sanal kamera (varsa)
        if let Some(sink) = sink {
            cmd.args(["-map", "0:v"]).args(["-fps_mode", "passthrough"]);
            for arg in sink.ffmpeg_output_args() {
                cmd.arg(arg);
            }
        }
        // 2. cikis: bu pencere. Onizlemeye 30 fps gerekmiyor.
        let (pw, ph) = config.preview_size();
        cmd.args(["-map", "0:v"])
            .args([
                "-vf",
                &format!("scale={pw}:{ph},fps={}", config.preview_fps),
            ])
            .args(["-pix_fmt", "rgba"])
            .args(["-f", "rawvideo", "-"]);
    }

    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
}

/// Islenmis kareleri sanal kameraya yazan surec.
fn spawn_writer(
    config: &ReceiverConfig,
    sink: Option<&dyn Sink>,
) -> std::io::Result<Option<Child>> {
    let Some(sink) = sink else {
        return Ok(None); // sanal kamera yok: uygulama izleyici olarak calisir
    };
    let (w, h) = config.frame;
    // Kompozit kareyi dogrudan sanal kameranin bekledigi bicimde uretiyor;
    // burada donusum yapilmiyor, bayt bayt geciyor.
    let pix_fmt = output_format(w, h).ffmpeg_pix_fmt();
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error"])
        .args(["-fflags", "nobuffer"])
        .args(["-f", "rawvideo"])
        .args(["-pix_fmt", pix_fmt])
        .args(["-s", &format!("{w}x{h}")])
        .args(["-r", &config.fps.to_string()])
        .args(["-i", "-"])
        .args(["-fps_mode", "passthrough"]);
    for arg in sink.ffmpeg_output_args() {
        cmd.arg(arg);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(Some)
}

/// Boru okumasi kisa donebilir; tam kare gelene kadar birlestir.
fn read_exact_or_eof(stream: &mut impl Read, buffer: &mut [u8]) -> std::io::Result<()> {
    let mut filled = 0;
    while filled < buffer.len() {
        match stream.read(&mut buffer[filled..]) {
            Ok(0) => return Err(std::io::Error::new(ErrorKind::UnexpectedEof, "akis bitti")),
            Ok(n) => filled += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn drain_stderr(stderr: impl Read, last_error: Arc<Mutex<String>>) {
    let reader = std::io::BufReader::new(stderr);
    use std::io::BufRead;
    for line in reader.lines().map_while(Result::ok) {
        let line = line.trim().to_string();
        if !line.is_empty() {
            *last_error.lock().unwrap() = line;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(frame: (u32, u32)) -> ReceiverConfig {
        ReceiverConfig {
            host: "1.2.3.4".into(),
            fps: 30,
            frame,
            preview_pixels: 640 * 360,
            preview_fps: 15,
            effects: false,
        }
    }

    /// Onizleme alani yonden bagimsiz olarak ayni butcede kalmali.
    #[test]
    fn onizleme_alani_sabit_kalir() {
        let yatay = config((1280, 720)).preview_size();
        let dikey = config((720, 1280)).preview_size();
        let alan = |(w, h): (u32, u32)| w * h;
        let fark = alan(yatay).abs_diff(alan(dikey));
        assert!(
            fark * 20 < alan(yatay),
            "alanlar cok farkli: {yatay:?} {dikey:?}"
        );
    }

    #[test]
    fn onizleme_en_boy_oranini_korur() {
        let (w, h) = config((1280, 720)).preview_size();
        let oran = w as f64 / h as f64;
        assert!((oran - 16.0 / 9.0).abs() < 0.05, "oran bozuldu: {oran}");
    }

    /// Boyutlar cift olmali; yuv/rgb donusumleri tek sayilarda takiliyor.
    #[test]
    fn onizleme_boyutlari_cift() {
        for frame in [(1280, 720), (720, 1280), (640, 480), (1920, 1080)] {
            let (w, h) = config(frame).preview_size();
            assert_eq!(w % 2, 0, "{frame:?} -> {w}x{h}");
            assert_eq!(h % 2, 0, "{frame:?} -> {w}x{h}");
        }
    }

    #[test]
    fn sifir_kare_cokmez() {
        assert_eq!(config((0, 0)).preview_size(), (2, 2));
    }

    /// Efekt acikken alici yeniden kurulmali: boru hattinin sekli degisiyor.
    #[test]
    fn efekt_degisimi_yapilandirmayi_degistirir() {
        let mut a = config((1280, 720));
        let b = a.clone();
        a.effects = true;
        assert_ne!(a, b);
    }
}
