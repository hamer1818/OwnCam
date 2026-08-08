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

### 2.1 Yatay ayna — EKLENDI (ayar); "arıza" tespiti muhtemelen hataliydi

**Onemli duzeltme:** bu maddenin dayandigi "on kamerada tisorttteki yazi ters
okunuyor" gozlemi bana ait ve buyuk olasilikla **yanlis okumaydi** — ayni
turden bir hata bu oturumda tekrar edildi (sandalye kafaligi tisort sanildi).
Camera2 on kamerada gercek sahneyi veriyor; ortada duzeltilecek bir arıza
olmayabilir.

Ayar yine de yerinde: aynali/aynasiz tercihi kullanima gore degisiyor.

`StreamConfig.mirror` eklendi; telefon tarafinda, `FrameRenderer.buildMatrix`
icinde tek isaret degisikligiyle uygulaniyor, kare basina ek maliyet yok.
Masaustunde "Aynala" kutusu, uctan uca `/config?mirror=1`.

Emulatorde olculdu: aynali kare, aynasiz karenin yatay yansimasiyla 8,98
ortalama farkla ortusuyor (sahnenin kendi hareket gurultusu ~5,7), dogrudan
karsilastirmada ise 28,60. Yani ayna gercekten cikti uzayinda uygulaniyor.

**Varsayilan kapali.** Kural basit: karsi tarafin seni gercekte oldugu gibi
gormesi icin **kapali**, kendini aynadaki gibi gormek icin **acik**. Sag elini
kaldirdiginda goruntude sagda cikiyorsa ayna aciktir.

Bir tuzak vardi, dusuldu: donus 0 ve onizleme kapaliyken GL katmani tamamen
atlaniyordu (kamera dogrudan kodlayiciya). Ayna da GL'de uygulandigi icin o
kisayol artik `!config.mirror` de ariyor.

### 2.2 Boru hatti kilitlenmesi — EMULATORDE GECTI, gercek cihazda bekliyor

Suphe: SurfaceView yok edildikten sonra onizleme EGL yuzeyinin vsync'e
kilitlenip `eglSwapBuffers`'ta sonsuz beklemesi. `EglCore.setSwapInterval(0)`
bunun icin eklenmisti ama dogrulanmamisti.

Emulatorde (Pixel 10 Pro XL) sinandi — uc asama da 30 fps'te kilitli ilerledi:

| Durum | Sonuc |
|---|---|
| On planda | 30 fps |
| HOME ile arka planda | 30 fps |
| Ekran kapali | 30 fps |
| Ekran acilip on plana donunce | 30 fps, toparliyor |

12 dakikalik surekli kosuda 0 dusen kare, OwnCam'e ait tek bir EGL hatasi yok.

**Sonra gercek telefonda (Huawei CLT-L09, Android 10) tekrarlandi ve gecti:**

