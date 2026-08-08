# Telefon → Linux Webcam: Kendi Uygulamamızın Planı

**Tarih:** 2026-08-02
**Hedef sistem:** CachyOS, Ryzen 7 9800X3D, RTX 5080, PipeWire, OBS 32.1.2
**Telefon:** Android 10, WiFi bağlantısı (USB kablo güvenilir değil)

---

## 1. Neden yazıyoruz, neyi kabul ediyoruz

Mevcut araçların her biri bir yerden tıkandı:

| Araç | Tıkanma noktası |
|---|---|
| DroidCam (ücretsiz) | 640x480 ve ~14.3 fps'e kilitli. Ölçüldü: 70 ms'lik metronom gibi düzenli aralık, yani ağ değil bilinçli sınır. |
| DroidCamX | Ücretli, kapalı kaynak, yine kendi sınırları var. |
| scrcpy | Kamera kaynağı **Android 12+** istiyor. Android 10'da sadece ekran yansıtabiliyor. |
| Iriun | Çalışabilirdi — `ufw` gelen bağlantıları düşürüyordu (UDP 5353 + port 4699). Kapalı kaynak ve ek DKMS modülü gerektiriyor. |

**Dürüst not:** Iriun'un çalışması tek bir `ufw` kuralına bakıyordu. Kendi uygulamamızı yazmanın gerekçesi "başka çare yok" değil; gerekçe **kontrol**: kaynak/kare hızı sınırı yok, kapalı ikili yok, ek DKMS modülü yok, gecikme bütçesini biz belirliyoruz.

**Kabul ettiğimiz maliyet:** Faz 0 için birkaç gün, tam olgunluk için haftalar. Aşağıdaki plan bu maliyeti fazlara bölerek her fazın sonunda **çalışan bir şey** bırakacak şekilde tasarlandı.

---

## 2. Başarı kriterleri (ölçülebilir)

Tahminle değil, rakamla karar vereceğiz. Referans değerler bu sistemde ölçüldü:

| Metrik | Mevcut (DroidCam) | Faz 0 hedefi | Nihai hedef | **Faz 0 ölçümü** |
|---|---|---|---|---|
| Çözünürlük | 640x480 | 1280x720 | 1920x1080 | **1920x1080** ✅ nihai |
| Kare hızı (`/dev/videoN`) | 14.3 fps | ≥ 25 fps | 30 fps sabit | **29.6 fps** ✅ nihai |
| Uçtan uca gecikme | ölçülmedi | < 250 ms | < 120 ms | öznel: sorunsuz |
| Kare aralığı jitter | 7 ms | < 15 ms | < 10 ms | **3 ms** ✅ nihai |
| 100 ms'den uzun boşluk | 0 | 0 | 0 | **0** ✅ |

Faz 0 ölçümü 2026-08-02, `/dev/video11`. **Nihai hedefler Faz 0'da tutuldu** —
Faz 1'e (kendi Rust alıcımız) geçmenin ölçülebilir bir gerekçesi kalmadı.
Alıcının CPU maliyeti 1080p'de %13 tek çekirdek (720p'de %6).

Gecikme rakamla değil gözle değerlendirildi; kronometre yöntemiyle sayısallaştırmak
isteyen için bölüm 9'daki yordam duruyor.

**Bir fazın sonunda hedef tutmuyorsa bir sonraki faza geçmeden önce durup nedenini ölçeceğiz.**

---

## 3. Mimari

```
┌─────────────── ANDROID 10 ────────────────┐
│  Camera2 API                              │
│      │ (Surface, zero-copy)               │
│      ▼                                    │
│  MediaCodec H.264 donanım kodlayıcı       │
│      │ (NAL birimleri)                    │
│      ▼                                    │
│  TCP sunucu :5299                         │
└───────────────┬───────────────────────────┘
                │  WiFi (yerel ağ)
┌───────────────▼──── LINUX ────────────────┐
│  Alıcı (Faz 0: ffmpeg / Faz 1: kendi)     │
│      │ H.264 çözme                        │
│      ▼                                    │
│  /dev/video11  (v4l2loopback OUTPUT)      │
└───────────────┬───────────────────────────┘
                │
        ┌───────▼────────┐
        │ OBS + arka plan│  → /dev/video10 → Zoom/Meet/Discord
        │ kaldırma       │
        └────────────────┘
```

**Kritik tasarım kararı:** Kamera görüntüsü Camera2'den MediaCodec'e **Surface üzerinden** gidiyor. Byte dizisi olarak CPU'ya hiç uğramıyor. Bu tek karar tek başına ~30-50 ms kazandırır.

