package com.owncam

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.hardware.camera2.CameraAccessException
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CameraMetadata
import android.hardware.camera2.CaptureRequest
import android.hardware.camera2.params.OutputConfiguration
import android.hardware.camera2.params.SessionConfiguration
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.media.MediaFormat
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import android.util.Range
import android.util.Size
import android.view.Surface
import com.owncam.gl.FrameRenderer
import java.nio.ByteBuffer
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Camera2 -> (Surface) -> MediaCodec H.264 donanim kodlayici.
 *
 * Kritik tasarim karari (plan bolum 3): goruntu kameradan kodlayiciya
 * **Surface uzerinden** gidiyor. Byte dizisi olarak CPU'ya hic ugramiyor.
 */
class CameraEncoder(
    private val context: Context,
    private val config: StreamConfig,
    private val listener: Listener
) {

    interface Listener {
        /** Kodlanmis Annex-B veri. `codecConfig` ise SPS/PPS demektir. */
        fun onEncodedFrame(data: ByteArray, isKeyFrame: Boolean, isCodecConfig: Boolean)
        fun onError(message: String)
        fun onStarted(frameSize: Size)
    }

    private val running = AtomicBoolean(false)

    private var cameraThread: HandlerThread? = null
    private var cameraHandler: Handler? = null
    private var drainThread: Thread? = null

    private var encoder: MediaCodec? = null
    private var inputSurface: Surface? = null
    private var renderer: FrameRenderer? = null
    private var camera: CameraDevice? = null
    private var session: CameraCaptureSession? = null
    private var requestBuilder: CaptureRequest.Builder? = null

    /**
     * Kameradan gelen kare. Her zaman sensorun dogal (yatay) yonunde -
     * Camera2 goruntuyu dondurmez, `SCALER_STREAM_CONFIGURATION_MAP` yalnizca
     * yatay boyutlar sunar.
     */
    @Volatile var captureSize: Size = Size(config.width, config.height)
        private set

    /**
     * Kodlayiciya giden kare. Donus 90/270 iken [captureSize]'in devrigi:
     * kamera dik bir alan gorduguse kare de dik uretilir, boylece goruntu
     * yatay bir karenin ortasindaki dar seride sikismaz.
     */
    @Volatile var frameSize: Size = Size(config.width, config.height)
        private set

    /** Sensorun dogal yone gore acisi; yalnizca teshis icin gosteriliyor. */
    private var sensorOrientation = 0


    fun start() {
        if (!running.compareAndSet(false, true)) return

        if (context.checkSelfPermission(Manifest.permission.CAMERA)
            != PackageManager.PERMISSION_GRANTED
        ) {
            fail("Kamera izni yok")
            return
        }

        val manager = context.getSystemService(Context.CAMERA_SERVICE) as CameraManager
        val cameraId = selectCamera(manager) ?: run {
            fail("Uygun kamera bulunamadi")
            return
        }

        val characteristics = manager.getCameraCharacteristics(cameraId)
        sensorOrientation =
            characteristics.get(CameraCharacteristics.SENSOR_ORIENTATION) ?: 0
        captureSize = selectCaptureSize(characteristics, rotation)
        frameSize = frameSizeFor(captureSize, rotation)
        Log.i(TAG, "yakalama ${captureSize.width}x${captureSize.height}" +
            " -> kare ${frameSize.width}x${frameSize.height}" +
            " (donus $rotation, ${config.frameMode.key})")

        try {
            startEncoder(frameSize)
        } catch (e: Exception) {
            fail("Kodlayici baslatilamadi: ${e.message}")
            return
        }

        // Hizli yol: donus gerekmiyorsa ve onizleme istenmiyorsa kamera
        // dogrudan kodlayicinin yuzeyine cizsin. GL katmani sadece donus ve
        // onizleme icin var; ikisi de yoksa araya girmesinin tek etkisi fazladan
        // bir kirilma noktasi olmak. GL oncesi surum bu yolda 1080p30'da
        // sorunsuz calisiyordu, varsayilan yapilandirma oraya geri donuyor.
        // GL yalnizca yapacak isi yoksa atlanabilir. Ayna da GL'de
        // uygulaniyor, dolayisiyla acikken bu kisayol kullanilamaz.
        if (rotation == 0 && !config.preview && !config.mirror) {
            Log.i(TAG, "GL atlandi: kamera dogrudan kodlayiciya")
        } else {
            try {
                val surface = inputSurface ?: throw IllegalStateException("giris yuzeyi yok")
                renderer = FrameRenderer(surface, captureSize, frameSize).also {
                    it.rotation = rotation
                    it.crop = config.frameMode == FrameMode.FILL
                    it.mirror = config.mirror
                }
            } catch (e: Exception) {
                fail("GL katmani kurulamadi: ${e.message}")
                return
            }
        }

        cameraThread = HandlerThread("owncam-camera").apply { start() }
        cameraHandler = Handler(cameraThread!!.looper)

        try {
            manager.openCamera(cameraId, cameraStateCallback(characteristics), cameraHandler)
        } catch (e: CameraAccessException) {
            fail("Kamera acilamadi: ${e.message}")
        } catch (e: SecurityException) {
            fail("Kamera izni reddedildi")
        }
    }

    fun stop() {
        if (!running.compareAndSet(true, false)) return

        runCatching { session?.stopRepeating() }
        runCatching { session?.close() }
        session = null
        runCatching { camera?.close() }
        camera = null

        // Sirasi onemli: GL katmani kodlayicinin yuzeyini tutuyor.
        runCatching { renderer?.release() }
        renderer = null

        drainThread?.interrupt()
        drainThread = null

        runCatching { encoder?.stop() }
        runCatching { encoder?.release() }
        encoder = null
        runCatching { inputSurface?.release() }
        inputSurface = null

        cameraThread?.quitSafely()
        cameraThread = null
        cameraHandler = null
        Log.i(TAG, "kodlayici durdu")
    }

    /** Ekrandaki onizleme yuzeyi; null ise onizleme kapatilir. */
    fun setPreviewSurface(surface: Surface?, width: Int, height: Int) {
        renderer?.setPreviewSurface(surface, width, height)
    }

    /** Onizleme gercekten ciziliyor mu; arayuz gerekirse yeniden baglar. */
    val previewActive: Boolean get() = renderer?.previewActive ?: false

    /**
     * Uygulanan donus. Tek bir bilgi kaynagi var: `config.imageRotation`.
     *
     * Eskiden bunun ustune bir de `manualOffset` biniyordu; iki ayri elle
     * ayarin toplami kimsenin takip edemedigi bir sayi uretiyordu ve durum
     * ciktisindaki uc alan (imageRotation / manualOffset / appliedRotation)
     * birbirini tutmuyordu.
     */
    @Volatile private var rotation = normalize(config.imageRotation)

    /**
     * Telefonun fiziksel yonu (saat yonunde, 90'a yuvarlanmis).
     *
     * `rotation`dan **ayri** tutuluyor. Ikisi cogu telefonda ayni cikiyor ama
     * ayni sey degil: `rotation` goruntuyu duzeltmek icin, `deviceOrientation`
     * karenin sekli icin. Hedef cihazda kadraj ters eksende geldiginden bu ayrim
     * sart - aksi halde dik tutuşta yatay video uretiliyordu.
     */
    @Volatile private var deviceOrientation = normalize(config.imageRotation)

    /**
     * Donusu ve telefonun fiziksel yonunu degistir.
     *
     * Kare boyutu ayni kaliyorsa akisi kesmeden uygulanir. Degisiyorsa `false`
     * doner: kodlayici kare boyutu akisin ortasinda degistirilemez - ffmpeg'i ve
     * OBS'i kirar - cagiran tarafin akisi yeniden kurmasi gerekir.
     */
    fun applyOrientation(degrees: Int, device: Int): Boolean {
        val nextRotation = normalize(degrees)
        val nextDevice = normalize(device)
        // Ceyrek tur degistiginde yalnizca kare boyutu degil, **yakalama boyutu**
        // da yeniden secilmeli (bkz. selectCaptureSize).
        if (nextDevice % 180 != deviceOrientation % 180) return false
        if (nextRotation % 180 != rotation % 180) return false

        val previousDevice = deviceOrientation
        deviceOrientation = nextDevice
        if (frameSizeFor(captureSize, nextRotation) != frameSize) {
            deviceOrientation = previousDevice
            return false
        }
        rotation = nextRotation
        renderer?.rotation = nextRotation
        return true
    }

    /**
     * Akis kurulmadan once telefonun bilinen yonunu ver.
     *
     * Kare boyutu `start()` icinde bir kez seciliyor ve akis boyunca
     * degistirilemiyor; o secim dogru olsun diye yon onceden biliniyor olmali.
     */
    fun presetOrientation(degrees: Int, device: Int) {
        rotation = normalize(degrees)
        deviceOrientation = normalize(device)
    }

    /** Kodlayicidan cikan kare sayisi; akis durursa hangi halkanin koptugunu gosterir. */
    val encoderOutputs = java.util.concurrent.atomic.AtomicLong(0)

    val cameraFrames: Long get() = renderer?.cameraFrames?.get() ?: 0
    val glDraws: Long get() = renderer?.glDraws?.get() ?: 0

    val sensorAngle: Int get() = sensorOrientation
    val appliedAngle: Int get() = rotation

    /**
     * Siyah bant kaliyor mu? Yalnizca FIT modunda ve kare cevrilemediyse
     * (kodlayici dik boyutu reddettiyse) mumkun. FILL'de goruntu kareyi
     * dolduruyor, bant hicbir kosulda olusmuyor.
     */
    val pillarboxed: Boolean
        get() = config.frameMode == FrameMode.FIT &&
            rotation % 180 == 90 &&
            frameSize == captureSize

    private fun normalize(degrees: Int): Int = ((degrees % 360) + 360) % 360

    /**
     * Verilen donus icin kodlayici kare boyutu.
     *
     * FILL modunda kare her zaman kullanicinin sectigi cozunurluk: goruntu
     * kirpilarak kareyi dolduruyor, alici taraf ne olursa olsun ayni yatay
     * kareyi goruyor.
     *
     * FIT modunda hicbir sey kirpilmiyor, o yuzden donus 90/270 iken karenin
     * kendisi ceviriliyor. Cevrilmezse goruntu ortadaki dar seride kuculurdu:
     * 1280x720'de 1280 sutunun yalnizca 405'i dolar, %69 siyah bant kalirdi.
     */
    private fun frameSizeFor(capture: Size, rotation: Int): Size {
        if (config.frameMode == FrameMode.FILL) {
            // Kare telefonun **fiziksel yonunu** takip eder, icerigin seklini
            // degil: telefon dikse dikey kare, yansa yatay kare.
            //
            // Bu ikisi ayni sey degil. Hedef cihazda kadraj ters eksende
            // geliyor (dik tutunca genis goruyor), yani kare sekli icerikten
            // turetilirse tam tersi cikiyor: dik tutuşta yatay video.
            val short = minOf(config.width, config.height)
            val long = maxOf(config.width, config.height)
            return if (deviceOrientation % 180 == 0) Size(short, long) else Size(long, short)
        }
        if (rotation % 180 != 90) return capture

        val caps = encoderCapabilities() ?: return capture
        val target = Size(capture.height, capture.width)
        if (runCatching { caps.isSizeSupported(target.width, target.height) }.getOrDefault(false)) {
            return target
        }

        // Tam devrik kabul edilmiyor. Cogu donanim kodlayicisi genislikte
        // 16'nin katini istiyor ve 1920x1080 cevrilince genislik 1080 oluyor.
        // Hizalamaya yuvarlayip tekrar deniyoruz: birkac piksel kirpmak,
        // karenin %69'unu siyah banda vermekten cok daha iyi.
        val alignedWidth = target.width / caps.widthAlignment.coerceAtLeast(1) *
            caps.widthAlignment.coerceAtLeast(1)
        val alignedHeight = target.height / caps.heightAlignment.coerceAtLeast(1) *
            caps.heightAlignment.coerceAtLeast(1)
        if (alignedWidth > 0 && alignedHeight > 0 &&
            runCatching { caps.isSizeSupported(alignedWidth, alignedHeight) }.getOrDefault(false)
        ) {
            Log.i(TAG, "dik kare hizalandi: ${target.width}x${target.height}" +
                " -> ${alignedWidth}x$alignedHeight")
            return Size(alignedWidth, alignedHeight)
        }

        Log.w(TAG, "kodlayici dik kare kabul etmedi, yatay kare korunuyor" +
            " (kenarlar siyah kalacak)")
        return capture
    }

    /**
     * Kullanilacak H.264 kodlayicisinin yetenekleri.
     *
     * `createEncoderByType` listedeki ilk uygun kodlayiciyi seciyor; ayni
     * sirayla ilkini aliyoruz ki sorulan yetenek gercekten kurulacak
     * kodlayicinin yetenegi olsun. Sormadan `configure` etmek desteklenmeyen
     * boyutta akisi tumden dusuruyordu.
     */
    private fun encoderCapabilities(): MediaCodecInfo.VideoCapabilities? = runCatching {
        MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos.asSequence()
            .filter { info ->
                info.isEncoder && info.supportedTypes.any { it.equals(MIME, ignoreCase = true) }
            }
            .mapNotNull {
                runCatching { it.getCapabilitiesForType(MIME).videoCapabilities }.getOrNull()
            }
            .firstOrNull()
    }.getOrNull()

    /** Arayuzde gosterilecek teshis satiri. */
    fun rotationInfo(): String = buildString {
        append("donus $rotation · ${config.frameMode.key}")
        append(" · ${captureSize.width}x${captureSize.height}")
        append(" -> ${frameSize.width}x${frameSize.height}")
        if (pillarboxed) append(" · DAR (kenarlar siyah)")
    }

    @Volatile private var lastKeyFrameRequestNanos = 0L

    /**
     * Yeni istemci baglandiginda ya da kare dusuruldugunde anahtar kare zorla.
     *
     * **Asla cagiran is parcacigi uzerinde calismaz.** `setParameters`
     * kodlayici tikaliyken bloklayabiliyor; bu cagri kodlayiciyi bosaltan
     * is parcacigindan geldigi icin cikis tamponlari serbest kalmiyor,
     * kodlayici girisi doluyor, GL `swapBuffers`'da bloklaniyor ve kamera
     * tumden duruyordu. Ayri bir is parcacigina atiyoruz.
     *
     * Ayrica saniyede birden fazla istenmiyor: kuyruk dolu kaldiginda her
     * dusen kare icin cagriliyor ve kodlayiciyi gereksiz mesgul ediyordu.
     */
    fun requestKeyFrame() {
        val now = System.nanoTime()
        if (now - lastKeyFrameRequestNanos < KEY_FRAME_MIN_INTERVAL_NANOS) return
        lastKeyFrameRequestNanos = now

        cameraHandler?.post {
            val codec = encoder ?: return@post
            runCatching {
                codec.setParameters(Bundle().apply {
                    putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0)
                })
            }
        }
    }

    // ---------------------------------------------------------------- kodlayici

    private fun startEncoder(size: Size) {
        val format = buildFormat(size, withProfile = true)
        val codec = MediaCodec.createEncoderByType(MIME)

        try {
            codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
        } catch (e: Exception) {
            // Bazi cihazlar acikca verilen profil/seviye kombinasyonunu reddediyor.
            // Bu durumda kodlayicinin kendi varsayilanina birakip devam ediyoruz.
            Log.w(TAG, "profil ile configure basarisiz, profilsiz deneniyor: ${e.message}")
            runCatching { codec.release() }
            val fallback = MediaCodec.createEncoderByType(MIME)
            fallback.configure(
                buildFormat(size, withProfile = false), null, null,
                MediaCodec.CONFIGURE_FLAG_ENCODE
            )
            encoder = fallback
            inputSurface = fallback.createInputSurface()
            fallback.start()
            startDrainThread(fallback)
            return
        }

        encoder = codec
        inputSurface = codec.createInputSurface()
        codec.start()
        startDrainThread(codec)
    }

    private fun buildFormat(size: Size, withProfile: Boolean): MediaFormat =
        MediaFormat.createVideoFormat(MIME, size.width, size.height).apply {
            setInteger(
                MediaFormat.KEY_COLOR_FORMAT,
                MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface
            )
            setInteger(MediaFormat.KEY_BIT_RATE, config.bitRate)
            setInteger(MediaFormat.KEY_FRAME_RATE, config.frameRate)
            setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 1)
            setInteger(
                MediaFormat.KEY_BITRATE_MODE,
                MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_CBR
            )
            // Gercek zamanli oncelik. Android 10'da gecikme icin en onemli ayar:
            // KEY_LOW_LATENCY API 30 ile geldi, bu telefonda yok.
            setInteger(MediaFormat.KEY_PRIORITY, 0)

            if (withProfile) {
                // Baseline'da B-kare yok -> kodlayici kare biriktiremez.
                setInteger(
                    MediaFormat.KEY_PROFILE,
                    MediaCodecInfo.CodecProfileLevel.AVCProfileBaseline
                )
                setInteger(
                    MediaFormat.KEY_LEVEL,
                    if (size.width * size.height > 1280 * 720)
                        MediaCodecInfo.CodecProfileLevel.AVCLevel4
                    else
                        MediaCodecInfo.CodecProfileLevel.AVCLevel31
                )
            }

            // Android 11+ cihazlarda ekstra kazanc; Android 10'da atlanir.
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                setInteger(MediaFormat.KEY_LATENCY, 1)
                setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
            }
        }

    private fun startDrainThread(codec: MediaCodec) {
        drainThread = Thread({ drainLoop(codec) }, "owncam-drain").apply {
            priority = Thread.MAX_PRIORITY
            start()
        }
    }

    private fun drainLoop(codec: MediaCodec) {
        val info = MediaCodec.BufferInfo()
        while (running.get()) {
            val index = try {
                codec.dequeueOutputBuffer(info, 100_000L)
            } catch (e: IllegalStateException) {
                if (running.get()) fail("Kodlayici hatasi: ${e.message}")
                return
            }

            when {
                index == MediaCodec.INFO_TRY_AGAIN_LATER -> continue
                index == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED -> continue
                index < 0 -> continue
            }

            val buffer: ByteBuffer? = runCatching { codec.getOutputBuffer(index) }.getOrNull()
            if (buffer != null && info.size > 0) {
                buffer.position(info.offset)
                buffer.limit(info.offset + info.size)
                val data = ByteArray(info.size)
                buffer.get(data)

                encoderOutputs.incrementAndGet()
                val isConfig = info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0
                val isKey = info.flags and MediaCodec.BUFFER_FLAG_KEY_FRAME != 0
                listener.onEncodedFrame(data, isKey, isConfig)
            }

            runCatching { codec.releaseOutputBuffer(index, false) }

            if (info.flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0) return
        }
    }

    // ------------------------------------------------------------------- kamera

    private fun cameraStateCallback(characteristics: CameraCharacteristics) =
        object : CameraDevice.StateCallback() {
            override fun onOpened(device: CameraDevice) {
                camera = device
                if (!running.get()) {
                    runCatching { device.close() }
                    return
                }
                createSession(device, characteristics)
            }

            override fun onDisconnected(device: CameraDevice) {
                runCatching { device.close() }
                camera = null
                fail("Kamera baglantisi kesildi")
            }

            override fun onError(device: CameraDevice, error: Int) {
                runCatching { device.close() }
                camera = null
                fail("Kamera hatasi: $error")
            }
        }

    private fun createSession(device: CameraDevice, characteristics: CameraCharacteristics) {
        // GL devredeyse kamera ona, degilse dogrudan kodlayiciya cizer.
        val surface = renderer?.cameraSurface
            ?: inputSurface
            ?: return fail("Hedef yuzey yok")

        val builder = device.createCaptureRequest(CameraDevice.TEMPLATE_RECORD).apply {
            addTarget(surface)
            set(CaptureRequest.CONTROL_MODE, CameraMetadata.CONTROL_MODE_AUTO)
            set(
                CaptureRequest.CONTROL_AF_MODE,
                CaptureRequest.CONTROL_AF_MODE_CONTINUOUS_VIDEO
            )
            selectFpsRange(characteristics)?.let {
                set(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE, it)
                Log.i(TAG, "AE fps araligi: $it")
            }
            // Video stabilizasyonu kapali: gecikme ekler, karsiligi yok.
            set(
                CaptureRequest.CONTROL_VIDEO_STABILIZATION_MODE,
                CaptureRequest.CONTROL_VIDEO_STABILIZATION_MODE_OFF
            )
        }
        requestBuilder = builder

        val callback = object : CameraCaptureSession.StateCallback() {
            override fun onConfigured(configured: CameraCaptureSession) {
                if (!running.get()) {
                    runCatching { configured.close() }
                    return
                }
                session = configured
                try {
                    configured.setRepeatingRequest(builder.build(), null, cameraHandler)
                    listener.onStarted(frameSize)
                    if (config.lockExposure) scheduleExposureLock()
                } catch (e: CameraAccessException) {
                    fail("Tekrarlayan istek basarisiz: ${e.message}")
                }
            }

            override fun onConfigureFailed(configured: CameraCaptureSession) {
                fail("Yakalama oturumu kurulamadi")
            }
        }

        val outputs = listOf(OutputConfiguration(surface))
        val sessionConfig = SessionConfiguration(
            SessionConfiguration.SESSION_REGULAR,
            outputs,
            { r -> cameraHandler?.post(r) },
            callback
        )
        try {
            device.createCaptureSession(sessionConfig)
        } catch (e: CameraAccessException) {
            fail("Oturum olusturulamadi: ${e.message}")
        }
    }

    /**
     * Pozlamayi hemen kilitlemek karanlik/patlamis bir kare sabitler.
     * Once AE'nin oturmasi icin kisa bir sure bekliyoruz.
     */
    private fun scheduleExposureLock() {
        cameraHandler?.postDelayed({
            val builder = requestBuilder ?: return@postDelayed
            val active = session ?: return@postDelayed
            builder.set(CaptureRequest.CONTROL_AE_LOCK, true)
            runCatching { active.setRepeatingRequest(builder.build(), null, cameraHandler) }
            Log.i(TAG, "pozlama kilitlendi")
        }, EXPOSURE_SETTLE_MS)
    }

    private fun selectCamera(manager: CameraManager): String? {
        val ids = manager.cameraIdList
        ids.firstOrNull { id ->
            manager.getCameraCharacteristics(id)
                .get(CameraCharacteristics.LENS_FACING) == config.lensFacing
        }?.let { return it }
        return ids.firstOrNull()
    }

    /**
     * Kameradan hangi boyutu isteyecegiz.
     *
     * FIT modunda istenen cozunurlugun kendisi - kirpma yok, secmeye gerek yok.
     *
     * FILL modunda kirpma var, dolayisiyla kaynak ne kadar genisse kayip o kadar
     * az. Ayni sensorde 4:3 mod, 16:9'un dikey kirpilmamis hali: genislik ayni,
     * yukseklik fazla. Yani 4:3 istemek bedava kazanc - kirpilan kenar 720 yerine
     * 1080 pikselden geliyor, buyutme 1.78x yerine ~1.19x kaliyor.
     */
    private fun selectCaptureSize(characteristics: CameraCharacteristics, rotation: Int): Size {
        val requested = Size(config.width, config.height)
        if (config.frameMode != FrameMode.FILL) {
            return selectSize(characteristics, requested.width, requested.height)
        }

        val supported = characteristics
            .get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP)
            ?.getOutputSizes(MediaCodec::class.java)
            ?: return requested

        val budget = requested.width.toLong() * requested.height * MAX_CAPTURE_AREA_FACTOR
        return supported
            .filter { it.width.toLong() * it.height <= budget }
            .filter { minOf(it.width, it.height) >= minOf(requested.width, requested.height) }
            .sortedWith(
                compareByDescending<Size> { minOf(it.width, it.height) }
                    .thenBy { it.width.toLong() * it.height }
            )
            .firstOrNull()
            ?.also {
                if (it != requested) {
                    Log.i(TAG, "dik montaj: yakalama ${it.width}x${it.height} secildi" +
                        " (kirpilacak karenin genisligi icin)")
                }
            }
            ?: selectSize(characteristics, requested.width, requested.height)
    }

    private fun selectSize(characteristics: CameraCharacteristics, w: Int, h: Int): Size {
        val map = characteristics.get(
            CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP
        ) ?: return Size(w, h)

        val supported = map.getOutputSizes(MediaCodec::class.java) ?: return Size(w, h)
        supported.firstOrNull { it.width == w && it.height == h }?.let { return it }

        // Tam eslesme yoksa ayni en-boy oraninda, alan olarak en yakini.
        val target = w.toLong() * h
        return supported.minByOrNull { size ->
            val areaDiff = kotlin.math.abs(size.width.toLong() * size.height - target)
            val ratioDiff = kotlin.math.abs(
                size.width.toDouble() / size.height - w.toDouble() / h
            )
            areaDiff + (ratioDiff * 10_000_000).toLong()
        } ?: Size(w, h)
    }

    /**
     * Jitter icin sabit araliklar ([30,30]) degisken olanlara ([15,30]) tercih
     * edilir: degisken aralikta kamera isik azalinca kare hizini dusurur.
     */
    private fun selectFpsRange(characteristics: CameraCharacteristics): Range<Int>? {
        val ranges = characteristics.get(
            CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES
        ) ?: return null
        val target = config.frameRate
        return ranges.filter { it.upper == target }
            .minByOrNull { target - it.lower }
            ?: ranges.maxByOrNull { it.upper }
    }

    private fun fail(message: String) {
        Log.e(TAG, message)
        listener.onError(message)
    }

    companion object {
        private const val TAG = "OwnCam/Encoder"
        private const val MIME = MediaFormat.MIMETYPE_VIDEO_AVC
        private const val EXPOSURE_SETTLE_MS = 2_000L
        private const val KEY_FRAME_MIN_INTERVAL_NANOS = 1_000_000_000L

        /**
         * Dik montajda daha genis bir kaynak secilebilir ama sinirsiz degil:
         * kamera ve GL her karede bu pikselleri tasiyor. 2x, 720p icin
         * 1440x1080'i (4:3) kapsiyor, 4K'ya kacmiyor.
         */
        private const val MAX_CAPTURE_AREA_FACTOR = 2
    }
}
