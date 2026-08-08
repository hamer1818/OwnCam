# Yapilacaklar

Durum: 2026-08-07. Her madde depoda dogrulandi; tahmin olanlar oyle
isaretlendi. Sira onem sirasi, kolaylik sirasi degil.

---

## 1. Once bu: surum kontrolu yok

**Depo bir git deposu degil.** `git rev-parse` "bir git deposu degil" doniyor.
Su an 4330 satir Rust, ~2500 satir Kotlin, 10 bash betigi ve 462 KB model
tek kopya hâlinde diskte duruyor. Yanlislikla silinen bir dosyanin geri donusu
yok, "dun calisiyordu" sorusunun cevabi yok.

```bash
cd /mnt/veri/yazilim/OwnCam
git init && git add -A && git commit -m "OwnCam: ilk kayit"
```

Once kok dizine `.gitignore` gerekiyor (bkz. madde 4) — yoksa `desktop/target`
ve APK ciktilari da girer.

Bu maddeyi digerlerinin onune koymamin sebebi: asagidaki her degisiklik
geri alinabilir olmali.

---

## 2. Bilinen kusurlar

### 2.1 On kamera goruntusu ayna degil

Telefonun on kamerasi kullanilirken goruntu **aynalanmiyor**. Uzerindeki
yazi ters okunuyor. Kodun hicbir yerinde ayna islemi yok — `mirror` ya da
`ayna` gecen tek satir bulunmuyor.

Dogru davranis ayrik: kendini izlerken ayna (dogal), karsi tarafa giderken
duz. Bu yuzden **ayar olmali, sabit deger degil.**

Ucuz cozum: `FrameRenderer.buildMatrix` icinde X eksenini ters cevirmek —
GL matrisinde tek isaret degisikligi, kare basina ek maliyet sifir.
Alternatif olarak masaustunde kompozit shader'inda da yapilabilir ama o zaman
efekt kapaliyken calismaz; **telefon tarafi dogru yer.**

Dogrulama: `owncam-snapshot.sh` ile kare alip uzerinde yazi olan bir sey tut.

### 2.2 Boru hatti kilitlenebiliyor — duzeltme cihazda dogrulanmadi

`CLAUDE.md` "Known issues"ta duruyor: kare uretimi ~10 karede durmus, uygulama
sureci yasarken. Suphe edilen sebep, SurfaceView yok edildikten sonra onizleme
EGL yuzeyinin vsync'e kilitlenip `eglSwapBuffers`'ta sonsuz beklemesi.

`EglCore.setSwapInterval(0)` bunun icin eklendi ama **cihazda dogrulanmadi.**

Dogrulama yolu belli: uygulamayi arka plana at / ekrani kapat, 10-15 dakika
bekle, sonra `owncam-status.sh` ile asama sayaclarina bak
(`cameraFrames` -> `glDraws` -> `encoderOutputs` -> `framesSent`). Hangisi
duruyorsa tikanma orada.

Dogrulanana kadar gecerli gecici cozum: uygulamayi on planda tut.

### 2.3 Uzun sureli dayaniklilik olculmedi

`README.md` "Neden burada duruyoruz" bolumunun kendi tespiti: geriye kalan tek
gercek belirsizlik uzun sureli davranis. Telefon bir saatlik toplantida isinip
kare hizini dusuruyor mu, bilmiyoruz.

```bash
owncam-measure.sh 15     # ilk uzun toplantidan hemen sonra
```

Sonucu plan bolum 2'deki tabloyla karsilastir. Bu bir "yapilacak is" degil,
**yapilacak olcum** — sonucuna gore is cikabilir de cikmayabilir de.

---

## 3. Arka plan silme: bilinen sinirlar

### 3.1 Modelin kapsami dar

Ag "selfie" cercevesi (bas + omuzlar) icin egitilmis. Yuzun kareyi doldurdugu
asiri yakin cekimde maskeyi neredeyse bos uretiyor — bu oturumda gercek bir
karede goruldu. **Hata degil, kapsam.**

Secenekler, ucu de denenmedi:

- Kullaniciya "kisi bulunamadi" uyarisi goster (maskenin ortalamasi esigin
  altindaysa). Ucuz, durustce bilgilendirir.
- 144x256'lik yatay model surumunu yatay karelerde kullan. Model dosyasi
  ayni aileden, plan altyapisi zaten degisken girdi olcusunu kaldiriyor.
- Daha buyuk bir model (or. RVM). Kalite artar, 1,99 ms'lik butce buyur —
  once olc, sonra karar ver.

### 3.2 Zamansal kararlilik olculmedi

Maske kare kare bagimsiz uretiliyor; ardisik karelerde titreme olup olmadigi
**olculmedi**. Tek karelik testler bunu gostermez.

