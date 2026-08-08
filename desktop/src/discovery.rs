//! Telefonu yerel agda bulma - **tasinabilir** mDNS.
//!
//! Onceki surum `avahi-browse` alt sureci calistiriyordu; Linux'a ozeldi ve
//! Windows'ta telefon hicbir zaman bulunamiyordu. `mdns-sd` saf Rust, iki
//! platformda da ayni kod.
//!
//! Kesif **arka planda** ve suruyor: telefon uygulamayi actiktan sonra da
//! yayina baslayabilir, bir kerelik tarama bunu kaciriyordu.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use mdns_sd::{ServiceDaemon, ServiceEvent};

/// Android tarafi kendini bu ad altinda duyuruyor (MdnsAdvertiser.kt).
const SERVICE: &str = "_owncam._tcp.local.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub host: String,
    pub label: String,
}

pub struct Discovery {
    found: Arc<Mutex<Vec<Found>>>,
    _tx: Sender<()>,
}

impl Discovery {
    /// Arka planda taramayi baslat. Hata durumunda bos liste doner - kesif
    /// zorunlu degil, arayuzden IP elle girilebiliyor.
    pub fn start(on_change: impl Fn() + Send + 'static) -> Self {
        let found: Arc<Mutex<Vec<Found>>> = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = channel::<()>();

        {
            let found = found.clone();
            thread::spawn(move || run(found, rx, on_change));
        }

        Self { found, _tx: tx }
    }

    pub fn hosts(&self) -> Vec<Found> {
        self.found.lock().unwrap().clone()
    }

    /// Bulunan ilk telefon; uygulama acilisinda otomatik baglanmak icin.
    pub fn first(&self) -> Option<String> {
        self.found.lock().unwrap().first().map(|f| f.host.clone())
    }
}

fn run(found: Arc<Mutex<Vec<Found>>>, stop: Receiver<()>, on_change: impl Fn()) {
    let daemon = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("mDNS baslatilamadi: {e} - IP elle girilebilir");
            return;
        }
    };
    eprintln!("[kesif] tarama basliyor: {SERVICE}");
    let receiver = match daemon.browse(SERVICE) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mDNS taramasi acilamadi: {e}");
            return;
        }
    };

    loop {
        // Kapanma istegi geldiyse cik.
        if matches!(
            stop.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ) {
            let _ = daemon.shutdown();
            return;
        }

        match receiver.recv_timeout(Duration::from_millis(500)) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                // IPv4'u tercih ediyoruz: telefon TCP'yi IPv4 uzerinden sunuyor
                // ve link-local IPv6 adresleri baglanti kurmuyor.
                let Some(addr) = info.get_addresses().iter().find(|a| a.is_ipv4()) else {
                    continue;
                };
                let entry = Found {
                    host: addr.to_string(),
                    label: format!("{} ({})", trim_instance(info.get_fullname()), addr),
                };
                let mut list = found.lock().unwrap();
                if !list.iter().any(|f| f.host == entry.host) {
                    eprintln!("[kesif] bulundu: {}", entry.label);
                    list.push(entry);
                    drop(list);
                    on_change();
                }
            }
            Ok(ServiceEvent::ServiceRemoved(_, fullname)) => {
                let name = trim_instance(&fullname);
                let mut list = found.lock().unwrap();
                let before = list.len();
                list.retain(|f| !f.label.starts_with(&name));
                if list.len() != before {
                    drop(list);
                    on_change();
                }
            }
            Ok(_) => {}
            // `mdns-sd` flume kanali kullaniyor; hata tipini isimlendirmek icin
            // flume'u dogrudan bagimlilik yapmak gerekirdi. Bunun yerine
            // baglantinin kopup kopmadigini aliciya soruyoruz: zaman asimi
            // normal (dongunun nabzi), kopma ise cikis sebebi.
            Err(_) => {
                if receiver.is_disconnected() {
                    return;
                }
            }
        }
    }
}

/// "OwnCam._owncam._tcp.local." -> "OwnCam"
fn trim_instance(fullname: &str) -> String {
    fullname
        .split_once("._owncam")
        .map(|(name, _)| name.to_string())
        .unwrap_or_else(|| fullname.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ornek_adi_kisaltilir() {
        assert_eq!(trim_instance("OwnCam._owncam._tcp.local."), "OwnCam");
        assert_eq!(trim_instance("Telefon-2._owncam._tcp.local."), "Telefon-2");
    }

    #[test]
    fn beklenmeyen_bicim_oldugu_gibi_kalir() {
        assert_eq!(trim_instance("garip"), "garip");
    }
}
