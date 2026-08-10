# OwnCam — kurulum ve kullanim

Telefonu Linux'ta webcam yapar. Iki parca var: telefondaki **APK** ve PC'deki
**masaustu uygulamasi**. Ikisi de gerekli.

Kisa yol: APK'yi kur, PC'de v4l2loopback'i ayarla, `owncam`'i calistir.

---

## 0. Neye ihtiyacin var

| | |
|---|---|
| Telefon | Android 10 (API 29) ve ustu |
| PC | Linux, x86-64 |
| Ag | Telefon ve PC **ayni WiFi**'da |
| Paketler | `ffmpeg`, `v4l2loopback` cekirdek modulu |
| Arka plan efekti icin | Vulkan ya da OpenGL 4.3 destekleyen ekran karti |

Windows ve macOS desteklenmiyor ve planlanmiyor.

Guvenlik duvari icin **kural eklemene gerek yok**: baglantiyi PC baslatiyor,
telefon dinliyor. `ufw` varsayilan olarak gelen baglantilari engelliyorsa
sorun cikmaz.

---

## 1. Telefon

`owncam-<surum>.apk` dosyasini indir ve kur:

```bash
adb install owncam-0.1.1.apk
```

Kablo yoksa APK'yi telefona kopyalayip dosya yoneticisinden acabilirsin;
"bilinmeyen kaynak" iznini istedigi zaman ver.

> **Daha once bir OwnCam derlemesi kuruluysa** once onu kaldir
> (`adb uninstall com.owncam`). Bu surum farkli bir anahtarla imzalandi, bu
> yuzden ustune yuklenmez. Telefondaki ayarlar (donus, ayna, bit hizi) silinir;
> uygulama acilinca varsayilanlara doner.

Uygulamayi ac. Kamera ve bildirim izinlerini ver. Yayin **kendiliginden**
basliyor; telefon tarafinda baska bir ayar yok, hepsi PC'den yapiliyor.

Ekrani kapatabilir, uygulamayi arka plana atabilirsin — yayin devam eder.
(Olculdu: 30 dakika, ekran kapali, 48 812 kare, 0 dusen, 28 fps, pil 37→39 °C.)

---

## 2. PC — sanal kamera cihazi

Goruntunun gorunecegi cihaz `v4l2loopback` ile olusuyor. Bir kez yapiliyor.

**Modulu kur** (dagitimina gore):

```bash
sudo pacman -S v4l2loopback-dkms      # Arch / CachyOS / Manjaro
sudo apt install v4l2loopback-dkms    # Debian / Ubuntu
sudo dnf install v4l2loopback         # Fedora
```

**Cihazlari tanimla.** `/etc/modprobe.d/v4l2loopback.conf` dosyasini olustur:

```
options v4l2loopback devices=2 video_nr=10,11 card_label="OBS Virtual Camera,OwnCam" exclusive_caps=1,1
```

Iki cihaz aciliyor: `/dev/video11` OwnCam'in yazdigi yer, `/dev/video10` OBS
kullanmak istersen onun cikisi. Yalnizca OwnCam yetiyorsa `devices=1
video_nr=11 card_label="OwnCam" exclusive_caps=1` de olur.

**Acilista yuklensin:**

```bash
echo v4l2loopback | sudo tee /etc/modules-load.d/v4l2loopback.conf
sudo modprobe -r v4l2loopback ; sudo modprobe v4l2loopback
```

Dogrula:

```bash
v4l2-ctl --list-devices | grep -A2 OwnCam
```

> `exclusive_caps=1` yuzunden cihaz, **bir sey yazmaya baslayana kadar**
> okunamaz gorunur. Bu normal, ariza degil.

---

## 3. PC — masaustu uygulamasi

Surumden inen arsivi ac ve kurulum betigini calistir:

```bash
tar xzf owncam-0.1.1-linux-x86_64.tar.gz
cd owncam-0.1.1-linux-x86_64
./kur-masaustu.sh
```

