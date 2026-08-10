# OwnCam masaustu uygulamasi

Telefondan gelen goruntuyu gosteren, sanal kameraya besleyen ve **arka plani
ekran kartinda silen** Linux uygulamasi.

```bash
cargo build --release        # target/release/owncam  (10,4 MB)
cargo test                   # 54 test
./target/release/owncam      # telefonu mDNS ile bulur
./target/release/owncam 192.168.1.105   # IP elle
```

Ortam degiskenleri: `OWNCAM_HOST`, `OWNCAM_DEVICE`, `OWNCAM_EFFECT`
(`bulanik` / `renk` / `foto`), `OWNCAM_EFFECT_PHOTO`, `OWNCAM_SEG_BOYUT`,
`OWNCAM_MODEL` (kaliteli agin ONNX yolu), `OWNCAM_ORAN`.

`OWNCAM_SEG_BOYUT` segmentasyon aginin girdi olcusu (32'nin kati, varsayilan
256). Buyutmek maske cozunurlugunu artirip kenardaki basamaklari azaltiyor
ama **her sahnede iyi degil** - detayli 720p kaynakta 384 kazanc, dusuk
detayli kaynakta zarar. Olcumler `YAPILACAKLAR.md` 3.7'de.

**Kapsam Linux.** Windows sanal kamerasi (DirectShow) ve Windows derlemesi
kapsam disi birakildi.

## Neden Rust + egui

Sart listesi: **kucuk ikili**, **hizli video**, **ucretsiz**.

| Aday | Boyut | Elenme sebebi |
|---|---|---|
| Electron | 150–200 MB | Boyut sartini tek basina ihlal ediyor |
| Tauri (webview) | ~10 MB | 30 fps ham kareyi webview'a sokmak zayif nokta: canvas/base64 yavas, MSE/WebCodecs karmasik |
| Qt6 | orta | LGPL statik baglama ileride ticarilesirse tuzak |
| GTK4 (onceki surum) | — | Python/GTK kabugu; dagitimi ve paketlemesi agir |
| Go / Fyne | 20–30 MB | Video doku aktarimi zayif |
| **Rust + egui** | **10,4 MB** | Tek statik ikili, runtime yok, MIT/Apache |

`eframe` **glow** (OpenGL) arka ucuyla derleniyor. Segmentasyon ise ayri,
**penceresiz** bir wgpu cihazinda kosuyor: isleme arayuzun cizim dongusunden
bagimsiz bir is parcaciginda yurusun diye. Pencere kucultulunce ya da arka
plana atilinca sanal kameranin durmasi boyle onleniyor.

## Cozme hattina dokunulmadi

ffmpeg alt surec olarak kaliyor. Faz 0'da olculen degerler (3 ms jitter,
0 bosluk, tek cekirdegin %13'u) zaten hedefin altinda; degistirmek icin
olculebilir bir gerekce yok (bkz. ana README, "Neden burada duruyoruz").

## Iki boru hatti, secim efekte gore

```text
efekt kapali   telefon -> ffmpeg -+-> sanal kamera      (tek surec, iki cikis)
                                  +-> ham RGBA -> pencere

efekt acik     telefon -> ffmpeg -> ham RGBA -> GPU -+-> ffmpeg -> sanal kamera
                                   (segmentasyon +   +-> onizleme -> pencere
                                    kompozit)
```

Kapali yol **degismedi**: olculmus dusuk gecikmeli yol o, ve efekt istemeyen
kullanici onun bedelini odemesin. Efekt acilinca kareler tam cozunurlukte
uygulamadan geciyor; boru hattinin sekli degistigi icin alici yeniden
kuruluyor. Kalan ayarlar (bulaniklik, renk, kenar sertligi, foto) akisi
kesmeden canli uygulaniyor.

Onizleme **alana** gore olcekleniyor, genislige gore degil: dik karede
(720x1280) genislige gore olceklemek 640x1138'lik bir onizleme uretip boru
trafigini uce katliyordu.

## Arka plan silme

Iki ag secilebiliyor ve ikisi de **ayni** calisma zamanindan geciyor:

| | hizli (varsayilan) | kaliteli |
|---|---|---|
| Ag | MediaPipe Selfie Segmentation | Robust Video Matting (MobileNetV3) |
| Lisans | Apache-2.0 | GPL-3.0 |
| Depoda | evet, 462 KB gomulu | **hayir**, kullanici indiriyor |
| Girdi | 256x256 kare | kare cozunurlugu (icinde kuculuyor) |
| Cikti | maske, 256x256 | alfa + on plan, tam cozunurluk |
| Kareler arasi | bagimsiz | gizli durum tasiyor |
| Cikarim (1280x720) | 1,0 ms | 8,2 ms |
| Tam kompozit | 1,9 ms | 13,7 ms |

Olculen fark:

- **Kenar.** Hizli ag saci kesiyor ve kenarda yeni arka planin rengiyle bir
  hale birakiyor; RVM sac tellerini ve kulaklik kablosunu koruyor. RVM'nin
  `fgr` ciktisi kompozitte kullaniliyor - maskeleme ile matting'in farki bu:
  yari saydam bir pikselde **eski** arka planin rengi ayikliyor.
- **Titreme.** Ayni sahneye kare basina bagimsiz gurultu eklenip ardisik
  maske farki olculdu, ikisi de kare cozunurlugune buyutulerek: hizli
  0,00045, RVM 0,00025. Gercek ama olculu bir kazanc - RVM'nin asil ustunlugu
  kenar kalitesi.
- **Hiz.** Canli, telefon 1280x720 @ 30 fps: sanal kamera 29,5 fps, uygulama
  %5 islemci. Tam cozunurlukte RVM kare hizini dusurmuyor.

RVM agirliklari depoda **yok** ve olamaz: RVM GPL-3.0, OwnCam MIT. Kod
agirlik dagitmiyor, yalnizca yol aliyor:

```bash
# rvm_mobilenetv3_fp32.onnx dosyasini RVM'nin kendi surumlerinden indirin
OWNCAM_MODEL=/yol/rvm_mobilenetv3_fp32.onnx ./target/release/owncam 192.168.1.105
```

Arayuzde de "Arka plan > Ag" bolumunden secilebiliyor. `OWNCAM_ORAN` ag
govdesinin kareyi kucultme orani (varsayilan 0,375; RVM 1080p icin 0,25
oneriyor).

Ikisi de kendi WGSL cekirdeklerimizle kosuyor. Hazir cikarim kutuphanesi
kullanilmadi ve sebebi olculdu:

| Yol | Kare basina | Ikiliye etkisi |
|---|---|---|
| `tract` (ONNX, islemci) | **32 ms** | +35 MB |
| kendi WGSL cekirdeklerimiz | **1,35 ms** | +4,2 MB |

30 fps'in kare butcesi 33 ms. Islemcideki cozum butceyi tek basina doldurup
bir cekirdegi tam mesgul ediyordu; GPU yolu butcenin %4'unu kullaniyor ve
Vulkan/OpenGL uzerinden her ekran kartinda calisiyor - NVIDIA'ya bagli degil.

Modulun bolumleri:

- `seg/onnx.rs` - asgari protobuf okuyucu. Protobuf tel bicimi kendini tarif
  ettigi icin sema uretimi olmadan guvenle gezilebiliyor. Yuklemede agirlik
  butunlugu denetleniyor: sekil carpimi ile ham bayt uzunlugu tutmazsa dosya
  reddediliyor, sessizce yanlis sonuc uretilmiyor.
