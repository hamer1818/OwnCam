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
mod supervisor;

use app::OwnCamApp;

/// Pencere simgesi. Ham RGBA gomulu: yalnizca bunun icin PNG cozucu bir
/// goruntu kutuphanesi eklemeye degmez (arka plan fotografi da ayni sebeple
/// ffmpeg'e veriliyor). Kaynagi `linux/owncam.svg`.
fn pencere_simgesi() -> egui::IconData {
    egui::IconData {
        rgba: include_bytes!("../assets/simge_128.rgba").to_vec(),
        width: 128,
        height: 128,
    }
}

fn main() -> eframe::Result<()> {
    // Elle verilen adres her zaman kazanir; yoksa mDNS arka planda bulur.
    let host = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("OWNCAM_HOST").ok());

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 620.0])
            .with_min_inner_size([640.0, 400.0])
            .with_title("OwnCam")
            // Masaustu girdisindeki `StartupWMClass` ile ayni olmali; pencere
            // yoneticisi acilan pencereyi menudeki uygulamayla ancak boyle
            // eslestiriyor (gorev cubugunda dogru simge ve ad).
            .with_app_id("owncam")
            .with_icon(pencere_simgesi()),
        ..Default::default()
    };

    eframe::run_native(
        "OwnCam",
        options,
        Box::new(|cc| Ok(Box::new(OwnCamApp::new(cc, host)))),
    )
}