**sudo yok** — her sey `~/.local` altina gidiyor:

| Ne | Nereye |
|---|---|
| Uygulama | `~/.local/bin/owncam` |
| Menu girdisi | `~/.local/share/applications/owncam.desktop` |
| Simgeler | `~/.local/share/icons/hicolor/…` (SVG + 16–256 px) |

Bundan sonra **uygulama menusunde "OwnCam" olarak gorunuyor**; arama kutusuna
`webcam`, `kamera`, `telefon`, `camera`, `arka plan` ya da `droidcam` yazinca
da cikiyor. Tiklayinca aciliyor, gorev cubugunda kendi simgesiyle duruyor.

Kaldirmak icin `./kaldir-masaustu.sh` — yazdigi her seyi geri aliyor.

Kurmadan, oldugu yerden denemek istersen `./owncam` de calisir. Telefonu mDNS
ile kendi buluyor; bulamazsa IP'yi ver:

```bash
./owncam 192.168.1.42
```

Artik goruntu `/dev/video11`'de. Zoom, Meet, Discord, OBS — kamera listesinden
**OwnCam**'i sec.

---

## 4. Kullanim

Uygulama acikken sag panelden ayarlar **canli** degisiyor; cogu icin akis
kesilmiyor.

### Goruntu

| Ayar | Ne yapar |
|---|---|
| Cozunurluk / kare hizi | telefonun encoder'ina gider |
| Kamera | on / arka |
| Donus | goruntuyu dogru cevirir, telefona kaydedilir |
| Otomatik donus | telefon dondukce goruntu dik kalir |
| Ayna | yatay cevirir (varsayilan kapali) |
| Kadraj | `telefona uy` kirparak doldurur, `tam kadraj` hicbir seyi kirpmaz |

Telefonu nasil monte edersen et, **ilk kurulumda bir kez** donus kalibrasyonu
yap: `owncam-calibrate.sh` dort acinin hepsinden birer kare alip yan yana
koyar, sectigini telefona kaydeder. Kamerayi "yukarisi belli" bir seye tut —
bir insana ya da odaya, tavana degil.

### Arka plan

| Secim | Ne yapar |
|---|---|
| Kapali | kare oldugu gibi gecer, ek gecikme yok |
| Bulaniklastir | kisi net, arkasi bulanik (yogunluk ayarlanabilir) |
| Duz renk | arkasi secilen renkle dolar |
| Fotograf | arkasina secilen foto konur (kadraji kaplayacak sekilde) |

**Kenar sertligi** kaydiraci gecisin genisligini ayarliyor. Dusuk deger sac
gibi ince yapilarda daha dogru, yuksek deger daha keskin bir kesim verir.

Efekt ekran kartinda kosuyor: 1280x720'de kare basina 1,9 ms.

"kisi bulunamadi" uyarisi cikiyorsa ag kareyi cozemiyor demektir — kamerayi
bas-omuz cercevesine al. Modelin kapsami bu; yuzun kareyi doldurdugu asiri
yakin cekimde calismiyor.

### Daha iyi kenar: kaliteli ag (istege bagli)

Varsayilan ag kucuk ve hizli ama kaba — saci kesiyor ve kenarda halesi oluyor.
**Robust Video Matting** sac tellerini ve ince nesneleri koruyor.

Agirliklari arsivde **yok**: RVM GPL-3.0 lisansli, OwnCam MIT. Kendin
indiriyorsun:

1. [RobustVideoMatting surumleri](https://github.com/PeterL1n/RobustVideoMatting/releases)
   sayfasindan `rvm_mobilenetv3_fp32.onnx` (15 MB) indir.
2. Uygulamada **Arka plan > Ag > ONNX sec...** ile dosyayi goster.

Bedeli 1280x720'de kare basina 13,7 ms (33 ms butcenin altinda) ve ~300 MB
ekran karti bellegi. Olcumde sanal kamera 29,5 fps'te kaldi.

---

## 5. Arayuzsuz kullanim (istege bagli)

Arka plan efekti istemiyorsan uygulamayi hic acmadan, servis olarak da
kosabilirsin:

```bash
linux/install.sh                  # ~/.local/bin ve ~/.config/systemd/user
systemctl --user enable --now owncam
```

Bu yol `avahi-daemon` istiyor (telefonu bulmak icin); masaustu uygulamasi
istemiyor.

> **Ikisini ayni anda calistirma.** Ikisi de `/dev/video11`'e yaziyor, biri
> hata verir. Uygulamayi kullanmadan once `systemctl --user stop owncam`.

Teshis komutlari:

```bash
owncam-status.sh              # telefonun canli durumu, ozet tablo
owncam-status.sh --json
owncam-status.sh --rotate 90  # donusu ayarla ve telefona kaydet
owncam-snapshot.sh            # /dev/video11'den tek kare -> PNG
owncam-measure.sh 15          # 15 saniye kare hizi / titreme olcumu
```

---

## 6. Bir seyler ters giderse

**Menude gorunuyor ama tiklayinca acilmiyor.** Kurulum betigi `Exec` satirina
mutlak yolu yaziyor, tam da bu yuzden — ama uygulamayi elle kopyaladiysan
menudeki girdi `~/.local/bin`'i oturumun `PATH`'inde arar ve cogu dagitimda
orada olmaz. `./kur-masaustu.sh` ile kur.

**Menude cikmiyor.** Bazi masaustleri veritabanini gec tazeliyor; oturumu
kapatip acmak yetiyor. Girdinin gecerli oldugunu su dogrular:
`desktop-file-validate ~/.local/share/applications/owncam.desktop`

**Uygulama telefonu bulamiyor.** Ikisi ayni WiFi'da mi? Bazi yonlendiriciler
"istemci yalitimi" ile cihazlarin birbirini gormesini engelliyor. IP'yi elle
ver: `./owncam 192.168.1.42`. Telefonun IP'si telefonun WiFi ayarlarinda.

**Goruntu geliyor ama kamera listesinde OwnCam yok.** Uygulamayi kamerayi
acmadan **once** baslat; birçok program cihaz listesini bir kez okuyor.

**"Not a video capture device".** Eski bir `ffmpeg` cihazi tutuyor:

```bash
pgrep -af "ffmpeg.*video11"    # once PID'leri gor
kill <PID>                     # sonra numarayla oldur
```

(`pkill -f owncam...` komutun kendi kabugunu de yakalayabiliyor; once bak,
sonra oldur.)

**Goruntu yan/ters.** `owncam-calibrate.sh` calistir. Donus telefona
kaydedilir, bir daha yapman gerekmez.

**Goruntu ezik/dar.** Kadraj modunu `tam kadraj` yap.

**Arka plan efekti acilmiyor, "ekran karti" hatasi.** Vulkan ya da OpenGL 4.3
gerekiyor. `vulkaninfo --summary` ya da `glxinfo | grep "OpenGL version"` ile
bak.

**Kare hizi 30 degil 28.** Bu cihazin gercek hizi, kusur degil. Olculdu.

---

## 7. Kaynaktan derlemek

```bash
# Masaustu (Linux)
cd desktop && cargo build --release      # target/release/owncam

# Android — sistem JDK'si AGP ile calismiyor, Android Studio'nunkini kullan
cd android && JAVA_HOME=/opt/android-studio/jbr ./gradlew assembleDebug
```

Yayin APK'si icin imza anahtari ortamdan geliyor:

```bash
OWNCAM_KEYSTORE=/yol/release.keystore OWNCAM_KEYSTORE_PAROLA=... \
  JAVA_HOME=/opt/android-studio/jbr ./gradlew assembleRelease
```

Anahtar verilmezse yayin APK'si **imzasiz** cikar — bilerek boyle, sessizce
hata ayiklama anahtariyla imzalamak sonraki surumlerin ustune yuklenmesini
kalici olarak bozardi.

Testler:

```bash
cd desktop && cargo test        # 55 test
```
