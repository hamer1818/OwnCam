package com.owncam

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.hardware.camera2.CameraCharacteristics
import android.net.wifi.WifiManager
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.PowerManager
import android.os.SystemClock
import android.util.Log
import android.util.Size
import android.view.OrientationEventListener
import android.view.Surface
import java.net.Inet4Address
import java.net.NetworkInterface
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

/**
 * Akisi ayakta tutan on plan servisi.
 *
 * Ekran kapaninca akis kesilmesin diye foreground service + wake lock
 * (plan bolum 5.1 son madde).
 */
class StreamService : Service(), CameraEncoder.Listener {

    private var encoder: CameraEncoder? = null
    private var server: TcpVideoServer? = null
    private var advertiser: MdnsAdvertiser? = null
    private var wakeLock: PowerManager.WakeLock? = null
    private var wifiLock: WifiManager.WifiLock? = null
    private var statusServer: StatusServer? = null

    /** Durum ucu (kendi is parcacigi) okuyup yaziyor, yasam dongusu de. */
    @Volatile
    private var config = StreamConfig()

    /**
     * Servis geri cagrilari **ana is parcaciginda** calisir. Kamera acma,
     * `MediaCodec.configure/start/stop` ve GL kurulum/kapanisi yuzlerce
     * milisaniye surebiliyor, kilitlendiginde ise sonsuza kadar bekliyor -
     * ikisi de "uygulama yanit vermiyor" demek. Bu isler bu is parcaciginda
     * yapiliyor, ana is parcaciginda hicbiri yok.
     */
    private val lifecycle: ExecutorService =
        Executors.newSingleThreadExecutor { r -> Thread(r, "owncam-lifecycle") }

    override fun onCreate() {
        super.onCreate()
        instance = this
        // Akistan bagimsiz: yayin durdurulsa bile masaustu uygulamasi
        // yeniden baslatabilsin diye servis yasadigi surece ayakta.
        statusServer = StatusServer(
            StatusServer.DEFAULT_PORT,
            statusJson = { buildStatusJson() },
            onCommand = { path, params -> handleCommand(path, params) }
        ).also { it.start() }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> {
                lifecycle.execute {
                    stopStreaming()
                    stopSelf()
                }
                return START_NOT_STICKY
            }
        }

        @Suppress("DEPRECATION")
        // Intent yoksa (START_STICKY ile yeniden dogus) kayitli ayarlar.
        config = (intent?.getSerializableExtra(EXTRA_CONFIG) as? StreamConfig)
            ?: StreamConfig.load(this)