- `seg/plan.rs` - girdi olcusu yuklemede belli oldugu icin butun sekiller,
  dolgu degerleri ve tampon kaymalari bir kez hesaplaniyor. `Constant`,
  `Shape`, `Slice` ve `Concat` dugumleri sayi olarak katlaniyor, GPU'ya hic
  gitmiyorlar. Selfie modelinde 145 dugumun 136'si sevke doniyor, RVM'de
  353 dugum 296 sevke.

  Iki dugum **hic** sevk uretmiyor cunku yalnizca kayma aritmetigi: kanal
  ekseninde `Split` (NCHW'de zaten bitisik) ve gizli durumun `Expand`'i
  (durumu tam boyutta tuttugumuz icin takma ad, kopya degil).

  RVM'nin `downsample_ratio` girdisi yuklemede sabite ceviriliyor
  (`Graph::set_input_constant`); boylece butun sekiller statik cozuluyor ve
  model dosyasini yamalamak gerekmiyor.
- `seg/seg.wgsl` - evrisim (gruplu, genislemeli, istege bagli yanlilik),
  transpoze evrisim, keyfi olcekli iki dogrusal olcekleme, havuzlama,
  indirgemeler, yayinli ikili islemler ve eleman bazli cekirdekler. Kuresel
  ortalama is grubu basina bir kanal alip paylasilan bellekte agac indirgeme
  yapiyor. Butun ara tensorler tek bir arena tamponunu (hizli agda 30 MB,
  RVM'de 1280x720 icin 299 MB), butun agirliklar tek bir tamponu paylasiyor;
  her sevk yalnizca kayma tasiyor. Boylece butun adimlar icin tek baglama
  grubu yetiyor.
- Gizli durumlar arenada **kalici** yer tutuyor; kare sonunda `rNo` bolgesi
  `rNi` bolgesine kopyalaniyor. Geri beslemenin calistigi olculdu: ayni kare
  tekrar verilince ardisik maske farki 0,0060'tan 0,000023'e sonumleniyor
  (kopuk olsaydi bastan 0 olurdu).
- `seg/effects.wgsl` - kompozit. Bulanik arka plan **ceyrek cozunurlukte**
  hesaplaniyor (ayrilabilir Gauss, iki gecis), sonra iki dogrusal
  buyutuluyor: tam cozunurlukte 16 kat daha pahali ve gozle ayirt edilmiyor.
  Foto arka plan kareyi **kaplayacak** sekilde olcekleniyor, sigdirilmiyor -
  sigdirmak siyah kenar birakirdi.

### Dogruluk nasil denetleniyor

`tests/fixtures/` icindeki demirbaslar bagimsiz bir ONNX calisma zamaniyla
(`tract`) uretildi: gercek bir portre 256x256'ya olceklenip agdan gecirildi.
GPU cekirdeklerimiz bu maskeye karsi olculuyor - **en buyuk fark 0,0020,
ortalama 0,00004**. Demirbasin u8 niceleme tabani 1/255 = 0,0039 oldugundan
fark olcum gurultusunun altinda.

RVM ayni yontemle dogrulandi: gercek bir 1280x720 kare, ayni tract kosusuna
karsi **en buyuk fark 0,0039, ortalama 0,000093** - en buyuk fark tam olarak
referansin nicemleme tabani. 296 sevkin hepsi, yeni operatorler dahil.
Agirliklar depoya girmedigi icin bu bir birim testi degil, elle calistirilan
bir kanca:

```bash
OWNCAM_MODEL=/yol/rvm.onnx OWNCAM_HAM=/tmp/ham_rgb.bin OWNCAM_GIRDI=1280x720 \
  OWNCAM_CIKTI=/tmp/gpu_alpha.f32 \
  cargo test --release yabanci_model_kosusu -- --ignored --nocapture
```

Kardesi `yabanci_model_plani` yalnizca plani kuruyor ve desteklenmeyen ilk
operatoru tek satirda soyluyor; yeni bir model denerken islerin sirasi bu.

Bu denetim ucuz degil, gerekliydi: modeli elle uygularken uc ayri operator
yorumu denendi ve hangisinin dogru oldugu ancak bagimsiz bir referansla
anlasildi. Ozellikle iki ayrinti sezgiye aykiri:

- ONNX surumunde `Resize` **half_pixel** koordinat donusumu kullaniyor;
  ayni modelin TFLite surumu asimetrik kullaniyor. Ikisi ayni sonucu vermiyor.
- `ConvTranspose` cekirdeginin duzeni `[C_in, C_out, kh, kw]`; TFLite'taki
  karsiligi `[C_out, kh, kw, C_in]`. Yanlis duzen, gozle "dikey seritli"
  bir maske uretiyor - sessiz degil ama kaynagi belirsiz bir bozulma.

### Modelin sinirlari

Bu bolum **hizli** ag icin. Ag "selfie" cercevesi icin egitilmis: bas ve
omuzlar, arkada ayirt edilebilir bir arka plan. Asiri yakin cekimde - yuzun kareyi doldurdugu, arkasinin duz
beyaz duvar oldugu bir karede - maskeyi neredeyse bos uretiyor. Bu bir hata
degil, modelin kapsami. Kamerayi bas-omuz cercevesine alinca sorun kalkiyor.

## Sanal kamera - takilabilir katman

Cikis bir arayuzun (`sink::Sink`) arkasinda; bugun tek gerceklemesi
`v4l2loopback`. Sanal kamera bulunamazsa uygulama **izleyici** olarak calisir,
sessizce bozulmaz.

Cihaz secimi ada gore, alfabetik siraya gore **degil**: bu sistemde
`/dev/video0` DroidCam'den kalma "Loopback video device", `/dev/video11` ise
"OwnCam". Alfabetik siralamak yanlis cihazi seciyordu. OBS'in kendi sanal
kamerasi da disarida: zincir `telefon -> video11 -> OBS -> video10` seklinde,
biz zincirin basindayiz.

Cihaz yoklamasi "yazmayi dene" ile yapilmiyor: `exclusive_caps=1` ile cihaz o
an baska bir uretici tarafindan tutuluyorsa acilis basarisiz oluyor ve cihaz
hic yokmus gibi eleniyordu. Mesgulluk gecici, varlik degil.

## Kare boyutu dinamik

Otomatik donus acikken telefon fiziksel yonune gore kare seklini degistiriyor
(1280x720 <-> 720x1280). `-vf scale` boyutu sabit tuttugundan alici, telefonun
**bildirdigi** boyut degistiginde yeniden kuruluyor. Tahmin etmek mumkun degil;
durum ucundan okunuyor.

## Goruntu dosyasi okuma

Arka plan fotografi ffmpeg/ffprobe ile cozuluyor, goruntu kutuphanesi
eklenmedi: ffmpeg zaten zorunlu bagimlilik ve PNG/JPEG/WebP/AVIF hepsini
cozuyor. Dosya secici de alt surec (`zenity`, `kdialog`) - `rfd` ya GTK3'e
ya da portal + async calisma zamanina bagliyor. Secici bulunamazsa arayuzdeki
metin kutusu yolu elle almaya devam ediyor.

## Olculen degerler

Bu sistemde (RTX 5080, Vulkan), telefon 1280x720 @ 30 fps, arka plan bulanik:

| Olcu | Efekt kapali | Efekt acik (bulanik) |
|---|---|---|
| segmentasyon | — | 1,35 ms/kare |
| tam boru hatti (yukleme + ag + bulanik + kompozit + geri okuma) | — | ~2,1 ms/kare |
| islemci (owncam + butun ffmpeg surecleri) | tek cekirdegin %12,7'si | tek cekirdegin %21,5'i |
| ^ ayni olcum YUV420 ciktisindan **once** | — | (bkz. asagidaki A/B) |
| GPU | %4 | %21 |
| telefonda dusen kare | 0 | 0 |

Efektin bedeli **+17 puan GPU**. Islemci tarafindaki artis segmentasyondan
degil, ikinci ffmpeg surecinden ve kareleri borulardan gecirmekten geliyordu;
kompozit dogrudan YUV420 uretmeye baslayinca buyuk olcude kapandi:

| Cikis bicimi | Toplam islemci (emulator kaynagi, 720x1280, bulanik) |
|---|---|
| RGBA | %15,5 |
| **YUV420** | **%9,7** |

Geri okunan bayt piksel basina 4'ten 1,5'e iniyor ve ikinci ffmpeg donusum
yapmak yerine kareyi oldugu gibi geciriyor. Renk uzayinda gidip gelmenin
hatasi ortalama 0,90/255 - pratikte kayipsiz.

| | |
|---|---|
| ikili | 10,4 MB |
| model | 462 KB |
| test demirbaslari | 256 KB |
| arena (GPU) | 30 MB |

## Durum

- Linux: calisiyor, cihazda dogrulandi - telefondan `/dev/video11`'e kadar
  butun zincir, arka plan bulanik olarak kare yakalanarak.
- mDNS kesfi saf Rust (`mdns-sd`), arka planda surekli tariyor.
