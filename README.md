# OwnCam

Telefon kamerasini WiFi uzerinden Linux'ta sanal webcam yapan uygulama.
[telefon-kamera-plani.md](telefon-kamera-plani.md) Faz 0 uygulamasi.

```
Android 10                                    Linux (CachyOS)
Camera2 ─> SurfaceTexture ─> GL ─┬─> MediaCodec ─> TCP :5299 ─> ffmpeg ─> /dev/video11
           (donus matrisi)       │   H.264 baseline                          │
                                 └─> ekrandaki onizleme                      ▼
                                                            OBS + arka plan kaldirma
                                                                             │
                                                                             ▼
                                                                      /dev/video10
                                                                (Zoom / Meet / Discord)
```

## Durum — bitti, nihai hedefler tutuldu

2026-08-02, telefon bagli, `/dev/video11` uzerinden olculdu:

| Metrik | DroidCam | Faz 0 hedefi | Nihai hedef | **OwnCam** |
|---|---|---|---|---|
| Cozunurluk | 640x480 | 1280x720 | 1920x1080 | **1920x1080** |
| Kare hizi | 14.3 fps | >= 25 | 30 sabit | **29.6 fps** |
| Jitter | 7 ms | < 15 ms | < 10 ms | **3 ms** |
| En uzun aralik | - | - | - | 50 ms |
| >100 ms bosluk | 0 | 0 | 0 | **0** |
| Alici CPU | - | - | - | %13 tek cekirdek |

DroidCam'e gore iki kat akicilik, **alti kat** cozunurluk. Planin nihai
hedeflerinin tamami Faz 0'da tutuldu; Faz 1'e (kendi Rust alicimiz) gecmenin
olculebilir gerekcesi kalmadi.

Gecikme rakamla degil gozle degerlendirildi. Sayisallastirmak icin plan
bolum 9'daki kronometre yontemi duruyor.

| | |
|---|---|
| Android APK | telefonda calisiyor |
| Linux alici | calisiyor |
| systemd servisi | kurulu ve etkin (`systemctl --user enable --now owncam`) |
| OBS kaynagi | `/dev/video11` |
| Yeniden baglanma | test edildi: baglanti kopunca 1 s'de toparliyor |
| Cihaz etiketi | config yazildi, modul yeniden yuklendiginde "OwnCam" olacak |

## Kurulum

### 1. Telefon

APK zaten derli:

```bash
adb install -r android/app/build/outputs/apk/debug/app-debug.apk
```

Yeniden derlemek icin (Android Studio'nun JDK'si gerekiyor, sistemdeki JDK 26
AGP ile calismiyor):

```bash
cd android && JAVA_HOME=/opt/android-studio/jbr ./gradlew assembleDebug
```

Uygulama acilinca yayin kendiliginden basliyor; ayarlar masaustu uygulamasindan
yapiliyor. Telefonu nasil monte edersen et, ilk kurulumdan sonra bir kez
`owncam-calibrate.sh` calistir — dogru donusu secip kaydeder. Yatay montaj tam
genislikte bir kadraj verir; dikey montaj dik bir kadraj (fizik, bkz. asagisi).

### 2. Linux

```bash
linux/install.sh
```

`~/.local/bin`'e uc script, `~/.config/systemd/user`'a servis kurar. sudo yok.

```bash
owncam-receive.sh                 # telefonu mDNS ile bulur
owncam-receive.sh 192.168.1.42    # IP elle
```

Servis olarak:

```bash
systemctl --user enable --now owncam
```

### 3. Masaustu uygulamasi (arka plan silme burada)

```bash
cd desktop && cargo build --release
./target/release/owncam            # telefonu mDNS ile bulur
```

Sag paneldeki **Arka plan** bolumunden secilir:

| Secim | Ne yapar |
|---|---|
| Kapali | kare oldugu gibi gecer, ek gecikme yok |
| Arka plani bulaniklastir | kisi net, arkasi bulanik (yogunluk ayarlanabilir) |
| Duz renk | arkasi secilen renkle doldurulur |
| Fotograf | arkasina secilen foto konur (kadraji kaplayacak sekilde) |

**Kenar sertligi** kaydiraci maskenin gecisini ayarliyor: dusuk deger sac gibi
ince yapilarda daha dogru, yuksek deger daha keskin bir kesim veriyor.

