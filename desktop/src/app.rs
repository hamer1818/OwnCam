//! egui arayuzu: solda canli goruntu, sagda telefon ayarlari.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver as MpscReceiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::discovery::Discovery;
use crate::phone::{Phone, Status};
use crate::photo;
use crate::receiver::{EffectShared, FrameSlot, Receiver, ReceiverConfig};
use crate::seg::effects::{Background, Settings as EffectSettings};
use crate::sink;

pub const RESOLUTIONS: [(u32, u32); 3] = [(640, 480), (1280, 720), (1920, 1080)];
pub const FPS_OPTIONS: [u32; 4] = [15, 24, 30, 60];
pub const ROTATIONS: [i32; 4] = [0, 90, 180, 270];
/// Anahtarlar Android tarafindaki `StreamConfig.FrameMode` ile ayni olmali.
pub const FRAME_MODES: [(&str, &str); 2] = [
    ("telefona-uy", "Telefona uy (dikeyde dikey)"),
    ("tam-kadraj", "Tam kadraj (kirpma yok)"),
];

pub const EFFECT_MODES: [&str; 4] = [
    "Kapali",
    "Arka plani bulaniklastir",
    "Duz renk",
    "Fotograf",
];

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const PREVIEW_PIXELS: u32 = 640 * 360;

/// Arka plan efektinin arayuzdeki hali. Yalnizca acik/kapali gecisi boru
/// hattini degistiriyor; kalan ayarlar akisi kesmeden canli uygulaniyor.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectUi {
    pub mode: usize,
    pub blur: f32,
    pub color: [f32; 3],
    pub sharpness: f32,
    pub photo: String,
}

impl Default for EffectUi {
    fn default() -> Self {
        Self {
            mode: 0,
            blur: 0.6,
            color: [0.05, 0.35, 0.6],
            sharpness: 0.35,
            photo: String::new(),
        }
    }
}

impl EffectUi {
    /// Baslangic efektini ortamdan al: `OWNCAM_EFFECT=bulanik|renk|foto`
    /// ve foto icin `OWNCAM_EFFECT_PHOTO=/yol/foto.jpg`. Arayuzden secim
    /// yapmadan da acilabilsin diye; testler de bunu kullaniyor.
    pub fn from_env() -> Self {
        let mut ui = Self::default();
        if let Ok(mode) = std::env::var("OWNCAM_EFFECT") {
            ui.mode = Self::mode_index(&mode);
        }
        if let Ok(path) = std::env::var("OWNCAM_EFFECT_PHOTO") {
            ui.photo = path;
        }
        ui
    }

    /// Ortam degiskenindeki adi indekse cevir; taninmayan ad efekti acmaz.
    pub fn mode_index(name: &str) -> usize {
        match name.trim().to_lowercase().as_str() {
            "bulanik" | "blur" => 1,
            "renk" | "color" => 2,
            "foto" | "fotograf" | "image" => 3,
            _ => 0,
        }
    }

    pub fn enabled(&self) -> bool {
        self.mode != 0
    }

    pub fn background(&self) -> Background {
        match self.mode {
            1 => Background::Blur(self.blur),
            2 => Background::Color(self.color),
            3 => Background::Image,
            _ => Background::Off,
        }
    }

    pub fn settings(&self) -> EffectSettings {
        EffectSettings {
            background: self.background(),
            sharpness: self.sharpness,
        }
    }
}

pub struct OwnCamApp {
    phone: Option<Phone>,
    status: Option<Status>,
    /// Telefona gonderilecek ayarlar; arayuzden duzenleniyor.
    settings: Settings,
    /// Telefondan gelen degerleri arayuze yansitirken kendi degisikligimizi
    /// geri gondermemek icin bayrak.
    applying: bool,

    receiver: Option<Receiver>,
    frame: Arc<FrameSlot>,
    texture: Option<egui::TextureHandle>,
    texture_size: (u32, u32),

    receiver_state: Arc<Mutex<String>>,
    status_line: String,
    host_input: String,

    poll_tx: Sender<Result<Status, String>>,
    poll_rx: MpscReceiver<Result<Status, String>>,
    poll_pending: Arc<AtomicBool>,
    last_poll: Instant,

