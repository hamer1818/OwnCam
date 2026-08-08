package com.owncam.gl

import android.graphics.SurfaceTexture
import android.opengl.EGLSurface
import android.opengl.GLES20
import android.opengl.Matrix
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import android.util.Size
import android.view.Surface
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

/**
 * Kamera ile kodlayici arasindaki GL katmani.
 *
 * Kamera artik dogrudan MediaCodec'in yuzeyine degil, buradaki
 * SurfaceTexture'a ciziyor. Her kare bir kez GPU'ya gelip iki yere ciziliyor:
 *
 *   1. kodlayicinin giris yuzeyi (yayina giden kare)
 *   2. varsa ekrandaki onizleme
 *
 * Bu ara katmanin iki gerekcesi var: dondurmeyi uygulayabilmek ve onizlemenin
 * **birebir gonderilen kareyi** gostermesi. Goruntu hala CPU'ya hic ugramiyor,
 * her sey GPU uzerinde kaliyor.
 */
class FrameRenderer(
    private val encoderSurface: Surface,
    private val cameraSize: Size,
    private val outputSize: Size
) {

    private val thread = HandlerThread("owncam-gl").apply { start() }
    private val handler = Handler(thread.looper)

    private var eglCore: EglCore? = null
    private var encoderEglSurface: EGLSurface? = null
    private var previewEglSurface: EGLSurface? = null
    private var previewSize = Size(0, 0)

    private val renderer = TextureRenderer()
    private var surfaceTexture: SurfaceTexture? = null

    /** Kamera oturumunun hedef alacagi yuzey. */
    lateinit var cameraSurface: Surface
        private set

    /** Goruntuye uygulanacak saat yonunde donus: 0, 90, 180, 270. */
    @Volatile
    var rotation: Int = 0

    /** true ise icerik kareyi doldurup tasar (kirpilir), false ise kareye sigdirilir. */
    @Volatile
    var crop: Boolean = false

    private val texMatrix = FloatArray(16)
    private val mvpMatrix = FloatArray(16)

    /**
     * Asama sayaclari. Akis durdugunda hangi halkanin koptugunu PC'den
     * gorebilmek icin: kamera kare veriyor mu, GL ciziyor mu, kodlayici
     * cikti uretiyor mu.
     */
    val cameraFrames = java.util.concurrent.atomic.AtomicLong(0)
    val glDraws = java.util.concurrent.atomic.AtomicLong(0)

    init {
        val ready = CountDownLatch(1)
        var failure: Throwable? = null
        handler.post {
            try {
                initGl()
            } catch (t: Throwable) {
                failure = t
            } finally {
                ready.countDown()
            }
        }
        // Zaman asimsiz beklemek cagiran is parcacigini kilitler. Servis bunu
        // ana is parcacigindan cagirabildigi icin bu dogrudan ANR demek.
        if (!ready.await(INIT_TIMEOUT_MS, TimeUnit.MILLISECONDS)) {
            thread.quitSafely()
            throw IllegalStateException("GL kurulumu zaman asimina ugradi")
        }
        failure?.let { throw IllegalStateException("GL kurulamadi: ${it.message}", it) }
    }

    private fun initGl() {
        val core = EglCore()
        eglCore = core
        encoderEglSurface = core.createWindowSurface(encoderSurface).also { core.makeCurrent(it) }
        renderer.setup()

        val texture = SurfaceTexture(renderer.textureId)
        texture.setDefaultBufferSize(cameraSize.width, cameraSize.height)
        // Dinleyiciye kendi handler'imizi veriyoruz: geri cagri dogrudan GL
        // is parcaciginda calissin, ek bir post gecikmesi olmasin.
        texture.setOnFrameAvailableListener({ drawFrame() }, handler)
        surfaceTexture = texture
        cameraSurface = Surface(texture)
    }

    /**
     * Ekrandaki onizleme yuzeyini bagla; `surface` null ise onizlemeyi kapat.
     *
     * Ekran donunce `surfaceDestroyed` **cagrilmiyor** (Activity `configChanges`
     * ile yonu kendisi karsiliyor); dogrudan yeni olculerle `surfaceChanged`
     * geliyor, yani ayni pencereden ikinci bir EGL yuzeyi acmak gerekiyor.
     * Sirasi bu yuzden kritik.
     */
    fun setPreviewSurface(surface: Surface?, width: Int, height: Int) {
        // Bayragi cagiran is parcaciginda hemen indir: GL is parcacigina post
        // edilen is siraya giriyor, bu arada cizilen kare olu yuzeye dokunmasin.
        previewUsable = false

        handler.post {
            val core = eglCore ?: return@post

            // Yok etmeden once baglami kodlayici yuzeyine al. EGL, o an current
            // olan bir yuzeyi yok etmeyi erteliyor ve ANativeWindow bagli
            // kaliyor; ayni pencereden yeni yuzey acmak EGL_BAD_ALLOC veriyor.
            // Ekran donunce tam bu oluyordu: yeni yuzey acilamiyor, onizleme son
            // karede donup kaliyordu ve bir daha da acilmiyordu.
            encoderEglSurface?.let { runCatching { core.makeCurrent(it) } }
            previewEglSurface?.let {
                runCatching { core.releaseSurface(it) }
                previewEglSurface = null
            }

            if (surface == null || !surface.isValid || width <= 0 || height <= 0) return@post

            val created = runCatching {
                core.createWindowSurface(surface).also {
                    core.makeCurrent(it)
                    core.setSwapInterval(0)
                }
            }.onFailure { Log.w(TAG, "onizleme yuzeyi acilamadi: ${it.message}") }
                .getOrNull()

            // Baglami her halukarda kodlayici yuzeyine geri al.
            encoderEglSurface?.let { runCatching { core.makeCurrent(it) } }

            if (created != null) {
                previewEglSurface = created
                previewSize = Size(width, height)
                previewUsable = true
            }
        }
    }

    /**
     * Onizleme cizilebilir durumda mi. Basarisiz kalirsa arayuz yeniden
     * baglamayi deneyebilsin diye disari aciliyor - sessizce olu kalmasin.
     */
    val previewActive: Boolean get() = previewUsable

    @Volatile
    private var previewUsable = false

    fun release() {
        val done = CountDownLatch(1)
        handler.post {
            surfaceTexture?.setOnFrameAvailableListener(null)
            runCatching { cameraSurface.release() }
            runCatching { surfaceTexture?.release() }
            surfaceTexture = null

            val core = eglCore
            if (core != null) {
                previewEglSurface?.let { core.releaseSurface(it) }
                encoderEglSurface?.let { core.makeCurrent(it) }
                renderer.release()
                encoderEglSurface?.let { core.releaseSurface(it) }
                core.release()
            }
            previewEglSurface = null
            encoderEglSurface = null
            eglCore = null
            done.countDown()
        }
        // GL is parcacigi `swapBuffers`'da takilmis olabilir - bu gercekten
        // oluyor. Suresiz beklersek kapatma cagrisi ana is parcacigini
        // kilitler. Beklemeyi sinirlayip her halukarda birakiyoruz.
        if (!done.await(RELEASE_TIMEOUT_MS, TimeUnit.MILLISECONDS)) {
            Log.w(TAG, "GL kapanisi zaman asimina ugradi, zorla birakiliyor")
        }
        thread.quitSafely()
    }

    // ------------------------------------------------------------------ cizim

    private fun drawFrame() {
        val core = eglCore ?: return
        val texture = surfaceTexture ?: return
        val encoderTarget = encoderEglSurface ?: return

        cameraFrames.incrementAndGet()

        try {
            core.makeCurrent(encoderTarget)
            texture.updateTexImage()
            texture.getTransformMatrix(texMatrix)
        } catch (e: Exception) {
            Log.w(TAG, "kare alinamadi: ${e.message}")
            return
        }

        val angle = rotation

        // 1) Yayina giden kare
        GLES20.glViewport(0, 0, outputSize.width, outputSize.height)
        buildMatrix(outputSize.width, outputSize.height, angle, fitOutputFrame = false)
        renderer.draw(mvpMatrix, texMatrix)
        core.setPresentationTime(encoderTarget, texture.timestamp)
        core.swapBuffers(encoderTarget)
        glDraws.incrementAndGet()

        // 2) Onizleme - ayni doku, ayni donus, sadece hedef yuzey farkli
        val preview = previewEglSurface?.takeIf { previewUsable } ?: return
        try {
            core.makeCurrent(preview)
            GLES20.glViewport(0, 0, previewSize.width, previewSize.height)
            buildMatrix(previewSize.width, previewSize.height, angle, fitOutputFrame = true)
            renderer.draw(mvpMatrix, texMatrix)
            core.swapBuffers(preview)
        } catch (e: Exception) {
            // Onizlemenin yayini durdurma yetkisi yok: yuzeyi birakip devam.
            Log.w(TAG, "onizleme cizilemedi: ${e.message}")
            previewUsable = false
            runCatching { core.makeCurrent(encoderTarget) }
        }
    }

    /**
     * Donus + en-boy orani koruyan olcekleme.
     *
     * Kodlayici karesi akis boyunca sabit kalmali (ortasinda cozunurluk
     * degistirmek ffmpeg'i ve OBS'i kirar), bu yuzden `outputSize` disaridan
     * veriliyor. Icerigin o kareye nasil oturacagini [crop] belirliyor:
     *
     *  - `crop = true`  (FrameMode.FILL) icerik kareyi **doldurur**, tasan yer
     *    kirpilir. Siyah bant olusmaz.
     *  - `crop = false` (FrameMode.FIT) icerik kareye **sigdirilir**. Bant
     *    olusmamasi icin `CameraEncoder` kareyi donuse gore cevirmis olmali.
     *
     * `fitOutputFrame` onizleme icin: once tum yayin karesini onizleme alanina
     * oturtur, sonra icerigi o kareye yerlestirir. Boylece ekranda gorulen sey
     * birebir gonderilen kare olur - telefon ekraninda bant gorunmesi normal,
     * yayina giden kare o degil.
     */
    private fun buildMatrix(dstWidth: Int, dstHeight: Int, angle: Int, fitOutputFrame: Boolean) {
        val cameraAspect = cameraSize.width.toFloat() / cameraSize.height
        val contentAspect = if (angle % 180 == 90) 1f / cameraAspect else cameraAspect

        val frameAspect = outputSize.width.toFloat() / outputSize.height
        val dstAspect = dstWidth.toFloat() / dstHeight

        Matrix.setIdentityM(mvpMatrix, 0)

        if (fitOutputFrame) {
            // Onizleme yuzeyi: tum kareyi ekrana sigdir (burada bant serbest).
            val (fx, fy) = fitScale(frameAspect, dstAspect)
            Matrix.scaleM(mvpMatrix, 0, fx, fy, 1f)
        }

        val target = if (fitOutputFrame) frameAspect else dstAspect
        val (sx, sy) = if (crop) coverScale(contentAspect, target)
                       else fitScale(contentAspect, target)
        Matrix.scaleM(mvpMatrix, 0, sx, sy, 1f)

        // GL'de pozitif aci saat yonunun tersi; istenen donus saat yonunde.
        Matrix.rotateM(mvpMatrix, 0, -angle.toFloat(), 0f, 0f, 1f)
    }

    /** `content` en-boy oranini `target` icine **sigdiran** olcek carpanlari. */
    private fun fitScale(content: Float, target: Float): Pair<Float, Float> {
        val ratio = content / target
        return if (ratio <= 1f) ratio to 1f else 1f to (1f / ratio)
    }

    /**
     * `content` en-boy oranini `target`i **kaplayacak** sekilde buyuten olcek
     * carpanlari. Dortgen goruntu alanini tasar, tasan kisim kirpilir.
     */
    private fun coverScale(content: Float, target: Float): Pair<Float, Float> {
        val ratio = content / target
        return if (ratio >= 1f) ratio to 1f else 1f to (1f / ratio)
    }

    private companion object {
        const val TAG = "OwnCam/GL"
        const val INIT_TIMEOUT_MS = 3_000L
        const val RELEASE_TIMEOUT_MS = 1_500L
    }
}
