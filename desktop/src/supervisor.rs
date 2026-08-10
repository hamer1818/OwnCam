//! Telefonu yoklayan ve aliciyi ayakta tutan **gozetmen** is parcacigi.
//!
//! Neden ayri bir is parcacigi: bunlar eskiden egui'nin `update()` dongusunde
//! kosuyordu ve Wayland pencere gorunmez oldugunda cizim geri cagrisi
//! gondermiyor - yani pencere kucultuldugu anda uygulama telefondaki
//! degisikliklere **kor** kaliyordu. Olculdu: pencere kucukken telefon
//! 1280x720'den 1920x1080'e gecti, uygulama fark etmedi; pencere geri
//! acilinca hemen fark etti.
//!
//! Bu bir webcam uygulamasi icin kabul edilemez, cunku uygulama zaten hep
//! kucultulmus durur. En kotu hali telefonun donmesi: kare 720x1280 olur ama
//! alici 1280x720'ye olceklemeye devam eder ve goruntu, kullanici pencereyi
//! acana kadar ezik kalir.
//!
//! Kare boru hatti zaten bagimsizdi (`Receiver` kendi is parcaciginda kosuyor
//! ve olculdu: pencere kucukken sanal kamera beslenmeye devam ediyor). Eksik
//! olan **denetim** katmaniydi; burasi o.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver as MpscReceiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::phone::{Phone, Status};
use crate::receiver::{EffectShared, FrameSlot, Receiver, ReceiverConfig};
use crate::seg::gpu::Model;
use crate::sink;

const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Komutlara tepki suresi. Yoklama araligindan bagimsiz: kullanici efekti
/// acinca iki saniye beklemesin.
const TICK: Duration = Duration::from_millis(100);

/// Arayuzu tazeleme kancasi. `Arc` cunku hem gozetmen dongusu hem de her
/// alici ornegi ayni kancayi tasiyor.
type Repaint = Arc<dyn Fn() + Send + Sync>;

enum Command {
    SetHost(Option<String>),
    /// Efekt acik/kapali ve hangi ag. Ikisi de boru hattinin seklini
    /// degistirdigi icin ayni komutta geliyorlar.
    SetEffects(bool, Model),
    Stop,
}

#[derive(Default)]
struct Shared {
    status: Mutex<Option<Status>>,
    error: Mutex<Option<String>>,
    /// Her yeni durum yanitinda artiyor; arayuz yalnizca degisince ozumsuyor.
    generation: AtomicU64,
}