    sink_name: Option<String>,
    /// Arka plan efekti: arayuzdeki hali ve isleyiciyle paylasilan durum.
    effect: EffectUi,
    effects: Arc<Mutex<EffectShared>>,
    /// Efekt acilip kapandiginda boru hattinin sekli degisiyor; bir sonraki
    /// karede aliciyi yeniden kur (2 sn'lik yoklamayi bekleme).
    resync: bool,
    photo_state: Arc<Mutex<String>>,
    discovery: Discovery,
    /// Kesif bir telefon buldugunda kendiliginden baglan - ama yalnizca
    /// kullanici/parametre bir secim yapmamissa; secimi ezmeyelim.
    auto_connected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub resolution: usize,
    pub fps: usize,
    pub rotation: usize,
    pub frame_mode: usize,
    pub auto_rotate: bool,
    pub front: bool,
    pub exposure_lock: bool,
    pub phone_preview: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            resolution: 1,
            fps: 2,
            rotation: 0,
            frame_mode: 0,
            auto_rotate: true,
            front: true,
            exposure_lock: false,
            phone_preview: true,
        }
    }
}

impl Settings {
    fn to_params(&self) -> Vec<(&'static str, String)> {
        let (w, h) = RESOLUTIONS[self.resolution];
        let mut params = vec![
            ("width", w.to_string()),
            ("height", h.to_string()),
            ("fps", FPS_OPTIONS[self.fps].to_string()),
            ("mode", FRAME_MODES[self.frame_mode].0.to_string()),
            ("auto", bit(self.auto_rotate)),
            ("front", bit(self.front)),
            ("exposure", bit(self.exposure_lock)),
            ("preview", bit(self.phone_preview)),
        ];
        // Otomatikken donusu gondermiyoruz: telefon zaten kendi fiziksel
        // yonunden belirliyor, gonderirsek bir sonraki yon okumasi hemen
        // geri alir ve akis bosuna yeniden kurulur.
        if !self.auto_rotate {
            params.push(("rotation", ROTATIONS[self.rotation].to_string()));
        }
        params
    }
}

fn bit(value: bool) -> String {
    if value { "1".into() } else { "0".into() }
}

impl OwnCamApp {
    pub fn new(cc: &eframe::CreationContext<'_>, host: Option<String>) -> Self {
        let (poll_tx, poll_rx) = channel();
        let detected = sink::detect();
        let sink_name = detected.as_ref().map(|s| s.name());
        drop(detected);

        // Kesif arka planda surekli calisir: telefon uygulama acildiktan sonra
        // da yayina baslayabilir, tek seferlik tarama bunu kaciriyordu.
        let ctx = cc.egui_ctx.clone();
        let discovery = Discovery::start(move || ctx.request_repaint());
        let host_given = host.is_some();

        let mut app = Self {
            phone: host.clone().map(Phone::new),
            status: None,
            settings: Settings::default(),
            applying: false,
            receiver: None,
            frame: Arc::new(FrameSlot::default()),
            texture: None,
            texture_size: (0, 0),
            receiver_state: Arc::new(Mutex::new(String::new())),
            status_line: match &host {
                Some(h) => format!("telefon: {h}"),
                None => "araniyor (mDNS)...".into(),
            },
            host_input: host.unwrap_or_default(),
            poll_tx,
            poll_rx,
            poll_pending: Arc::new(AtomicBool::new(false)),
            last_poll: Instant::now() - POLL_INTERVAL,
            sink_name,
            effect: EffectUi::from_env(),
            effects: Arc::new(Mutex::new(EffectShared::default())),
            resync: false,
            photo_state: Arc::new(Mutex::new(String::new())),
            discovery,
            // Elle adres verildiyse kesif secimi ezmesin.
            auto_connected: host_given,
        };
        if app.effect.mode == 3 && !app.effect.photo.is_empty() {
            app.load_photo(app.effect.photo.clone());
        }
        app.texture = Some(cc.egui_ctx.load_texture(
            "kare",
            egui::ColorImage::new([2, 2], egui::Color32::from_gray(20)),
            egui::TextureOptions::LINEAR,
        ));
        app
    }

