package com.owncam

import android.util.Log
import java.io.BufferedOutputStream
import java.io.IOException
import java.io.OutputStream
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

/**
 * Telefon TCP **sunucu**, PC istemci.
 *
 * Bu yon bilincli secildi: PC'de `ufw` gelen baglantilari DROP ediyor, giden
 * trafik zaten ACCEPT. Boylece Linux tarafinda hicbir guvenlik duvari kurali
 * gerekmiyor (plan bolum 6 notu).
 *
 * Tel formati: **ham Annex-B H.264 byte akisi** (her NAL'in onunde 00 00 00 01
 * baslangic kodu). Uzunluk-onekli cerceveleme kullanilmiyor; boylece Linux
 * tarafinda `ffmpeg -f h264 -i tcp://...` hic ek kod olmadan cozebiliyor.
 * MediaCodec zaten Annex-B uretiyor, donusum maliyeti de sifir.
 */
class TcpVideoServer(
    private val port: Int,
    /** Yeni istemci baglandiginda kodlayicidan anahtar kare istemek icin. */
    private val onClientConnected: () -> Unit
) {

    /**
     * Kodlayici is parcacigi ile ag is parcacigini ayiran kuyruk.
     *
     * Derinlik 2: WiFi tikanirsa kareler burada birikip gecikme yaratmasin
     * diye **en eskisi atilir** (plan bolum 6: "en yeni kareyi al, eskileri at").
     * Blokli bir yazma yerine bilincli kare dusurmeyi tercih ediyoruz.
     */
    private val queue = ArrayBlockingQueue<Frame>(2)

    private val running = AtomicBoolean(false)
    private var serverSocket: ServerSocket? = null
    private var acceptThread: Thread? = null
    private var senderThread: Thread? = null

    @Volatile private var client: Socket? = null
    @Volatile private var out: OutputStream? = null

    /** Kodlayicidan gelen SPS/PPS. Her yeni istemciye ilk once bu gider. */
    @Volatile private var codecConfig: ByteArray? = null

    /**
     * Sonraki karelerden once gonderilecek SPS/PPS.
     *
     * Yalnizca gonderici is parcacigi yazar; kodlayiciyi bosaltan is parcacigi
     * asla sokete dokunmaz. Aksi halde PC okumayi birakinca soket yazimi
     * bloklaniyor, drain duruyor, kodlayici doluyor, GL `swapBuffers`
     * bloklaniyor ve **telefonun tamami kilitleniyordu**.
     */
    @Volatile private var pendingConfig: ByteArray? = null

    val bytesSent = AtomicLong(0)
    val framesSent = AtomicLong(0)
    val framesDropped = AtomicLong(0)

    /** Istemci bagli olmadigi icin gonderilmeyen kareler. */
    val framesSkipped = AtomicLong(0)

    /** Son basarili yazimin zamani; takilan istemciyi tespit etmek icin. */
    @Volatile private var lastWriteNanos = System.nanoTime()

    @Volatile var clientAddress: String? = null
        private set

    private class Frame(val data: ByteArray, val isKeyFrame: Boolean)

    fun start() {
        if (!running.compareAndSet(false, true)) return

        val socket = ServerSocket()
        socket.reuseAddress = true
        socket.bind(InetSocketAddress(port))
        serverSocket = socket

        acceptThread = Thread({ acceptLoop(socket) }, "owncam-accept").apply {
            isDaemon = true
            start()
        }
        senderThread = Thread({ sendLoop() }, "owncam-sender").apply {
            isDaemon = true
            priority = Thread.MAX_PRIORITY
            start()
        }
        Log.i(TAG, "TCP sunucu :$port dinlemede")
    }

    fun stop() {
        if (!running.compareAndSet(true, false)) return
        runCatching { serverSocket?.close() }
        dropClient()
        queue.clear()
        acceptThread?.interrupt()
        senderThread?.interrupt()
        acceptThread = null
        senderThread = null
        Log.i(TAG, "TCP sunucu durdu")
    }

    /**
     * Kodlayici SPS/PPS uretti. Sonraki her istemci icin sakla ve bagli
     * istemciye gonderilmek uzere sikaya koy. **Bloklamaz** - kodlayiciyi
     * bosaltan is parcacigindan cagriliyor.
     */
    fun setCodecConfig(data: ByteArray) {
        codecConfig = data
        if (out != null) pendingConfig = data
    }

    /**
     * Kodlayici is parcacigindan cagrilir. Asla bloklamaz.
     * Kuyruk doluysa en eski kare atilir.
     */
    fun submitFrame(data: ByteArray, isKeyFrame: Boolean) {
        if (out == null) {
            // Istemci yokken kareler sessizce atiliyordu; sayilmazsa
            // "kodlayici 4400 uretti ama 231 gitti" farki aciklanamiyor.
            framesSkipped.incrementAndGet()
            return
        }
        val frame = Frame(data, isKeyFrame)
        while (!queue.offer(frame)) {
            val discarded = queue.poll() ?: break
            framesDropped.incrementAndGet()
            // Atilan bir P-kare, bir sonraki anahtar kareye kadar cozucide
            // bozulmaya yol acar. Hemen yeni anahtar kare isteyip toparliyoruz.
            if (!discarded.isKeyFrame) onClientConnected()
        }

        // Kuyruk dolup duruyorsa ve uzun suredir tek bir kare bile
        // yazilamadiysa istemci olmus demektir (PC tarafinda ffmpeg
        // kilitlendiginde oluyor). Soketi kapatiyoruz: gonderici is
        // parcacigindaki bloklu yazim IOException'a duser, PC de yeniden
        // baglanir. Kendi kendini toparlayan tek nokta burasi.
        if (System.nanoTime() - lastWriteNanos > STALL_TIMEOUT_NANOS) {
            Log.w(TAG, "istemci ${STALL_TIMEOUT_NANOS / 1_000_000_000} sn yazmadi, kapatiliyor")
            dropClient()
        }

    }

    private fun acceptLoop(socket: ServerSocket) {
        while (running.get() && !socket.isClosed) {
            val newClient = try {
                socket.accept()
            } catch (e: IOException) {
                if (running.get()) Log.w(TAG, "accept hatasi: ${e.message}")
                break
            }

            // Tek istemci modeli: yeni baglanan oncekinin yerini alir.
            dropClient()
            queue.clear()

            try {
                newClient.tcpNoDelay = true          // Nagle kapali: gecikme > verim
                newClient.sendBufferSize = 256 * 1024
                newClient.keepAlive = true
                client = newClient
                out = BufferedOutputStream(newClient.getOutputStream(), 64 * 1024)
                clientAddress = newClient.inetAddress?.hostAddress
                lastWriteNanos = System.nanoTime()
                Log.i(TAG, "istemci baglandi: $clientAddress")

                // Sirasi onemli: once SPS/PPS, sonra anahtar kare istegi.
                // Yazimi gonderici is parcacigi yapiyor, burada bloklanmiyoruz.
                pendingConfig = codecConfig
                onClientConnected()
            } catch (e: IOException) {
                Log.w(TAG, "istemci kurulamadi: ${e.message}")
                dropClient()
            }
        }
    }

    private fun sendLoop() {
        while (running.get()) {
            val frame = try {
                queue.poll(200, TimeUnit.MILLISECONDS) ?: continue
            } catch (e: InterruptedException) {
                break
            }
            val stream = out ?: continue
            try {
                pendingConfig?.let { config ->
                    pendingConfig = null
                    stream.write(config)
                    bytesSent.addAndGet(config.size.toLong())
                }
                stream.write(frame.data)
                stream.flush()
                bytesSent.addAndGet(frame.data.size.toLong())
                framesSent.incrementAndGet()
                lastWriteNanos = System.nanoTime()
            } catch (e: IOException) {
                Log.i(TAG, "istemci koptu: ${e.message}")
                dropClient()
            }
        }
    }

    private fun dropClient() {
        runCatching { out?.close() }
        runCatching { client?.close() }
        out = null
        client = null
        clientAddress = null
    }

    val isClientConnected: Boolean get() = out != null

    companion object {
        private const val TAG = "OwnCam/Tcp"
        private const val STALL_TIMEOUT_NANOS = 3_000_000_000L
    }
}