| Durum | Kare hizi |
|---|---|
| On planda (`mCurrentFocus` = OwnCam) | 29,3 fps |
| Arka planda (odak launcher'a gecti) | 28,3 fps |
| Ekran kapali (`mWakefulness=Asleep`) | 28,1 fps |

Uc asama da kilitli ilerledi, dusen kare 0. Gecislerin gercekten oldugu
`dumpsys window` / `dumpsys power` ile **dogrulandi** - ilk denemede adb
baglantisi sessizce dusmus ve tuslar telefona hic gitmemisti, sayaclarin
ilerlemesi de o yuzden yaniltici olmustu.

Madde kapandi. `EglCore.setSwapInterval(0)` calisiyor.

### 2.3 Uzun sureli dayaniklilik — emulatorde 14 dk temiz, telefonda bekliyor

Emulatorde 14 dakika kesintisiz: 25 828 kare, ortalama 29,98 fps, **0 dusen
kare**, bozulma yok. Ama emulator isinmiyor ve termal kisma yapmiyor; asagidaki
soru hâlâ gecerli ve yaniti yalnizca gercek telefon verebilir.

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

### 3.2 Zamansal kararlilik — OLCULDU, is cikmadi

Maske kare kare bagimsiz uretiliyor. Titreyip titremedigi olculdu
(`cargo test --release maske_titremesi -- --ignored --nocapture`): ayni kareye
kare basina bagimsiz algilayici gurultusu eklenip ardisik maskeler
karsilastirildi.

| Gurultu | Ardisik maske farki (ort) | 0,5 esigini atlayan piksel |
|---|---|---|
| ±1/255 | 0,00031 | %0,017 |
| ±3/255 | 0,00079 | %0,055 |

%0,055, 256x256'lik maskede ~36 piksel demek. **Gozle gorulur titreme yok,
zamansal yumusatma eklenmedi** — eklemek gecikmeye bir kare bindirirdi ve
karsiliginda olculebilir bir kazanc yok.

Not: bu bir **alt sinir**. Gercek bir dizide harekete bagli degisim de olur,
ama o istenen degisim; titreme degil.

### 3.3 Efektin islemci bedeli — YUV420 ile %37 dustu

Bedelin segmentasyondan gelmedigi olculmustu (GPU tarafi 1,35 ms); kaynak
ikinci ffmpeg'in RGBA->YUV donusumu ve tam cozunurluklu kareleri borulardan
gecirmekti.

Kompozit artik dogrudan YUV420 uretiyor: geri okunan bayt piksel basina
4'ten 1,5'e iniyor (2,67 kat az) ve ikinci ffmpeg donusum yerine kareyi
oldugu gibi geciriyor.

A/B olcum (emulator kaynagi, 720x1280 @ 30 fps, arka plan bulanik):

| Cikis bicimi | Toplam islemci (owncam + butun ffmpeg) |
|---|---|
| RGBA (onceki) | %15,5 |
| YUV420 (yeni) | **%9,7** |

Renk uzayinda gidip gelmenin hatasi ortalama 0,90/255 — pratikte kayipsiz.

Kalan secenek (yapilmadi): uygulama `/dev/video11`'e dogrudan yazsin
(`VIDIOC_S_FMT` + `write`), ikinci ffmpeg tamamen kalksin. Artik kazanc
daha kucuk ve `unsafe` ioctl gerektiriyor; gerekce zayifladi.

### 3.4 `reduce_mean` cekirdegi — PARALELLESTIRILDI

Kanal basina tek is parcacigi vardi ve H*W uzerinde seri topluyordu; 16-128
parcacikla GPU neredeyse bos duruyordu. Artik is grubu basina bir kanal:
64 parcacik serpistirilmis okuyup paylasilan bellekte agac indirgeme yapiyor.

Olcum: **1,99 -> 1,35 ms/kare** (%32). Maske referansla birebir kaldi.

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
| Odak kilidi | **bitti** — `lockFocus`, AF tetigi ile kilit |
| Uyarlanabilir bit hizi | **bitti** — dusen kare sayaci sinyal, x0,75 in / x1,15 cik |
| Tepsi simgesi | yapilmadi (asagida) |
| Paketleme (AUR PKGBUILD) | yapilmadi (asagida) |

**Uyarlanabilir bit hizi** nasil calisiyor: gonderim kuyrugu iki kare tutup
dolunca en eskisini atiyor, yani dusen kare sayaci artiyorsa kodlayici agin
tasidigindan fazlasini uretiyor demektir. Tikanmanin tanimi bu; ayrica RTT ya
da pencere olcmeye gerek yok. Inis hizli (x0,75, hemen), cikis yavas (x1,15,
5 sakin turdan sonra), taban 800 kbit. Varsayilan **acik**: saglikli agda hic
devreye girmiyor.

**Tepsi simgesi yapilmadi.** Egui'nin tepsi destegi yok; `tray-icon` ya da
`ksni` eklemek gerekiyor ve ikisi de bagimlilik agacini buyutuyor. Kazanci
konfor, bedeli "hafif ikili" sartindan taviz — gerekce zayif.

**AUR paketi yapilmadi.** Once bir surum etiketi ve yayin arsivi gerekiyor;
ayrica AUR'a yuklemek senin hesabinla yapilacak bir is. Depo yeni public oldu,
dogal sirasi bundan sonra.

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

## Geriye ne kaldi

Yukaridaki maddelerin cogu kapandi. Acik kalanlar ve **neden** acik kaldiklari:

| Madde | Durum |
|---|---|
| 2.1 ayna | ayar hazir; varsayilan kapali, yonu kullanima gore sen sec |
| 2.2 kilitlenme | **gercek telefonda dogrulandi**, kapandi |
| 2.3 dayaniklilik | olcum kosuyor (gercek telefon, ekran kapali, tuketici bagli) |
| 3.1 model kapsami | uyari eklendi; daha iyi model (or. RVM) denenmedi |
| 3.2 titreme | olculdu, is cikmadi |
| 3.3 islemci bedeli | YUV420 ile %15,5 -> %9,7 |
| 3.4 reduce_mean | paralellestirildi, 1,99 -> 1,35 ms |
| 5. odak kilidi | bitti, cihazda dogrulama bekliyor |
| 5. uyarlanabilir bit hizi | bitti, cihazda dogrulama bekliyor |
| 5. tepsi simgesi, AUR paketi | bilerek yapilmadi |

Acik kalan tek olcum 2.3. Kalan tek kod isi, yeni APK'nin telefonda
dogrulanmasi (odak kilidi + uyarlanabilir bit hizi).

Sonraki adim icin en degerli aday **3.1**: model bas-omuz cercevesi disinda
zayif. RVM gibi daha iyi bir model kalitesi artirir ama 1,35 ms'lik butceyi
buyutur — once olc, sonra karar ver.