    fn poll_status(&mut self) {
        let Some(phone) = self.phone.clone() else {
            return;
        };
        if self.poll_pending.swap(true, Ordering::SeqCst) {
            return; // onceki sorgu hala yolda
        }
        let tx = self.poll_tx.clone();
        let pending = self.poll_pending.clone();
        std::thread::spawn(move || {
            let result = phone.status();
            let _ = tx.send(result);
            pending.store(false, Ordering::SeqCst);
        });
    }

    fn apply_settings(&self) {
        let Some(phone) = self.phone.clone() else {
            return;
        };
        let params = self.settings.to_params();
        std::thread::spawn(move || {
            let _ = phone.configure(&params);
        });
    }

    /// Telefonun bildirdigi degerleri arayuze yansit.
    fn absorb(&mut self, status: &Status) {
        self.applying = true;
        if let Some(res) = status.resolution.as_deref() {
            if let Some((w, h)) = res.split_once('x') {
                if let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>()) {
                    if let Some(i) = RESOLUTIONS.iter().position(|&r| r == (w, h)) {
                        self.settings.resolution = i;
                    }
                }
            }
        }
        if let Some(i) = FPS_OPTIONS.iter().position(|&f| f == status.fps) {
            self.settings.fps = i;
        }
        if let Some(i) = ROTATIONS.iter().position(|&r| r == status.image_rotation) {
            self.settings.rotation = i;
        }
        if let Some(mode) = status.frame_mode.as_deref() {
            if let Some(i) = FRAME_MODES.iter().position(|&(k, _)| k == mode) {
                self.settings.frame_mode = i;
            }
        }
        self.settings.auto_rotate = status.auto_rotate;
        self.settings.front = status.camera.as_deref() == Some("front");
        self.settings.exposure_lock = status.exposure_locked;
        self.settings.phone_preview = status.preview;
        self.applying = false;
    }

    /// Kare boyutu degistiyse aliciyi yeni boyutla yeniden kur.
    ///
    /// Otomatik donus acikken kare sekli telefonun fiziksel yonuyle degisiyor
    /// (1280x720 <-> 720x1280). `-vf scale` boyutu sabit tuttugu icin eski
    /// boyutla okumaya devam etmek goruntuyu bozar.
    fn sync_receiver(&mut self, ctx: &egui::Context, status: &Status) {
        let Some(phone) = self.phone.clone() else {
            return;
        };
        if !status.streaming {
            self.receiver = None;
            return;
        }
        let Some(frame) = status.frame_size() else {
            return;
        };
        let wanted = ReceiverConfig {
            host: phone.host.clone(),
            fps: if status.fps == 0 { 30 } else { status.fps },
            frame,
            preview_pixels: PREVIEW_PIXELS,
            preview_fps: 15,
            effects: self.effect.enabled(),
        };
        if self.receiver.as_ref().map(|r| &r.config) == Some(&wanted) {
            return;
        }
        // Onceki aliciyi once dusur: iki ffmpeg ayni sanal kameraya yazamaz.
        self.receiver = None;
        let ctx = ctx.clone();
        self.receiver = Some(Receiver::start(
            wanted,
            sink::detect(),
            self.frame.clone(),
            self.receiver_state.clone(),
            self.effects.clone(),
            move || ctx.request_repaint(),
        ));
    }

    fn upload_frame(&mut self, ctx: &egui::Context) {
        let Some(frame) = self.frame.take() else {
            return;
        };
        let size = [frame.width as usize, frame.height as usize];
        if size[0] * size[1] * 4 != frame.rgba.len() {
            return; // boyut degisimi sirasindaki yarim kare
        }
        let image = egui::ColorImage::from_rgba_unmultiplied(size, &frame.rgba);
        match &mut self.texture {
            Some(texture) if self.texture_size == (frame.width, frame.height) => {
                texture.set(image, egui::TextureOptions::LINEAR);
            }
            slot => {
                *slot = Some(ctx.load_texture("kare", image, egui::TextureOptions::LINEAR));
                self.texture_size = (frame.width, frame.height);
            }
        }
    }
}