pub struct Supervisor {
    tx: Sender<Command>,
    shared: Arc<Shared>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Supervisor {
    pub fn start(
        host: Option<String>,
        preview_pixels: u32,
        preview_fps: u32,
        slot: Arc<FrameSlot>,
        state: Arc<Mutex<String>>,
        effects: Arc<Mutex<EffectShared>>,
        repaint: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let repaint: Repaint = Arc::new(repaint);
        let (tx, rx) = channel();
        let shared = Arc::new(Shared::default());
        let handle = {
            let shared = shared.clone();
            thread::spawn(move || {
                run(
                    host,
                    preview_pixels,
                    preview_fps,
                    rx,
                    shared,
                    slot,
                    state,
                    effects,
                    repaint,
                )
            })
        };
        Self {
            tx,
            shared,
            handle: Some(handle),
        }
    }

    pub fn set_host(&self, host: Option<String>) {
        let _ = self.tx.send(Command::SetHost(host));
    }

    pub fn set_effects(&self, on: bool, model: Model) {
        let _ = self.tx.send(Command::SetEffects(on, model));
    }

    /// En son durum ve onun surumu. Surum degismediyse arayuz ozumsemiyor;
    /// aksi halde her karede kullanicinin duzenlemesini geri alirdi.
    pub fn status(&self) -> (Option<Status>, u64) {
        (
            self.shared.status.lock().unwrap().clone(),
            self.shared.generation.load(Ordering::Relaxed),
        )
    }

    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().unwrap().clone()
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Stop);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    host: Option<String>,
    preview_pixels: u32,
    preview_fps: u32,
    rx: MpscReceiver<Command>,
    shared: Arc<Shared>,
    slot: Arc<FrameSlot>,
    state: Arc<Mutex<String>>,
    effects: Arc<Mutex<EffectShared>>,
    repaint: Repaint,
) {
    let mut phone = host.map(Phone::new);
    let mut receiver: Option<Receiver> = None;
    let mut effects_on = false;
    let mut model = Model::Hizli;
    let mut last_poll = Instant::now()
        .checked_sub(POLL_INTERVAL)
        .unwrap_or_else(Instant::now);

    loop {
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                Command::SetHost(h) => {
                    phone = h.map(Phone::new);
                    receiver = None;
                    *shared.status.lock().unwrap() = None;
                    last_poll = Instant::now()
                        .checked_sub(POLL_INTERVAL)
                        .unwrap_or_else(Instant::now);
                }
                Command::SetEffects(on, m) => {
                    if on != effects_on || m != model {
                        effects_on = on;
                        model = m;
                        // Boru hattinin sekli degisiyor; alici yeniden kurulmali.
                        receiver = None;
                    }
                }
                Command::Stop => return,
            }
        }

        if last_poll.elapsed() >= POLL_INTERVAL {
            last_poll = Instant::now();
            if let Some(p) = &phone {
                match p.status() {
                    Ok(status) => {
                        sync_receiver(
                            &mut receiver,
                            &status,
                            &p.host,
                            effects_on,
                            &model,
                            preview_pixels,
                            preview_fps,
                            &slot,
                            &state,
                            &effects,
                            &repaint,
                        );
                        *shared.error.lock().unwrap() = None;
                        *shared.status.lock().unwrap() = Some(status);
                        shared.generation.fetch_add(1, Ordering::Relaxed);
                        repaint();
                    }
                    Err(e) => {
                        eprintln!("[durum] {e}");
                        *shared.error.lock().unwrap() = Some(e);
                        shared.generation.fetch_add(1, Ordering::Relaxed);
                        repaint();
                    }
                }
            }
        }

        thread::sleep(TICK);
    }
}

/// Telefonun bildirdigi kare boyutu degistiyse aliciyi yeniden kur.
///
/// Otomatik donus acikken kare sekli telefonun fiziksel yonuyle degisiyor
/// (1280x720 <-> 720x1280). `-vf scale` boyutu sabit tuttugu icin eski
/// boyutla okumaya devam etmek goruntuyu ezer.
#[allow(clippy::too_many_arguments)]
fn sync_receiver(
    receiver: &mut Option<Receiver>,
    status: &Status,
    host: &str,
    effects_on: bool,
    model: &Model,
    preview_pixels: u32,
    preview_fps: u32,
    slot: &Arc<FrameSlot>,
    state: &Arc<Mutex<String>>,
    effects: &Arc<Mutex<EffectShared>>,
    repaint: &Repaint,
) {
    if !status.streaming {
        if receiver.is_some() {
            eprintln!("[alici] telefon yayini durdurdu, alici kapatiliyor");
            *receiver = None;
        }
        return;
    }
    let Some(frame) = status.frame_size() else {
        eprintln!("[alici] telefon kare boyutu bildirmedi: {:?}", status.frame);
        return;
    };

    let wanted = ReceiverConfig {
        host: host.to_string(),
        fps: if status.fps == 0 { 30 } else { status.fps },
        frame,
        preview_pixels,
        preview_fps,
        effects: effects_on,
        model: model.clone(),
    };
    if receiver.as_ref().map(|r| &r.config) == Some(&wanted) {
        return;
    }

    eprintln!(
        "[alici] kuruluyor: {} {}x{} @{} fps, efekt {}",
        wanted.host, frame.0, frame.1, wanted.fps, wanted.effects
    );
    // Onceki aliciyi once dusur: iki ffmpeg ayni sanal kameraya yazamaz.
    *receiver = None;

    let hook = repaint.clone();
    *receiver = Some(Receiver::start(
        wanted,
        sink::detect(),
        slot.clone(),
        state.clone(),
        effects.clone(),
        move || hook(),
    ));
}