Efekt ekran kartinda kosuyor — bu sistemde kare basina 2,1 ms. Islemcide ayni
is 32 ms surerdi, yani 30 fps butcesini tek basina doldururdu; olcum ve gerekce
icin `desktop/README.md`.

Efekt olmadan da kullanilabilir; kapaliyken kareler uygulamaya hic ugramaz ve
olculmus dusuk gecikmeli yol aynen korunur.

Ag "selfie" cercevesi icin egitilmis: **bas ve omuzlar** goruntude olsun.
Yuzun kareyi doldurdugu asiri yakin cekimde kisiyi ayirt edemiyor.

### 4. OBS'i OwnCam'e baglat

OBS sahnesindeki kamera kaynagi su an `/dev/video0`'i (DroidCam) gosteriyor.
**OBS kapaliyken:**

```bash
linux/obs-use-video11.sh
```

Istege bagli, kozmetik — `/dev/video11`'in etiketini "Iriun Webcam"den
"OwnCam"e cevirir. Modulu yeniden yuklemek gerektigi icin owncam servisini
kendisi durdurup sonra yeniden baslatir; **OBS kapali olmali**:

```bash
sudo linux/relabel-v4l2loopback.sh
```

## PC'den telefonu okumak

Telefon 5300 portunda kucuk bir durum ucu acar; hangi ayarla hangi goruntunun
gittigi PC'den gorulebilir.

```bash
owncam-status.sh              # ozet tablo
owncam-status.sh --json       # ham JSON
owncam-status.sh --rotate 90  # donusu ayarla ve telefona kaydet
owncam-calibrate.sh           # dogru donusu gozle olcup kaydet
```

Ciktisi: yakalama boyutu, gonderilen kare boyutu, kare hizi, bit hizi, kamera
yonu, sensor acisi, goruntu donusu, uygulanan donus, kadrajin dar olup olmadigi,
kare sayaclari, bagli PC.

Goruntunun kendisine bakmak icin:

```bash
owncam-snapshot.sh          # /tmp/owncam-kare.png
```

Ikisi birlikte "ters mi duz mu, dar mi genis mi" sorusunu telefona elle bakmadan
cevaplar. Kimlik dogrulamasi yok - video akisiyla ayni guven seviyesinde, yerel
ag icin.

## Olcum

```bash
owncam-measure.sh 12
```

Plan bolum 9'daki olcumu yapar: fps, ortalama aralik, jitter, 100 ms'den uzun
bosluk sayisi. Faz 0 kriteri fps >= 25 ve jitter < 15 ms.

Uctan uca gecikme icin plan bolum 9'daki kronometre yontemi: ekranda
milisaniyeli kronometre ac, telefonu ekrana dogrult, sanal kameradan kare
yakala, karedeki degeri o anki saatle karsilastir.

## Tasarim notlari