impl eframe::App for OwnCamApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Kesif bir telefon bulduysa kendiliginden baglan.
        if !self.auto_connected {
            if let Some(host) = self.discovery.first() {
                self.phone = Some(Phone::new(host.clone()));
                self.host_input = host;
                self.auto_connected = true;
                self.last_poll = Instant::now() - POLL_INTERVAL;
            }
        }

        while let Ok(result) = self.poll_rx.try_recv() {
            match result {
                Ok(status) => {
                    self.absorb(&status);
                    self.sync_receiver(ctx, &status);
                    self.status_line = match self.phone.as_ref() {
                        Some(p) => format!("telefon: {}", p.host),
                        None => String::new(),
                    };
                    self.status = Some(status);
                }
                Err(e) => self.status_line = e,
            }
        }
        if self.last_poll.elapsed() >= POLL_INTERVAL {
            self.last_poll = Instant::now();
            self.poll_status();
        }
        self.upload_frame(ctx);

        egui::SidePanel::right("ayarlar")
            .default_width(300.0)
            .show(ctx, |ui| self.settings_panel(ui));

        if self.resync {
            self.resync = false;
            if let Some(status) = self.status.clone() {
                self.sync_receiver(ctx, &status);
            }
        }
        egui::CentralPanel::default().show(ctx, |ui| self.video_panel(ui));

        // Kare gelmese de durum satiri ve sayaclar tazelensin.
        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

impl OwnCamApp {
    fn video_panel(&mut self, ui: &mut egui::Ui) {
        let available = ui.available_size();
        if let Some(texture) = &self.texture {
            let [tw, th] = texture.size();
            if tw > 2 && th > 2 {
                // En-boy orani korunarak sigdir; kalan yer arka plan.
                let scale = (available.x / tw as f32).min(available.y / th as f32);
                let size = egui::vec2(tw as f32 * scale, th as f32 * scale);
                ui.centered_and_justified(|ui| {
                    ui.add(egui::Image::new(texture).fit_to_exact_size(size));
                });
                return;
            }
        }
        ui.centered_and_justified(|ui| {
            ui.label(self.receiver_state.lock().unwrap().clone());
        });
    }

    /// Arka plan efekti bolumu. Yalnizca acik/kapali gecisi aliciyi yeniden
    /// kuruyor; kalan ayarlar paylasilan duruma yazilip canli uygulaniyor.
    fn effect_panel(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        ui.strong("Arka plan");

        let onceki_acik = self.effect.enabled();
        combo(
            ui,
            "efekt",
            &mut self.effect.mode,
            |i| EFFECT_MODES[i].to_string(),
            EFFECT_MODES.len(),
        );

        match self.effect.mode {
            1 => {
                ui.add(egui::Slider::new(&mut self.effect.blur, 0.0..=1.0).text("bulaniklik"));
            }
            2 => {
                ui.horizontal(|ui| {
                    ui.label("renk");
                    ui.color_edit_button_rgb(&mut self.effect.color);
                });
            }
            3 => {
                ui.horizontal(|ui| {
                    if ui.button("Sec...").clicked() {
                        if let Some(path) = photo::pick() {
                            self.effect.photo = path.clone();
                            self.load_photo(path);
                        }
                    }
                    if ui.button("Yukle").clicked() && !self.effect.photo.trim().is_empty() {
                        self.load_photo(self.effect.photo.trim().to_string());
                    }
                });
                ui.add(egui::TextEdit::singleline(&mut self.effect.photo).hint_text("foto yolu"));
                let durum = self.photo_state.lock().unwrap().clone();
                if !durum.is_empty() {
                    ui.small(durum);
                }
            }
            _ => {}
        }

        if self.effect.enabled() {
            ui.add(
                egui::Slider::new(&mut self.effect.sharpness, 0.0..=1.0).text("kenar sertligi"),
            );
            let (err, gpu) = {
                let shared = self.effects.lock().unwrap();
                (shared.error.clone(), shared.gpu.clone())
            };
            if let Some(err) = err {
                ui.colored_label(egui::Color32::from_rgb(200, 120, 60), err);
            } else if let Some(gpu) = gpu {
                ui.small(format!("ekran karti: {gpu}"));
            }
        }

        // Ayarlari isleyiciye ilet; acik/kapali degistiyse alici yeniden kurulmali.
        self.effects.lock().unwrap().settings = self.effect.settings();
        if self.effect.enabled() != onceki_acik {
            self.resync = true;
        }
    }