---

## 4. Gecikme bütçesi

Nereye ne kadar harcayabileceğimizi baştan belirleyelim; ölçüm bu tabloya karşı yapılacak.

| Aşama | Bütçe | Not |
|---|---|---|
| Kamera yakalama → Surface | 15-33 ms | Sensör + ISP, kontrolümüz sınırlı |
| H.264 kodlama (donanım) | 10-20 ms | `KEY_PRIORITY=0` şart |
| Ağ (WiFi, yerel) | 5-20 ms | Bu sistemde router RTT'si 4.7 ms ölçüldü |
| Çözme | 5-15 ms | Yazılımsal çözme 9800X3D'de yeterli |
| v4l2loopback yazma | < 5 ms | Tampon = 2 kare olmalı, fazlası gecikme |
| OBS + arka plan kaldırma | 33-66 ms | Ölçüldü: %108 tek çekirdek, 30 fps sabit |
| **Toplam** | **73-159 ms** | |

En büyük tek risk: alıcı tarafın **fazla tampon tutması**. DroidCam'de hissettiğin gecikmenin bir kısmı buydu. Tampon boyutu her fazda açıkça ayarlanacak.

---

## 5. Faz 0 — Çalışan iskelet (hedef: 2-3 gün)

**Amaç:** Linux tarafına *hiç kod yazmadan* uçtan uca akış kurmak. Tüm efor Android'e gitsin.

### 5.1 Android uygulaması

Minimum uygulama. Kotlin, tek Activity, arayüz yok denecek kadar az.

- [ ] Yeni Android Studio projesi, `minSdk = 29` (Android 10)
- [ ] `AndroidManifest.xml`: `CAMERA`, `INTERNET`, `FOREGROUND_SERVICE`, `WAKE_LOCK` izinleri
- [ ] Çalışma zamanı kamera izni isteme
- [ ] `CameraManager` ile arka kamerayı aç (Camera2 API)
- [ ] `MediaCodec` H.264 kodlayıcı oluştur, `createInputSurface()` çağır
- [ ] Bu Surface'i `CameraCaptureSession`'a hedef olarak ver → **zero-copy yol**
- [ ] `MediaCodec` çıkışından NAL birimlerini oku
- [ ] TCP sunucu (port 5299), her NAL'i **4 byte big-endian uzunluk + veri** olarak gönder
- [ ] SPS/PPS'i (`BUFFER_FLAG_CODEC_CONFIG`) her yeni istemciye ilk önce gönder
- [ ] Foreground service + wake lock (ekran kapanınca akış kesilmesin)

**MediaCodec ayarları — Android 10 için kritik:**

```kotlin
val format = MediaFormat.createVideoFormat("video/avc", 1280, 720).apply {
    setInteger(MediaFormat.KEY_COLOR_FORMAT,
        MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface)
    setInteger(MediaFormat.KEY_BIT_RATE, 8_000_000)
    setInteger(MediaFormat.KEY_FRAME_RATE, 30)
    setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 1)
    setInteger(MediaFormat.KEY_BITRATE_MODE,
        MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_CBR)
    // Gerçek zamanlı öncelik - gecikme icin en onemli ayar
    setInteger(MediaFormat.KEY_PRIORITY, 0)
    setInteger(MediaFormat.KEY_PROFILE,
        MediaCodecInfo.CodecProfileLevel.AVCProfileBaseline)
}
```

> **Android 10 tuzağı:** `MediaFormat.KEY_LOW_LATENCY` **API 30 (Android 11)** ile geldi. Senin telefonunda yok. Bunun yerine `KEY_PRIORITY = 0` (gerçek zamanlı) ve **Baseline profil** kullan — Baseline'da B-kare yok, bu da kodlayıcının kare biriktirmesini engeller. B-kare kullanan profiller tek başına 2-3 kare gecikme ekler.

### 5.2 Linux tarafı — kod yok

```bash
ffmpeg -fflags nobuffer -flags low_delay -probesize 32 -analyzeduration 0 \
       -f h264 -i tcp://TELEFON_IP:5299 \
       -pix_fmt yuv420p -f v4l2 /dev/video11
```

Bayrakların anlamı: `nobuffer` + `low_delay` ffmpeg'in iç tamponunu kapatır, `probesize 32` ve `analyzeduration 0` başlangıçtaki akış analizi beklemesini kaldırır. Bunlar olmadan ffmpeg tek başına 500 ms+ ekler.

### 5.3 Faz 0 çıkış kriteri

Aşağıdaki ölçüm yapılıp tabloya yazılacak:

```bash
# Kare hizi ve jitter
ffmpeg -hide_banner -f v4l2 -i /dev/video11 -t 12 -vf showinfo -f null - 2>&1 \
  | grep -oE "pts_time:[0-9.]+" | sed 's/pts_time://' > /tmp/pts.txt
```

≥ 25 fps ve jitter < 15 ms tutuyorsa Faz 1'e geç. Tutmuyorsa **önce nedenini bul** — kodlayıcı mı, ağ mı, ffmpeg mi.

---

## 6. Faz 1 — Kendi Linux alıcımız (hedef: 1 hafta)

**Amaç:** ffmpeg'i çıkarıp tampon üzerinde tam kontrol almak, yeniden bağlanma ve servisleşme eklemek.

**Dil önerisi: Rust.** Gerekçe: bellek güvenliği, `v4l` crate'i v4l2 OUTPUT cihazlarını destekliyor, `ffmpeg-next` ile donanım/yazılım çözme yapılabiliyor, tek ikili dosya olarak dağıtılıyor. C de olur ama elle bellek yönetimi bu iş için gereksiz risk.

- [ ] TCP istemci + uzunluk-önekli NAL okuyucu
- [ ] H.264 çözücü (`ffmpeg-next`, önce yazılımsal — 1080p30 bu CPU'da önemsiz)
- [ ] YUV420 → v4l2loopback'e yazma (`VIDIOC_S_FMT` ile format ayarla, sonra `write()`)
- [ ] **Tampon = en fazla 2 kare.** Geriye kalan varsa **en yeni kareyi al, eskileri at** (gecikme birikmesin)
- [ ] Bağlantı koptuğunda otomatik yeniden bağlanma (üstel geri çekilme)
- [ ] mDNS ile telefonu otomatik bulma (`_telefonkamera._tcp`) — sabit IP yazmaktan kurtarır
- [ ] systemd kullanıcı servisi (`Restart=always`), EasyEffects servisindeki desen aynen kullanılabilir
- [ ] `--fps`, `--device`, `--buffer` komut satırı seçenekleri

**Bu sistemde hazır olan altyapı:**
- `v4l2loopback` 0.15.4 yüklü, `/dev/video11` "Iriun Webcam" etiketiyle mevcut (etiketi değiştirebiliriz)
- `/etc/modprobe.d/v4l2loopback.conf` iki cihaz tanımlıyor: 10 (OBS çıkışı) ve 11 (kaynak)
- `exclusive_caps=1` ayarlı — üretici yazmaya başlayana kadar cihaz okunamaz görünür, bu **normal**

> **ufw uyarısı:** `ufw` aktif ve varsayılan gelen politikası `DROP`. Telefon PC'ye bağlanacaksa kural gerekir. Bunu **tersine çevirerek tamamen atlayabiliriz**: PC telefona bağlansın (giden trafik zaten `ACCEPT`). DroidCam'in çalışıp Iriun'un çalışmamasının sebebi tam olarak buydu. **Öneri: telefon TCP sunucu olsun, PC istemci olsun.** Yukarıdaki mimari zaten böyle kurgulandı.

---

## 7. Faz 2 — Aktarımı sağlamlaştırma (hedef: 1-2 hafta, isteğe bağlı)

Faz 1 sonunda gecikme hedefi tutuyorsa **bu fazı atla.** TCP yerel ağda genelde yeterlidir.

Tutmuyorsa sebep büyük ihtimalle TCP'nin baş-blokajı (bir paket kaybolunca arkasındaki her şey bekler). O zaman:

- [ ] UDP + RTP paketleme (RFC 6184, H.264 için)
- [ ] Alıcıda jitter tamponu (uyarlanabilir, 2-4 kare)
- [ ] Kayıp kare tespiti → anahtar kare isteği (basit geri kanal)
- [ ] İsteğe bağlı: WebRTC'ye geçiş (tıkanıklık kontrolü + NACK + FEC hazır gelir, ama karmaşıklık büyük sıçrar)

**Karar noktası:** WebRTC'ye geçmek neredeyse projeyi baştan yazmak demek. Faz 1 yeterliyse buraya hiç girme.

---

## 8. Faz 3 — Kullanılabilirlik

- [ ] Android tarafında ayar ekranı: çözünürlük, bit hızı, ön/arka kamera, odak kilidi
- [ ] Otomatik pozlama kilidi — **senin ortamın için önemli**, pencereden gelen arka ışık yüzünden kamera sürekli pozlama arıyor
- [ ] Uyarlanabilir bit hızı (WiFi zayıflayınca kaliteyi düşür, kare atlamak yerine)
- [ ] Linux tarafında tepsi simgesi / basit durum arayüzü
- [ ] Telefon ekranı kapalıyken çalışma (foreground service + wake lock zaten Faz 0'da)
- [ ] Paketleme: AUR PKGBUILD + systemd servisi

---

## 9. Ölçüm yöntemi

Her fazda aynı ölçümü yap, sonuçları bölüm 2'deki tabloya işle.

**Kare hızı ve jitter:**
```bash
ffmpeg -hide_banner -f v4l2 -i /dev/video11 -t 12 -vf showinfo -f null - 2>&1 \
  | grep -oE "pts_time:[0-9.]+" | sed 's/pts_time://' > /tmp/pts.txt
python3 -c "
import numpy as np
t=np.loadtxt('/tmp/pts.txt'); d=np.diff(t)*1000
print(f'fps={len(t)/(t[-1]-t[0]):.1f} ort={d.mean():.0f}ms jitter={d.std():.0f}ms max={d.max():.0f}ms')
"
```

**Uçtan uca gecikme (en güvenilir yöntem):**
Ekranda milisaniyeli bir kronometre aç, telefonu ekrana doğrult, sanal kameradan kare yakala, karedeki değeri o anki saatle karşılaştır. Çapraz korelasyon yöntemini denedik, hareket az olduğu için güvenilir sonuç vermedi — bu yöntem daha kesin.

**CPU maliyeti:**
```bash
PID=$(pgrep -x UYGULAMA_ADI)
T1=$(awk '{print $14+$15}' /proc/$PID/stat); sleep 5
T2=$(awk '{print $14+$15}' /proc/$PID/stat)
python3 -c "print(f'{($T2-$T1)/$(getconf CLK_TCK)/5*100:.0f}% tek cekirdek')"
```

---

## 10. Riskler ve durma noktaları

| Risk | Belirti | Ne yapmalı |
|---|---|---|
| Telefonun kodlayıcısı yavaş | Faz 0'da fps < 20, CPU düşük | Çözünürlüğü 720p'de tut, 1080p'yi bırak |
| Android 10 kodlayıcı kısıtları | Gecikme 200 ms'in altına inmiyor | Baseline profil ve `KEY_PRIORITY=0` doğrulandı mı kontrol et |
| WiFi jitter | Düzensiz kare aralıkları, boşluklar | Faz 2'ye geç (UDP/RTP) |
| Telefon ısınma/kısma | Zamanla fps düşüyor | Bit hızını düşür, 720p'ye in |
| Kapsam büyümesi | Faz 3'te takılıp kalmak | Faz 1 çalışıyorsa **bırak ve kullan** |

**Açık durma noktası:** Faz 1 sonunda 720p/25fps/<200ms elde edilirse bu zaten DroidCam'in iki katı akıcılık ve dört katı çözünürlük demek. Oraya varınca "yeterince iyi" deyip durmak meşru bir karar.

---

## 11. İlk adımlar

1. Android Studio kur (yoksa), yeni proje aç: `minSdk 29`, Kotlin, Empty Activity
2. Camera2 + MediaCodec + TCP sunucu iskeletini yaz (Faz 0, bölüm 5.1)
3. Telefonda çalıştır, IP'sini not et
4. Linux'ta bölüm 5.2'deki ffmpeg komutunu çalıştır
5. Bölüm 9'daki ölçümü yap, sonucu bölüm 2 tablosuna yaz
6. Hedef tutuyorsa Faz 1'e geç

---

## Ek: Bu sistemde hazır olan altyapı

Bunlar bu oturumda kuruldu ve çalışıyor, yeniden yapmaya gerek yok:

- `v4l2loopback` 0.15.4 (DKMS), `/etc/modprobe.d/v4l2loopback.conf` ile 2 cihaz: `/dev/video10` (OBS çıkışı), `/dev/video11` (kaynak girişi)
- OBS 32.1.2 + `obs-backgroundremoval` 1.3.7, sahne hazır: arka plan katmanı + kamera + RVM MobileNetV3 modeli, CPU'da %108 tek çekirdek, 30 fps sabit
- OBS sanal kamerası `/dev/video10`, başlatıcı: "Sanal Kamera (arka plan kaldırma)"
- OBS sahne dosyası: `~/.config/obs-studio/basic/scenes/Başlıksız.json` — kaynağın `device_id` alanını `/dev/video11` yapmak yeterli
- `ufw` aktif, gelen `DROP`. Mimari "PC istemci" olarak kurgulandığı için kural gerekmiyor.