Olcum: sabit sahnede 100 kare topla, ardisik maskelerin farkinin ortalamasina
bak. Titreme varsa cozumu ucuz — onceki maskeyle ussel ortalama (bir satir
shader), ama gecikmeye 1 kare ekler. Olcmeden ekleme.

### 3.3 Efektin islemci bedeli gereginden yuksek

Olculdu: efekt acikken toplam islemci %12,7 -> %21,5. Artis segmentasyondan
**degil** — GPU tarafi 1,99 ms. Bedel ikinci ffmpeg surecinden ve tam
cozunurluklu RGBA kareleri borulardan gecirmekten geliyor.

Iki yol var, ikisi de denenmedi:

- Kompozit shader'i RGBA yerine dogrudan YUV420 uretsin: boru trafigi yariya
  iner, ikinci ffmpeg donusum yapmaz.
- Uygulama `/dev/video11`'e dogrudan yazsin (`VIDIOC_S_FMT` + `write`),
  ikinci ffmpeg tamamen kalksin. Daha az surec ama `unsafe` ioctl gerekir.

Ilki daha az risk, once o denenmeli.

### 3.4 `reduce_mean` cekirdegi GPU'yu bos birakiyor

Kanal basina tek is parcacigi, H*W uzerinde seri toplam. 16-128 parcacikla
GPU neredeyse bos duruyor. Toplam butce zaten karsilandigi icin **acil degil**;
butce sikisirsa ilk bakilacak yer burasi (paralel indirgeme).

---

## 4. Temizlik

- **Kok `.gitignore` yok.** En az: `desktop/target/`, `linux/__pycache__/`,
  `android/build/`, `android/.gradle/`, `android/local.properties`.
  (`desktop/` ve `android/` kendi `.gitignore`larina sahip, kok dizin degil.)
- **`linux/__pycache__/owncam-desktop.cpython-314.pyc`** — derlenmis Python
  cikitisi, depoda isi yok.
- **`linux/owncam-desktop.py`** artik kullanilmiyor; yerini Rust uygulamasi
  aldi. Silinsin mi kalsin mi **senin kararin** — silmedim.
- `.directory` (KDE klasor ayari) da depoya ait degil.

---

## 5. Plandaki kullanilabilirlik maddeleri (plan bolum 8)

Kodda dogrulanan durum:

| Madde | Durum |
|---|---|
| Ayar ekrani (cozunurluk, bit hizi, on/arka kamera) | **bitti** — masaustu uygulamasinda |
| Otomatik pozlama kilidi | **bitti** — `exposureLocked` |
| Odak kilidi | yok |
| Uyarlanabilir bit hizi | yok — bit hizi ayarlanabiliyor ama WiFi zayiflayinca kendiliginden dusmuyor |
| Tepsi simgesi | yok |
| Paketleme (AUR PKGBUILD) | yok |

Hicbiri performansi etkilemiyor, hepsi konfor. Uyarlanabilir bit hizi
digerlerinden daha degerli: WiFi zayifladiginda su an kare atiliyor, oysa
kaliteyi dusurmek daha az rahatsiz edici olurdu.

---

## 6. Bilerek yapilmayacaklar

Bunlar unutuldugu icin degil, **karar verildigi icin** listede yok. Yeniden
onermeden once gerekcenin degistigini goster.

- **Faz 1 (kendi Rust alicimiz).** Faz 0 olcumleri nihai hedeflerin hepsini
  gecti: 29,6 fps, 3 ms jitter, 0 bosluk. Rust alicisi olculebilir hicbir
  metrigi iyilestirmez. Bkz. README "Neden burada duruyoruz".
- **Faz 2 (UDP/RTP, WebRTC).** Tetikleyici yok: 0 bosluk, 3 ms jitter.
- **Windows destegi.** Kullanici kapsam disi birakti — ne capraz derleme ne
  DirectShow sanal kamerasi.
- **`SENSOR_ORIENTATION`'dan donus turetmek.** Bu cihazda o deger yaniltici;
  uc ayri turetme denemesi bu yuzden basarisiz oldu. Aci **olculur**, bkz.
  `owncam-calibrate.sh`.
- **Hazir cikarim kutuphanesi (tract/ort) ile segmentasyon.** Olculdu:
  islemcide 32 ms (30 fps butcesi 33 ms) ve ikiliye +35 MB.

---

## Sirasi gelince nereden baslamali

1. `git init` (madde 1) — geri kalani geri alinabilir yapar.
2. Ayna ayari (2.1) — kucuk, gorunur, kullanicinin isine yariyor.
3. Kilitlenme dogrulamasi (2.2) — is degil, gozlem; ama cevabi mimariyi
   etkileyebilir.
4. Uzun sureli olcum (2.3) ve maske titremesi (3.2) — ikisi de once olcum.