    fn load_photo(&self, path: String) {
        let effects = self.effects.clone();
        let state = self.photo_state.clone();
        *state.lock().unwrap() = "yukleniyor...".into();
        std::thread::spawn(move || match photo::load(&path) {
            Ok(p) => {
                let ozet = format!("{}x{} yuklendi", p.width, p.height);
                effects.lock().unwrap().pending_background = Some((p.width, p.height, p.rgba));
                *state.lock().unwrap() = ozet;
            }
            Err(e) => *state.lock().unwrap() = e,
        });
    }

    fn settings_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.heading("OwnCam");
        ui.label(&self.status_line);
        ui.label(self.receiver_state.lock().unwrap().clone());

        match &self.sink_name {
            Some(name) => {
                ui.colored_label(egui::Color32::from_rgb(90, 180, 90), format!("kamera: {name}"))
            }
            None => ui.colored_label(
                egui::Color32::from_rgb(200, 160, 60),
                "sanal kamera yok - izleyici modunda",
            ),
        };

        ui.separator();

        let found = self.discovery.hosts();
        if !found.is_empty() {
            ui.label("Bulunan telefonlar:");
            for entry in &found {
                let current = self.phone.as_ref().map(|p| p.host.as_str()) == Some(entry.host.as_str());
                if ui.selectable_label(current, &entry.label).clicked() {
                    self.phone = Some(Phone::new(entry.host.clone()));
                    self.host_input = entry.host.clone();
                    self.auto_connected = true;
                    self.last_poll = Instant::now() - POLL_INTERVAL;
                }
            }
            ui.add_space(4.0);
        }

        if self.phone.is_none() {
            if found.is_empty() {
                ui.label("mDNS ile araniyor... bulunamazsa IP'yi elle gir:");
            }
            ui.horizontal(|ui| {
                ui.text_edit_singleline(&mut self.host_input);
                if ui.button("Baglan").clicked() && !self.host_input.trim().is_empty() {
                    self.phone = Some(Phone::new(self.host_input.trim()));
                    self.auto_connected = true;
                    self.last_poll = Instant::now() - POLL_INTERVAL;
                }
            });
            return;
        }

        let before = self.settings.clone();

        egui::Grid::new("ayar_izgara")
            .num_columns(2)
            .spacing([8.0, 6.0])
            .show(ui, |ui| {
                ui.label("Cozunurluk");
                combo(ui, "cozunurluk", &mut self.settings.resolution, |i| {
                    let (w, h) = RESOLUTIONS[i];
                    format!("{w}x{h}")
                }, RESOLUTIONS.len());
                ui.end_row();

                ui.label("Kare hizi");
                combo(ui, "fps", &mut self.settings.fps, |i| {
                    format!("{} fps", FPS_OPTIONS[i])
                }, FPS_OPTIONS.len());
                ui.end_row();

                ui.label("Kadraj");
                combo(ui, "kadraj", &mut self.settings.frame_mode, |i| {
                    FRAME_MODES[i].1.to_string()
                }, FRAME_MODES.len());
                ui.end_row();

                ui.label("Donus");
                ui.add_enabled_ui(!self.settings.auto_rotate, |ui| {
                    combo(ui, "donus", &mut self.settings.rotation, |i| {
                        format!("{}°", ROTATIONS[i])
                    }, ROTATIONS.len());
                });
                ui.end_row();
            });

        ui.add_space(4.0);
        ui.checkbox(&mut self.settings.auto_rotate, "Otomatik donus (telefonu takip et)");
        ui.checkbox(&mut self.settings.front, "On kamera");
        ui.checkbox(&mut self.settings.exposure_lock, "Pozlamayi kilitle");
        ui.checkbox(&mut self.settings.phone_preview, "Telefonda onizleme");

        if self.settings != before && !self.applying {
            self.apply_settings();
        }

        self.effect_panel(ui);

        ui.add_space(6.0);
        let streaming = self.status.as_ref().map(|s| s.streaming).unwrap_or(false);
        if ui
            .button(if streaming { "Yayini durdur" } else { "Yayini baslat" })
            .clicked()
        {
            if let Some(phone) = self.phone.clone() {
                std::thread::spawn(move || {
                    let _ = if streaming { phone.stop() } else { phone.start() };
                });
                // Bir sonraki yoklama gecikmesini beklemeden durumu tazele.
                self.last_poll = Instant::now() - POLL_INTERVAL;
            }
        }