        // startForeground'in hemen cagrilmasi gerekiyor; agir is arkada.
        startForeground(NOTIFICATION_ID, buildNotification("Baslatiliyor..."))
        lifecycle.execute { startStreaming() }
        return START_STICKY
    }

    override fun onDestroy() {
        instance = null
        statusServer?.stop()
        statusServer = null
        // Beklemiyoruz: kapanis kilitlenirse ana is parcacigini tutmasin.
        lifecycle.execute { stopStreaming() }
        lifecycle.shutdown()
        super.onDestroy()
    }

    // ------------------------------------------------------------------- akis

    private fun startStreaming() {
        if (encoder != null) return

        acquireLocks()

        val tcp = TcpVideoServer(config.port) { encoder?.requestKeyFrame() }
        val cam = CameraEncoder(this, config, this)
        cam.presetOrientation(config.imageRotation, deviceOrientation)
        server = tcp
        encoder = cam

        try {
            tcp.start()
        } catch (e: Exception) {
            onError("Port ${config.port} acilamadi: ${e.message}")
            return
        }
        cam.start()
        registerControlReceiver()
        startOrientationTracking()
        startBitrateControl()

        advertiser = MdnsAdvertiser(this).also { it.register(config.port) }

        statsProvider = {
            val connected = if (tcp.isClientConnected) "PC: ${tcp.clientAddress}" else "PC bekleniyor"
            "$connected\ngonderilen: ${tcp.framesSent.get()} kare, " +
                "dusen: ${tcp.framesDropped.get()}, " +
                "${tcp.bytesSent.get() / 1_000_000} MB"
        }

        state = State(
            running = true,
            address = "${localIpAddress() ?: "?"}:${config.port}",
            detail = config.label
        )
        updateNotification()
    }

    private fun stopStreaming() {
        stopOrientationTracking()
        stopBitrateControl()
        statsProvider = null
        runCatching { unregisterReceiver(controlReceiver) }
        advertiser?.unregister()
        advertiser = null
        encoder?.stop()
        encoder = null
        server?.stop()
        server = null
        releaseLocks()
        state = State(running = false, address = null, detail = "Durduruldu")
    }

    /**
     * adb uzerinden kontrol - ag hic devrede olmadan, sadece USB kablosuyla.
     *
     *   adb shell am broadcast -a com.owncam.CONTROL --ei rotate 90
     *   adb shell am broadcast -a com.owncam.CONTROL
     *   adb logcat -d -s OwnCam/Control:I
     *
     * Her iki durumda da guncel durum JSON'u gunluge yazilir; yaniti okumak
     * icin ayri bir kanal gerekmiyor.
     */
    private val controlReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) {
            if (intent?.hasExtra(EXTRA_ROTATE) == true) {
                setRotation(intent.getIntExtra(EXTRA_ROTATE, 0))
            }
            Log.i(TAG_CONTROL, buildStatusJson().replace(Regex("\\s+"), " "))
        }
    }

    private fun registerControlReceiver() {
        val filter = IntentFilter(ACTION_CONTROL)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            registerReceiver(controlReceiver, filter, Context.RECEIVER_EXPORTED)
        } else {
            @Suppress("UnspecifiedRegisterReceiverFlag")
            registerReceiver(controlReceiver, filter)
        }
    }

    /**
     * Masaustu uygulamasindan gelen komutlar.
     *
     *   /rotate?deg=90                   donusu ayarla ve kaydet
     *   /config?rotation=90&front=1&...  ayar degistir
     *   /stop  /start
     *
     * Cozunurluk, kare hizi ve kamera yonu kodlayiciyi yeniden kurmayi
     * gerektiriyor; hepsini tek bir yeniden baslatmada topluyoruz.
     */
    private fun handleCommand(path: String, params: Map<String, String>) {
        when (path) {
            // Elle donus istegi otomatigi kapatir: aksi halde bir sonraki
            // yon okumasi kullanicinin secimini hemen geri alirdi.
            "/rotate" -> params["deg"]?.toIntOrNull()?.let {
                if (config.autoRotate) {
                    config = config.copy(autoRotate = false)
                    StreamConfig.save(this, config)
                    stopOrientationTracking()
                }
                setRotation(it)
            }

            "/stop" -> lifecycle.execute { stopStreaming() }

            "/start" -> lifecycle.execute { if (encoder == null) startStreaming() }

            "/config" -> {
                val updated = config.copy(
                    width = params["width"]?.toIntOrNull() ?: config.width,
                    height = params["height"]?.toIntOrNull() ?: config.height,
                    frameRate = params["fps"]?.toIntOrNull() ?: config.frameRate,
                    lensFacing = params["front"]?.let {
                        if (it == "1") CameraCharacteristics.LENS_FACING_FRONT
                        else CameraCharacteristics.LENS_FACING_BACK
                    } ?: config.lensFacing,
                    imageRotation = params["rotation"]?.toIntOrNull() ?: config.imageRotation,
                    preview = params["preview"]?.let { it == "1" } ?: config.preview,
                    autoRotate = params["auto"]?.let { it == "1" } ?: config.autoRotate,
                    frameMode = params["mode"]?.let { FrameMode.from(it) } ?: config.frameMode,
                    mirror = params["mirror"]?.let { it == "1" } ?: config.mirror,
                    lockExposure = params["exposure"]?.let { it == "1" } ?: config.lockExposure,
                    lockFocus = params["focus"]?.let { it == "1" } ?: config.lockFocus,
                    adaptiveBitrate = params["adaptive"]?.let { it == "1" }
                        ?: config.adaptiveBitrate
                ).let {
                    if (params["width"] != null || params["height"] != null) {
                        it.copy(bitRate = StreamConfig.defaultBitRate(it.width, it.height))
                    } else it
                }

                when {
                    updated == config -> Unit
                    // Yalnizca donus degistiyse akisi kesmeden gecebiliriz.
                    updated == config.copy(imageRotation = updated.imageRotation) ->
                        setRotation(updated.imageRotation)
                    else -> {
                        StreamConfig.save(this, updated)
                        lifecycle.execute { restartWith(updated) }
                    }
                }
            }
        }
    }

    /**
     * Donusu ayarla, kaydet ve mumkunse akisi kesmeden uygula.
     *
     * 0<->180 ve 90<->270 gecislerinde kare boyutu ayni kaldigi icin
     * kodlayiciya dokunmaya gerek yok - `owncam-calibrate.sh` bu sayede
     * dort acinin arasinda hizlica gezebiliyor. Kare boyutu degisiyorsa
     * (0/180 <-> 90/270) akis yeniden kuruluyor.
     */
    private fun setRotation(degrees: Int, device: Int = degrees) {
        val deg = ((degrees % 360) + 360) % 360
        val dev = ((device % 360) + 360) % 360
        if (deg == config.imageRotation && dev == deviceOrientation) return
        deviceOrientation = dev
        val updated = config.copy(imageRotation = deg)
        StreamConfig.save(this, updated)
        if (encoder?.applyOrientation(deg, dev) == true) {
            config = updated
            updateNotification()
            return
        }
        lifecycle.execute { restartWith(updated) }
    }

    private fun restartWith(updated: StreamConfig) {
        val wasRunning = encoder != null
        if (wasRunning) stopStreaming()
        config = updated
        if (wasRunning) startStreaming()
    }

    // -------------------------------------------------- uyarlanabilir bit hizi

    private val bitrateHandler = Handler(Looper.getMainLooper())
    private var lastDropped = 0L
    private var calmTicks = 0
    private var bitrateTick: Runnable? = null

    /**
     * Ag yetismedigi zaman kare atmak yerine kaliteyi dusur.
     *
     * Sinyal olarak gonderim kuyrugundan **dusen kare** sayaci kullaniliyor.
     * Kuyruk iki kare tutuyor ve dolunca en eskisini atiyor; yani sayac
     * artiyorsa kodlayici agin tasiyabildiginden fazlasini uretiyor demektir.
     * Baska bir olcume (RTT, pencere boyu) gerek yok - tikanmanin tanimi bu.
     *
     * Inis hizli, cikis yavas: tikanmaya hemen tepki verilmeli ama toparlanma
     * denemesi yeni bir tikanma yaratmamali. Yukari cikis ancak birkac sakin
     * turdan sonra ve kucuk adimlarla.
     */
    private fun startBitrateControl() {
        if (!config.adaptiveBitrate) return
        lastDropped = server?.framesDropped?.get() ?: 0
        calmTicks = 0
        val tick = object : Runnable {
            override fun run() {
                stepBitrate()
                bitrateHandler.postDelayed(this, BITRATE_TICK_MS)
            }
        }
        bitrateTick = tick
        bitrateHandler.postDelayed(tick, BITRATE_TICK_MS)
    }

    private fun stopBitrateControl() {
        bitrateTick?.let { bitrateHandler.removeCallbacks(it) }
        bitrateTick = null
    }

    private fun stepBitrate() {
        val cam = encoder ?: return
        val dropped = server?.framesDropped?.get() ?: return
        val newDrops = dropped - lastDropped
        lastDropped = dropped

        val current = cam.appliedBitrate
        if (newDrops > 0) {
            calmTicks = 0
            val target = (current * BITRATE_DOWN).toInt()
                .coerceAtLeast(CameraEncoder.MIN_BITRATE)
            if (target < current) {
                Log.i(TAG, "ag tikandi ($newDrops kare), bit hizi $current -> $target")
                cam.setBitrate(target)
            }
            return
        }

        calmTicks++
        if (calmTicks < BITRATE_CALM_TICKS || current >= config.bitRate) return
        calmTicks = 0
        val target = (current * BITRATE_UP).toInt().coerceAtMost(config.bitRate)
        Log.i(TAG, "ag sakin, bit hizi $current -> $target")
        cam.setBitrate(target)
    }

    // ------------------------------------------------------- otomatik donus

    private var orientationListener: OrientationEventListener? = null
    private var pendingOrientation = -1
    private var pendingSince = 0L

    /**
     * Telefonun son bilinen fiziksel yonu. Kare **sekli** bundan geliyor:
     * dik tutunca dikey kare, yan tutunca yatay kare.
     *
     * Duz yatarken yon okunamadigi icin son deger korunuyor - webcam zaten oyle
     * duruyor, o yuzden "bayatlamasi" istenen davranis.
     */
    @Volatile
    private var deviceOrientation = 0

    /**
     * Donusu telefonun fiziksel yonunden izle.
     *
     * Olculen kural: **imageRotation = cihaz yonu**. `OrientationEventListener`
     * cihaz donusunu saat yonunde veriyor ve olcum tam bunu istiyor - cihaz 0
     * (dik) iken 0, cihaz 270 (sola yatik) iken 270. `SENSOR_ORIENTATION`
     * hesaba hic girmiyor; bu cihazda o deger yaniltici.
     *
     * Serviste yasiyor, Activity'de degil: telefon uygulama ekrani kapaliyken
     * dondugunde de goruntu duzelsin.
     */
    private fun startOrientationTracking() {
        if (!config.autoRotate || orientationListener != null) return
        val listener = object : OrientationEventListener(this) {
            override fun onOrientationChanged(orientation: Int) {
                // Telefon duz yatiyorsa yon okunamiyor: son bilinen deger kalsin.
                if (orientation == ORIENTATION_UNKNOWN) return
                onDeviceOrientation(((orientation + 45) / 90 * 90) % 360)
            }
        }
        if (!listener.canDetectOrientation()) {
            Log.w(TAG, "cihaz yonu okunamiyor, otomatik donus kapali")
            return
        }
        listener.enable()
        orientationListener = listener
    }

    private fun stopOrientationTracking() {
        orientationListener?.disable()
        orientationListener = null
        pendingOrientation = -1
    }

    /**
     * Yeni yon uygulanmadan once bir sure sabit kalmali. Telefonu elde
     * cevirirken ara acilardan geciliyor; her birine tepki vermek kare boyutunu
     * ileri geri degistirip akisi tekrar tekrar yeniden kurardi.
     */
    private fun onDeviceOrientation(snapped: Int) {
        if (snapped == config.imageRotation && snapped == deviceOrientation) {
            pendingOrientation = -1
            return
        }
        val now = SystemClock.elapsedRealtime()
        if (snapped != pendingOrientation) {
            pendingOrientation = snapped
            pendingSince = now
            return
        }
        if (now - pendingSince < ORIENTATION_SETTLE_MS) return
        pendingOrientation = -1
        Log.i(TAG, "otomatik yon: cihaz $snapped -> donus $snapped")
        setRotation(snapped, snapped)
    }

    /**
     * PC'nin okuyacagi durum. El yazimi JSON - tek bir duz nesne icin
     * kutuphane getirmeye degmez.
     */
    private fun buildStatusJson(): String {
        val cam = encoder
        val tcp = server
        fun q(value: String?) = if (value == null) "null" else "\"$value\""
        fun size(value: Size?) = q(value?.let { "${it.width}x${it.height}" })

        return """
        {
          "streaming": ${cam != null},
          "resolution": ${size(cam?.captureSize)},
          "frame": ${size(cam?.frameSize)},
          "fps": ${config.frameRate},
          "bitrate": ${config.bitRate},
          "camera": ${q(if (config.lensFacing == CameraCharacteristics.LENS_FACING_FRONT) "front" else "back")},
          "sensorOrientation": ${cam?.sensorAngle ?: 0},
          "imageRotation": ${config.imageRotation},
          "preview": ${config.preview},
          "mirror": ${config.mirror},
          "autoRotate": ${config.autoRotate},
          "deviceOrientation": $deviceOrientation,
          "frameMode": ${q(config.frameMode.key)},
          "appliedRotation": ${cam?.appliedAngle ?: config.imageRotation},
          "narrow": ${cam?.pillarboxed ?: false},
          "exposureLocked": ${config.lockExposure},
          "focusLocked": ${config.lockFocus},
          "adaptiveBitrate": ${config.adaptiveBitrate},
          "bitrate": ${config.bitRate},
          "bitrateNow": ${encoder?.appliedBitrate ?: config.bitRate},
          "cameraFrames": ${cam?.cameraFrames ?: 0},
          "glDraws": ${cam?.glDraws ?: 0},
          "encoderOutputs": ${cam?.encoderOutputs?.get() ?: 0},
          "framesSent": ${tcp?.framesSent?.get() ?: 0},
          "framesDropped": ${tcp?.framesDropped?.get() ?: 0},
          "framesSkipped": ${tcp?.framesSkipped?.get() ?: 0},
          "bytesSent": ${tcp?.bytesSent?.get() ?: 0},
          "client": ${q(tcp?.clientAddress)}
        }
        """.trimIndent()
    }

    private fun acquireLocks() {
        val power = getSystemService(Context.POWER_SERVICE) as PowerManager
        wakeLock = power.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "OwnCam::stream").apply {
            setReferenceCounted(false)
            acquire(WAKE_LOCK_TIMEOUT_MS)
        }
        // WiFi guc tasarrufu kare araliklarinda 100 ms'i asan bosluklar yaratir.
        val wifi = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
        wifiLock = wifi.createWifiLock(
            WifiManager.WIFI_MODE_FULL_HIGH_PERF, "OwnCam::wifi"
        ).apply {
            setReferenceCounted(false)
            acquire()
        }
    }

    private fun releaseLocks() {
        runCatching { wakeLock?.takeIf { it.isHeld }?.release() }
        runCatching { wifiLock?.takeIf { it.isHeld }?.release() }
        wakeLock = null
        wifiLock = null
    }

    // --------------------------------------------------- CameraEncoder.Listener

    override fun onEncodedFrame(data: ByteArray, isKeyFrame: Boolean, isCodecConfig: Boolean) {
        val tcp = server ?: return
        if (isCodecConfig) tcp.setCodecConfig(data) else tcp.submitFrame(data, isKeyFrame)
    }

    override fun onStarted(frameSize: Size) {
        state = state.copy(
            detail = "${frameSize.width}x${frameSize.height} @ ${config.frameRate}fps"
        )
        updateNotification()
    }

    override fun onError(message: String) {
        Log.e(TAG, message)
        state = state.copy(running = false, detail = "Hata: $message")
        updateNotification()
    }

    // ---------------------------------------------------------------- bildirim

    private fun updateNotification() {
        val text = buildString {
            append(state.address ?: "-")
            append("  ")
            append(state.detail)
            server?.let { if (it.isClientConnected) append("  [PC bagli]") }
        }
        val manager = getSystemService(NotificationManager::class.java)
        manager.notify(NOTIFICATION_ID, buildNotification(text))
    }

    private fun buildNotification(text: String): Notification {
        val manager = getSystemService(NotificationManager::class.java)
        if (manager.getNotificationChannel(CHANNEL_ID) == null) {
            manager.createNotificationChannel(
                NotificationChannel(
                    CHANNEL_ID, "OwnCam akis",
                    NotificationManager.IMPORTANCE_LOW
                ).apply { setShowBadge(false) }
            )
        }

        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )
        val stop = PendingIntent.getService(
            this, 1, Intent(this, StreamService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
        )

        return Notification.Builder(this, CHANNEL_ID)
            .setContentTitle("OwnCam yayinda")
            .setContentText(text)
            .setSmallIcon(android.R.drawable.ic_menu_camera)
            .setContentIntent(open)
            .setOngoing(true)
            .addAction(
                Notification.Action.Builder(null as android.graphics.drawable.Icon?, "Durdur", stop)
                    .build()
            )
            .build()
    }

    data class State(
        val running: Boolean = false,
        val address: String? = null,
        val detail: String = "Hazir"
    )

    companion object {
        private const val TAG = "OwnCam/Service"
        private const val CHANNEL_ID = "owncam_stream"
        private const val NOTIFICATION_ID = 1
        private const val WAKE_LOCK_TIMEOUT_MS = 12L * 60 * 60 * 1000
        /** Yeni yon uygulanmadan once sabit kalmasi gereken sure. */
        private const val ORIENTATION_SETTLE_MS = 900L

        /** Uyarlanabilir bit hizinin denetim araligi. */
        private const val BITRATE_TICK_MS = 2_000L
        /** Tikanmada carpan: hizli in. */
        private const val BITRATE_DOWN = 0.75
        /** Sakin donemde carpan: yavas cik. */
        private const val BITRATE_UP = 1.15
        /** Yukari cikmadan once kac sakin tur beklenecegi (2 sn'lik turlar). */
        private const val BITRATE_CALM_TICKS = 5
        const val ACTION_STOP = "com.owncam.STOP"
        const val EXTRA_CONFIG = "config"

        /** adb ile kontrol: `am broadcast -a com.owncam.CONTROL --ei rotate 90` */
        const val ACTION_CONTROL = "com.owncam.CONTROL"
        const val EXTRA_ROTATE = "rotate"
        private const val TAG_CONTROL = "OwnCam/Control"

        @Volatile
        private var instance: StreamService? = null

        /**
         * Arayuzdeki onizleme yuzeyini akisa bagla. Servis calismiyorsa
         * sessizce yok sayilir - kullanici henuz baslatmamis demektir.
         */
        fun attachPreview(surface: Surface?, width: Int, height: Int) {
            instance?.encoder?.setPreviewSurface(surface, width, height)
        }

        /** Onizleme gercekten ciziliyor mu (arayuzun yeniden baglama karari icin). */
        fun previewActive(): Boolean = instance?.encoder?.previewActive ?: false

        /** Teshis: uygulanan donus, kadraj modu, yakalama ve kare boyutu. */
        fun rotationInfo(): String? = instance?.encoder?.rotationInfo()

        @Volatile
        var state: State = State()
            private set

        /** Arayuzun canli sayaclari okumasi icin. Servis durunca null olur. */
        @Volatile
        var statsProvider: (() -> String)? = null

        /** WiFi arayuzunun IPv4 adresi; kullaniciya gosterilir. */
        fun localIpAddress(): String? = runCatching {
            NetworkInterface.getNetworkInterfaces().toList()
                .filter { it.isUp && !it.isLoopback }
                .sortedBy { if (it.name.startsWith("wlan")) 0 else 1 }
                .flatMap { it.inetAddresses.toList() }
                .filterIsInstance<Inet4Address>()
                .firstOrNull { !it.isLoopbackAddress }
                ?.hostAddress
        }.getOrNull()

        fun start(context: Context, config: StreamConfig) {
            val intent = Intent(context, StreamService::class.java)
                .putExtra(EXTRA_CONFIG, config)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                context.startForegroundService(intent)
            } else {
                context.startService(intent)
            }
        }

        fun stop(context: Context) {
            context.startService(
                Intent(context, StreamService::class.java).setAction(ACTION_STOP)
            )
        }
    }
}