**Telefon sunucu, PC istemci.** `ufw` aktif ve gelen politikasi `DROP`. Bu yon
sayesinde Linux'ta hicbir guvenlik duvari kurali gerekmiyor — giden trafik
zaten `ACCEPT`. (Iriun'un calismamasinin sebebi tam olarak buydu.)

**Tel formati ham Annex-B, uzunluk-onekli degil.** Plan bolum 5.1'de
"4 byte big-endian uzunluk + veri" yaziyordu ama bolum 5.2'deki
`ffmpeg -f h264 -i tcp://...` komutu ham Annex-B bekliyor — ikisi bir arada
calismaz. MediaCodec zaten Annex-B uretiyor, o yuzden ham gonderiyoruz:
Faz 0'in "Linux tarafina hic kod yazma" hedefi korunuyor ve Faz 1'deki kendi
alicimiz icin de ayristirmasi zor bir sey degil.

**Zero-copy.** Camera2 -> GL -> MediaCodec zincirinin tamami GPU'da. Goruntu
byte dizisi olarak CPU'ya hic ugramiyor. Basta kamera dogrudan kodlayicinin
`createInputSurface()` yuzeyine yaziyordu; donus ve onizleme icin araya bir GL
gecisi girdi, ama veri hala GPU'dan cikmiyor.

**Donus olculur, hesaplanmaz.** Ivmeolcer devrede degil: telefon bir stant
uzerinde ya da masada duz dururken `OrientationEventListener` yonu hic okuyamaz
ve son bilinen deger bayatlar. Sensor acisindan turetmek de denendi ve tutmadi —
elde iki bagimsiz gozlem var, ikisi de birbirinden farkli iki formulle **ayni
derecede** uyusuyor, yani veri formulu secmeye yetmiyor. Webcam hep ayni sekilde
durdugu icin dogru aciyi bir kez olcup kaydetmek hem kesin hem her cihazda
calisiyor:

```bash
owncam-calibrate.sh
```

Dort acinin her birinde bir kare yakalar, yan yana koyar, sectigini telefona
kaydeder. Ayar kalicidir; telefon yeniden baslasa da korunur.

Donus 0↔180 ve 90↔270 arasinda degisirken kare boyutu ayni kaldigi icin akis
kesilmez, yalnizca GL matrisi degisir. 0/180 ↔ 90/270 gecisinde kare boyutu
degistigi icin akis yeniden kurulur (~1 sn).

**Kamera her zaman yatay kare uretir — bu degistirilemez.** Sensor telefonun
govdesine sabit, Camera2 goruntuyu dondurmez ve yalnizca yatay boyutlar sunar.
Telefonu dik tuttugunda o yatay tampon dunyada **dik bir alani** gosterir. Yani
"yatay geliyor" olan sey tamponun sekli, kadrajin sekli degil.

Geriye tek soru kaliyor: dik gorulen bir alan yatay bir kareye nasil otursun?
Iki mantikli cevap var, **ikisi de siyah bant uretmez**:

| Kadraj | Kare | Davranis |
|---|---|---|
| **Doldur** (varsayilan) | hep sectigin cozunurluk, orn. 1280x720 | goruntu kareyi doldurur, tasan yer kirpilir |
| Sigdir | 90/270'te dik uretilir, 720x1280 | hicbir sey kirpilmaz, alici dikey video alir |

Eski davranista bu secim hic yapilmamisti: kare yatay kaliyor ama goruntu ona
sigdiriliyordu, yani 1280 sutunun yalnizca **405'i** doluyor, kalan **%69** siyah
bant oluyordu. Artik iki modda da kaplama **%100**.

Dik montajda "Doldur" secildiginde yakalama boyutu da degisiyor: cikti genisligi
kameranin **kisa** ekseninden geldigi icin 16:9 yerine 4:3 isteniyor — kaynak
bolge 720x405 yerine 1080x608 oluyor, yani %50 daha fazla detay.

Kadrajin kendisi yine fizik: telefon dikey durdugunda kamera gercekten dikey bir
alan goruyor. En genis kadraj icin telefonu **yatay** monte et; o zaman donus
0 veya 180 olur ve hicbir sey kirpilmaz.

**Telefonun ekran yonu telefonun kendi yonunu takip eder.** Once manifestte
`landscape` olarak kilitliydi, sonra `imageRotation`dan turetiliyordu; ikisi de
yanlisti. Turetme ozellikle yaniltici: bu cihazda dik montajin dogru donusu
**0**, yani "donus 0 ise yatay monte edilmistir" varsayimi tam tersini yapiyor
ve uygulama dik tutulurken inatla yatay aciliyordu. Artik sensore birakiliyor.

**Bu cihazda dogru donus 0 - olculdu.** Telefon dik tutulurken ivmeolcer
`(-0.95, 8.85, 3.44)` okudu (yani dogal dik yon) ve karedeki kisi 270 derece
daha cevrilince duz durdu: `(90 + 270) % 360 = 0`. `SENSOR_ORIENTATION` 270
bildiriyor ama tampon dogal yonde zaten duz; ders kitabi formullerinin hepsi
270 tahmin ettigi icin uc ayri denemede de tutmadi. Baska bir cihazda
`owncam-calibrate.sh` ile bir kez olc - kamerayi tavana degil, yonu belli bir
seye (insan, oda) dogrult.

**Onizleme yayina giden karenin aynisi.** Ayni GL baglami, ayni doku, ayni
donus matrisi; sadece hedef yuzey farkli. Siyah bantlar dahil ne gonderiliyorsa
o gorunur. Ikinci bir kamera akisi acilmiyor. Uygulama arka plana gecince
onizleme kapanir, yayin etkilenmez.

**Android 10 kisitlari.** `KEY_LOW_LATENCY` API 30 ile geldi, bu telefonda yok.
Yerine `KEY_PRIORITY = 0` (gercek zamanli) ve **Baseline profil** — Baseline'da
B-kare yok, kodlayici kare biriktiremiyor. API 30+ cihazlarda ek olarak
`KEY_LATENCY` ve `KEY_LOW_LATENCY` de set ediliyor.

**Kare dusurme, tampon buyutme degil.** Android tarafinda gonderim kuyrugu
2 kare; dolarsa **en eski kare atilir** ve hemen anahtar kare istenir. WiFi
tikandiginda gecikme birikmesin diye. ffmpeg tarafinda `nobuffer`,
`low_delay`, `probesize 32`, `analyzeduration 0`, `fps_mode passthrough`.

**Uyanik kalma.** Foreground service + `PARTIAL_WAKE_LOCK` +
`WIFI_MODE_FULL_HIGH_PERF` WiFi kilidi. WiFi guc tasarrufu kare araliklarinda
100 ms'i asan bosluklar yaratiyor.

## Dosyalar

```
android/app/src/main/java/com/owncam/
  MainActivity.kt      onizleme, izin, sayaclar; ekran yonunu donuse uydurur
  StreamService.kt     on plan servisi, wake lock, komutlar, bildirim
  CameraEncoder.kt     Camera2 + MediaCodec, donus ve kare boyutu
  TcpVideoServer.kt    TCP sunucu :5299, kare dusuren gonderim kuyrugu
  MdnsAdvertiser.kt    _owncam._tcp duyurusu
  StatusServer.kt      :5300 durum/teshis ucu (JSON)
  StreamConfig.kt      ayarlar
  gl/EglCore.kt        EGL baglami, kodlayici + onizleme yuzeyleri
  gl/TextureRenderer.kt  OES doku shader'i, tam ekran dortgen
  gl/FrameRenderer.kt  kareyi dondurup iki yuzeye cizen GL is parcacigi

linux/
  owncam-receive.sh          ana alici, yeniden baglanma dahil
  owncam-discover.sh         avahi ile telefonu bulma
  owncam-measure.sh          fps/jitter olcumu (plan bolum 9)
  owncam-status.sh           telefonun ayarlarini PC'den oku
  owncam-snapshot.sh         sanal kameradan tek kare yakala
  owncam.service             systemd kullanici servisi
  install.sh                 scriptleri ve servisi kurar
  obs-use-video11.sh         OBS sahnesini /dev/video11'e cevirir
  relabel-v4l2loopback.sh    cihaz etiketini OwnCam yapar (sudo)
```

## Neden burada duruyoruz

Plan bolum 10: *"Faz 1 sonunda 720p/25fps/<200ms elde edilirse ... 'yeterince
iyi' deyip durmak mesru bir karar."* O esik Faz 1'de degil **Faz 0'da**, ustelik
1080p30 ile asildi.

Faz 1'in gerekcesi "ffmpeg'i cikarip tampon kontrolu almak, yeniden baglanma ve
servislesme eklemek"ti. Ucu de karsilandi: gecikme sorunsuz, ffmpeg %13 tek
cekirdek, yeniden baglanma bash tarafinda calisiyor, systemd servisi kurulu.
Rust alicisi olculebilir hicbir metrigi iyilestirmez.

Faz 2 (UDP/RTP) icin de tetikleyici yok: 0 bosluk, 3 ms jitter.

Geriye kalan tek gercek belirsizlik **uzun sureli dayaniklilik** — telefon bir
saatlik toplantida isinip kare hizini dusuruyor mu, ekran kapaliyken akis
kesiliyor mu. Bunu ancak gercek kullanim gosterir; ilk uzun toplantidan sonra
`owncam-measure.sh 15` tekrar calistirilip yukaridaki tabloyla karsilastirilmali.

Yeni sey eklemek isterseniz plan bolum 8'de kullanilabilirlik maddeleri var
(uyarlanabilir bit hizi, tepsi simgesi, AUR paketi) — hepsi konfor, hicbiri
performans.
