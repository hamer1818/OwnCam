//! OwnCam masaustu uygulamasi - Windows ve Linux.
//!
//! Neden Rust + egui: tek statik ikili (~10-15 MB), runtime bagimliligi yok,
//! GPU hizlandirmali cizim. Electron 150 MB+, Tauri'nin webview'ina 30 fps ham
//! kare sokmak zayif nokta, GTK4'un Windows tarafi ise dagitim cilesi.
//!
//! Cozme hattina dokunulmadi: ffmpeg alt surec olarak kaliyor. Faz 0'da olculen
//! degerler (3 ms jitter, 0 bosluk, %13 CPU) zaten hedefin altinda; degistirmek
//! icin olculebilir bir gerekce yok. Degisen sadece arayuz kabugu.

mod app;
mod discovery;
mod phone;
mod photo;
mod receiver;
mod seg;
mod sink;

use app::OwnCamApp;

fn main() -> eframe::Result<()> {
    // Elle verilen adres her zaman kazanir; yoksa mDNS arka planda bulur.
    let host = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("OWNCAM_HOST").ok());

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 620.0])
            .with_min_inner_size([640.0, 400.0])
            .with_title("OwnCam"),
        ..Default::default()
    };

    eframe::run_native(
        "OwnCam",
        options,
        Box::new(|cc| Ok(Box::new(OwnCamApp::new(cc, host)))),
    )
}
