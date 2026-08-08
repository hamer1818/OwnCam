package com.owncam

import android.hardware.camera2.CameraCharacteristics
import java.io.Serializable

/**
 * Akis ayarlari. Servise Intent ile gecirilir.
 *
 * Varsayilanlar plandaki Faz 0 hedefine gore secildi: 720p30 @ 8 Mbit.
 */
/**
 * Kamera **her zaman yatay kare uretir** ve bu degistirilemez: sensor telefonun
 * govdesine sabit, `SCALER_STREAM_CONFIGURATION_MAP` yalnizca yatay boyutlar
 * sunar, Camera2 goruntuyu dondurmez. Telefonu dik tuttugunda o yatay tampon
 * dunyada dik bir alani gosterir - yani "yatay geliyor" olan sey tamponun sekli,
 * kadrajin sekli degil.
 *
 * Geriye tek soru kaliyor: dik gorulen bir alan, yatay bir kareye nasil otursun?
 * Iki mantikli cevap var, [FILL] ve [FIT]. Ucuncu bir secenek yok ve **hicbiri
 * siyah bant uretmez** - eski davranistaki bantlar bu secimin yapilmamis
 * olmasindan geliyordu.
 */
enum class FrameMode(val key: String, val label: String) {
    /**
     * Kare **telefonun fiziksel yonunu** takip eder: dik tutunca dikey kare
     * (720x1280), yan tutunca yatay kare (1280x720). Goruntu kareyi doldurur,
     * tasan yer kirpilir.
     *
     * **Varsayilan** - beklenen davranis bu: "dikeyde dikey, yatayda yatay".
     *
     * Cogu telefonda bedeli sifirdir; kadraj zaten kareyle ayni oranda gelir ve
     * kirpilacak bir sey olmaz. Hedef cihazda (CLT-L09 on kamera) kadraj ters
     * eksende geliyor - dik tutunca genis goruyor - o yuzden burada kirpma
     * gercekten oluyor. Kaybi azaltmak icin bu modda kamera 16:9 yerine 4:3
     * calistiriliyor: buyutme 1.78x yerine ~1.19x kaliyor.
     */
    FILL("telefona-uy", "Telefona uy (dikeyde dikey)"),

    /**
     * Kare **kadraji** takip eder: hicbir sey kirpilmaz, kameranin gordugu her
     * piksel gonderilir. Kare sekli telefonun yonuyle ayni olmayabilir.
     *
     * Tam goruntu alani lazimsa bunu sec.
     */
    FIT("tam-kadraj", "Tam kadraj (kirpma yok)");

    companion object {
        fun from(key: String?): FrameMode =
            entries.firstOrNull { it.key == key } ?: FILL
    }
}

