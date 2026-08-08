package com.owncam

import android.Manifest
import android.content.pm.ActivityInfo
import android.content.pm.PackageManager
import android.graphics.Typeface
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.Gravity
import android.view.Surface
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.widget.FrameLayout
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity

/**
 * Telefon tarafinda arayuz yok denecek kadar az: **sadece onizleme**.
 *
 * Butun ayarlar masaustu uygulamasindan yapiliyor (`linux/owncam-desktop.py`,
 * telefonun :5300 ucuna baglanir). Telefonu elde tutup ayar degistirmek
 * pratikte ise yaramiyordu - telefon zaten kamera olarak konumlandirilmis
 * oluyor, ekranina her dokunus kadraji bozuyor.
 *
 * Uygulama acilinca yayin **kendiliginden basliyor**; durdurmak icin
 * bildirimdeki "Durdur" ya da masaustunden.
 */
class MainActivity : AppCompatActivity() {

    private lateinit var statusView: TextView
    private lateinit var previewView: SurfaceView

    private var previewSurface: Surface? = null
    private var previewWidth = 0
    private var previewHeight = 0
    private var previewAttached = false

    private val handler = Handler(Looper.getMainLooper())
    private val refresh = object : Runnable {
        override fun run() {
            updateStatus()
            handler.postDelayed(this, 700)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        applyScreenOrientation()
        setContentView(buildUi())
        if (ensurePermissions()) startStreamIfNeeded()
    }

    /**
     * Ekran yonu telefonun kendi yonunu takip eder.
     *
     * Bir donem manifestte `landscape` olarak kilitliydi, sonra da
     * `imageRotation`dan turetiliyordu. Ikisi de yanlisti. Turetme ozellikle
     * yaniltici: hedef cihazda (CLT-L09 on kamera) telefon **dik** tutuldugunda
     * dogru goruntu donusu **0** cikiyor - olculdu - yani "donus 0 ise yatay
     * monte edilmistir" varsayimi tam tersini yapiyordu ve uygulama dik
     * tutulurken inatla yatay aciliyordu.
     *
     * Goruntu donusu ile telefonun fiziksel yonu arasinda tasinabilir bir bag
     * yok. Ekran icin dogru cevap zaten telefonun kendisinde: sensore birakiyoruz.
     */
    private fun applyScreenOrientation() {
        requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_FULL_SENSOR
    }

    override fun onResume() {
        super.onResume()
        handler.post(refresh)
    }

    override fun onPause() {
        handler.removeCallbacks(refresh)
        StreamService.attachPreview(null, 0, 0)
        previewAttached = false
        super.onPause()
    }

    // --------------------------------------------------------------------- ui

    private fun buildUi(): View {
        val root = FrameLayout(this)

        previewView = SurfaceView(this).apply { holder.addCallback(previewCallback) }
        root.addView(previewView, FrameLayout.LayoutParams(MATCH_PARENT, MATCH_PARENT))

        val pad = (12 * resources.displayMetrics.density).toInt()
        statusView = TextView(this).apply {
            typeface = Typeface.MONOSPACE
            textSize = 12f
            setPadding(pad, pad / 2, pad, pad / 2)
            setBackgroundColor(0x99000000.toInt())
            setTextColor(0xFFDDDDDD.toInt())
        }
        root.addView(
            statusView,
            FrameLayout.LayoutParams(MATCH_PARENT, FrameLayout.LayoutParams.WRAP_CONTENT).apply {
                gravity = Gravity.BOTTOM
            }
        )
        return root
    }

    private val previewCallback = object : SurfaceHolder.Callback {
        override fun surfaceCreated(holder: SurfaceHolder) = Unit

        override fun surfaceChanged(holder: SurfaceHolder, format: Int, w: Int, h: Int) {
            previewSurface = holder.surface
            previewWidth = w
            previewHeight = h
            previewAttached = false
            attachPreviewIfPossible()
        }

        override fun surfaceDestroyed(holder: SurfaceHolder) {
            StreamService.attachPreview(null, 0, 0)
            previewSurface = null
            previewAttached = false
        }
    }

    private fun attachPreviewIfPossible() {
        if (previewAttached) return
        val surface = previewSurface?.takeIf { it.isValid } ?: return
        if (!StreamService.state.running) return
        StreamService.attachPreview(surface, previewWidth, previewHeight)
        previewAttached = true
        lastAttachAt = System.currentTimeMillis()
    }

    /**
     * Baglama basarisiz olduysa yeniden dene.
     *
     * EGL yuzeyi acilamadiginda onizleme sessizce olu kaliyordu: `previewAttached`
     * true oldugu icin bir daha hic denenmiyordu. Ekran donuslerinde onizlemenin
     * "donup kalmasi"nin ikinci yarisi buydu.
     */
    private fun retryPreviewIfDead() {
        if (!previewAttached || !StreamService.state.running) return
        if (StreamService.previewActive()) return
        if (System.currentTimeMillis() - lastAttachAt < PREVIEW_RETRY_MS) return
        previewAttached = false
        attachPreviewIfPossible()
    }

    private var lastAttachAt = 0L

    // ------------------------------------------------------------------ akis

    private fun startStreamIfNeeded() {
        if (StreamService.state.running) return
        StreamService.start(this, StreamConfig.load(this))
    }

    private fun updateStatus() {
        val state = StreamService.state
        val ip = state.address ?: StreamService.localIpAddress() ?: "?"
        statusView.text = buildString {
            append(if (state.running) "● " else "○ ")
            append(ip)
            append("  ")
            append(state.detail)
            StreamService.rotationInfo()?.let { append("\n").append(it) }
            StreamService.statsProvider?.invoke()?.let {
                append("\n").append(it.replace("\n", "  "))
            }
        }
        if (state.running) {
            attachPreviewIfPossible()
            retryPreviewIfDead()
        } else {
            previewAttached = false
        }
    }

    // ----------------------------------------------------------------- izin

    private fun ensurePermissions(): Boolean {
        val needed = mutableListOf<String>()
        if (checkSelfPermission(Manifest.permission.CAMERA) != PackageManager.PERMISSION_GRANTED) {
            needed += Manifest.permission.CAMERA
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
            != PackageManager.PERMISSION_GRANTED
        ) {
            needed += Manifest.permission.POST_NOTIFICATIONS
        }
        if (needed.isEmpty()) return true
        requestPermissions(needed.toTypedArray(), REQUEST_CODE)
        return false
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode != REQUEST_CODE) return
        if (checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) {
            startStreamIfNeeded()
        } else {
            statusView.text = "Kamera izni verilmedi"
        }
    }

    private companion object {
        const val REQUEST_CODE = 100
        const val PREVIEW_RETRY_MS = 1_500L
    }
}