        ui.separator();
        if let Some(status) = &self.status {
            ui.monospace(format!(
                "kare      {} @ {} fps{}",
                status.frame.as_deref().unwrap_or("-"),
                status.fps,
                if status.auto_rotate { " (oto)" } else { "" }
            ));
            ui.monospace(format!(
                "yakalama  {}",
                status.resolution.as_deref().unwrap_or("-")
            ));
            ui.monospace(format!(
                "donus     {}°  telefon yonu {}°",
                status.applied_rotation, status.device_orientation
            ));
            if status.narrow {
                ui.colored_label(egui::Color32::from_rgb(200, 120, 60), "DAR (kenarlar siyah)");
            }
            ui.monospace(format!(
                "gonderilen {}  dusen {}",
                status.frames_sent, status.frames_dropped
            ));
            ui.monospace(format!("kodlayici  {}", status.encoder_outputs));
            ui.monospace(format!(
                "bagli PC   {}",
                status.client.as_deref().unwrap_or("-")
            ));
        }
    }
}

fn combo(
    ui: &mut egui::Ui,
    id: &str,
    selected: &mut usize,
    label: impl Fn(usize) -> String,
    count: usize,
) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(label(*selected))
        .show_ui(ui, |ui| {
            for i in 0..count {
                ui.selectable_value(selected, i, label(i));
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Otomatik donus acikken donus gonderilmemeli - telefon geri alirdi.
    #[test]
    fn otomatik_donuste_rotation_gonderilmez() {
        let s = Settings { auto_rotate: true, ..Default::default() };
        let params = s.to_params();
        assert!(!params.iter().any(|(k, _)| *k == "rotation"));
        assert!(params.iter().any(|(k, v)| *k == "auto" && v == "1"));
    }

    #[test]
    fn elle_donuste_rotation_gonderilir() {
        let s = Settings { auto_rotate: false, rotation: 3, ..Default::default() };
        let params = s.to_params();
        let rotation = params.iter().find(|(k, _)| *k == "rotation").unwrap();
        assert_eq!(rotation.1, "270");
        assert!(params.iter().any(|(k, v)| *k == "auto" && v == "0"));
    }

    /// Ortamdan gelen efekt adlari dogru moda esleniyor mu.
    #[test]
    fn efekt_adlari_moda_eslenir() {
        assert_eq!(EffectUi::mode_index("bulanik"), 1);
        assert_eq!(EffectUi::mode_index("BLUR"), 1);
        assert_eq!(EffectUi::mode_index("renk"), 2);
        assert_eq!(EffectUi::mode_index("foto"), 3);
        assert_eq!(EffectUi::mode_index("sacma"), 0);
        assert_eq!(EffectUi::mode_index(""), 0);
    }

    /// Mod indeksleri `Background`'a dogru cevrilmeli.
    #[test]
    fn mod_arka_plana_cevrilir() {
        let mut e = EffectUi::default();
        assert_eq!(e.background(), Background::Off);
        assert!(!e.enabled());
        e.mode = 1;
        e.blur = 0.5;
        assert_eq!(e.background(), Background::Blur(0.5));
        assert!(e.enabled());
        e.mode = 2;
        e.color = [1.0, 0.0, 0.0];
        assert_eq!(e.background(), Background::Color([1.0, 0.0, 0.0]));
        e.mode = 3;
        assert_eq!(e.background(), Background::Image);
    }

    /// Efekt listesi ile mod indeksleri ayni uzunlukta olmali.
    #[test]
    fn efekt_listesi_tutarli() {
        assert_eq!(EFFECT_MODES.len(), 4);
        for (i, ad) in EFFECT_MODES.iter().enumerate() {
            assert!(!ad.is_empty(), "{i}. efektin adi bos");
        }
    }

    /// Mod anahtarlari Android tarafiyla birebir ayni olmali.
    #[test]
    fn kadraj_anahtarlari_android_ile_ayni() {
        let s = Settings { frame_mode: 0, ..Default::default() };
        let params = s.to_params();
        let mode = params.iter().find(|(k, _)| *k == "mode").unwrap();
        assert_eq!(mode.1, "telefona-uy");
    }
}