data class StreamConfig(
    val width: Int = 1280,
    val height: Int = 720,
    val bitRate: Int = 8_000_000,
    val frameRate: Int = 30,
    val lensFacing: Int = CameraCharacteristics.LENS_FACING_FRONT,
    /**
     * Otomatik pozlama kilidi. Pencereden gelen arka isik kameranin surekli
     * pozlama aramasina sebep oluyorsa acilir (plan bolum 8).
     */
    val lockExposure: Boolean = false,
    /**
     * Goruntuye uygulanacak saat yonundeki donus: 0, 90, 180, 270.
     *
     * Sensor acisi + cihaz yonundan **hesaplanmiyor**. Uc ayri formul denendi,
     * hicbiri hedef cihazda tutmadi; sensor acisinin uretici tarafindan nasil
     * raporlandigi cihazdan cihaza degisiyor. Dogrudan secmek hem kesin hem de
     * her cihazda calisiyor - kullanici bir kez secer, ayar kaydedilir.
     *
     * Hedef cihazda (CLT-L09 on kamera) olculen dogru deger **0**: telefon dik
     * tutuldugunda goruntu zaten duz geliyor. `SENSOR_ORIENTATION` 270 bildirse
     * de tampon dogal yonde zaten duz - o yuzden hicbir formul tutmuyordu.
     * Baska cihazda `owncam-calibrate.sh` ile bir kez olcup kaydet.
     */
    val imageRotation: Int = 0,
    /**
     * Uygulama icinde onizleme. Kapaliyken ve donus 0 iken kamera dogrudan
     * kodlayiciya baglanir - GL katmani hic devreye girmez. Varsayilan kapali:
     * kanitlanmis yol varsayilan olsun, ek katman istege bagli kalsin.
     */
    val preview: Boolean = true,
    /**
     * Donusu telefonun fiziksel yonunden otomatik belirle.
     *
     * Olculen kural (bkz. README): **imageRotation = cihaz yonu**, yani
     * `OrientationEventListener`in saat yonundeki degeri, 90'a yuvarlanmis.
     * Iki bagimsiz olcumle dogrulandi: dik tutuşta (cihaz 0) dogru deger 0,
     * sola yatik tutuşta (cihaz 270) dogru deger 270.
     *
     * Telefon duz yatarken yon okunamiyor (`ORIENTATION_UNKNOWN`); o durumda
     * **son bilinen deger korunuyor**. Webcam zaten oyle duruyor, dolayisiyla
     * "duz yatinca bayatliyor" bir kusur degil, istenen davranis.
     */
    val autoRotate: Boolean = true,
    /** Kadrajin kareye nasil oturacagi; bkz. [FrameMode]. */
    val frameMode: FrameMode = FrameMode.FILL,
    /**
     * Goruntuyu yatayda ters cevir.
     *
     * Camera2 on kamerada **aynalanmamis** kare veriyor: lensin gordugu
     * gercek sahne. Kendini izlerken bu ters hissettiriyor (aynaya alisigiz),
     * ama karsi tarafa giden goruntude uzerindeki yazi dogru okunuyor.
     *
     * Hangisinin dogru oldugu kullanima gore degisiyor, o yuzden ayar.
     * Varsayilan **kapali**: yayina giden kare gercege sadik kalsin.
     */
    val mirror: Boolean = false,
    val port: Int = DEFAULT_PORT
) : Serializable {

    val label: String get() = "${width}x$height @ ${frameRate}fps, ${bitRate / 1_000_000} Mbit"

    companion object {
        const val DEFAULT_PORT = 5299
        private const val serialVersionUID = 1L
        private const val PREFS = "owncam"

        /**
         * Ayarlar masaustunden degistiriliyor; telefon yeniden baslatildiginda
         * kaybolmamalari icin kalici olarak saklaniyor.
         */
        fun load(context: android.content.Context): StreamConfig {
            val p = context.getSharedPreferences(PREFS, android.content.Context.MODE_PRIVATE)
            val d = StreamConfig()
            return d.copy(
                width = p.getInt("width", d.width),
                height = p.getInt("height", d.height),
                bitRate = p.getInt("bitRate", d.bitRate),
                frameRate = p.getInt("frameRate", d.frameRate),
                lensFacing = p.getInt("lensFacing", d.lensFacing),
                lockExposure = p.getBoolean("lockExposure", d.lockExposure),
                imageRotation = p.getInt("imageRotation", d.imageRotation),
                preview = p.getBoolean("preview", d.preview),
                autoRotate = p.getBoolean("autoRotate", d.autoRotate),
                frameMode = FrameMode.from(p.getString("frameMode", d.frameMode.key)),
                mirror = p.getBoolean("mirror", d.mirror)
            )
        }

        fun save(context: android.content.Context, config: StreamConfig) {
            context.getSharedPreferences(PREFS, android.content.Context.MODE_PRIVATE)
                .edit()
                .putInt("width", config.width)
                .putInt("height", config.height)
                .putInt("bitRate", config.bitRate)
                .putInt("frameRate", config.frameRate)
                .putInt("lensFacing", config.lensFacing)
                .putBoolean("lockExposure", config.lockExposure)
                .putInt("imageRotation", config.imageRotation)
                .putBoolean("preview", config.preview)
                .putBoolean("autoRotate", config.autoRotate)
                .putBoolean("mirror", config.mirror)
                .putString("frameMode", config.frameMode.key)
                .apply()
        }

        /**
         * Secilebilir donus degerleri. Ivmeolcer devrede degil: telefon bir
         * stant uzerinde ya da masada duz dururken `OrientationEventListener`
         * yonu hic okuyamiyor ve son bilinen deger bayatliyordu. Webcam hep
         * ayni sekilde durdugu icin bir kez secmek yeterli.
         *
         * 90/270 secilirse telefon dik monte edilmis demektir; kadrajin kareye
         * nasil oturacagini [FrameMode] belirler. Kadrajin kendisi fizik,
         * yazilim genisletemez.
         */
        val ROTATIONS = listOf(0, 90, 180, 270)

        val RESOLUTIONS = listOf(
            640 to 480,
            1280 to 720,
            1920 to 1080
        )

        /** Cozunurluge gore makul bir varsayilan bit hizi. */
        fun defaultBitRate(width: Int, height: Int): Int = when {
            width * height >= 1920 * 1080 -> 12_000_000
            width * height >= 1280 * 720 -> 8_000_000
            else -> 3_000_000
        }
    }
}
