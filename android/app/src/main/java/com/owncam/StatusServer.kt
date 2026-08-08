package com.owncam

import android.util.Log
import java.io.BufferedReader
import java.io.IOException
import java.io.InputStreamReader
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.URLDecoder
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Kucuk bir HTTP ucu: PC'nin telefonun o anki ayarlarini okumasi icin.
 *
 *   GET /status          -> JSON durum
 *   GET /rotate?deg=90   -> elle donus duzeltmesini ayarla, JSON durum doner
 *
 * Amac teshis: hangi ayarla hangi goruntunun gittigini PC tarafindan
 * gormek, telefona her seferinde elle bakmak zorunda kalmamak.
 *
 * Kimlik dogrulamasi yok - video akisiyla ayni guven seviyesinde, yerel ag
 * icin. PC istemci oldugu icin Linux'ta guvenlik duvari kurali gerekmiyor.
 */
class StatusServer(
    private val port: Int,
    private val statusJson: () -> String,
    /** Yol ve sorgu parametreleri; masaustu uygulamasi buradan ayar degistirir. */
    private val onCommand: (path: String, params: Map<String, String>) -> Unit
) {

    private val running = AtomicBoolean(false)
    private var serverSocket: ServerSocket? = null
    private var thread: Thread? = null

    fun start() {
        if (!running.compareAndSet(false, true)) return
        try {
            val socket = ServerSocket()
            socket.reuseAddress = true
            socket.bind(InetSocketAddress(port))
            serverSocket = socket
            thread = Thread({ acceptLoop(socket) }, "owncam-status").apply {
                isDaemon = true
                start()
            }
            Log.i(TAG, "durum sunucusu :$port")
        } catch (e: IOException) {
            running.set(false)
            Log.w(TAG, "durum sunucusu acilamadi: ${e.message}")
        }
    }

    fun stop() {
        if (!running.compareAndSet(true, false)) return
        runCatching { serverSocket?.close() }
        thread?.interrupt()
        thread = null
    }

    private fun acceptLoop(socket: ServerSocket) {
        while (running.get() && !socket.isClosed) {
            val client = try {
                socket.accept()
            } catch (e: IOException) {
                break
            }
            client.use { handle(it) }
        }
    }

    private fun handle(client: java.net.Socket) {
        try {
            client.soTimeout = 3000
            val reader = BufferedReader(InputStreamReader(client.getInputStream()))
            val requestLine = reader.readLine() ?: return
            val path = requestLine.split(" ").getOrNull(1) ?: "/"

            onCommand(path.substringBefore('?'), parseQuery(path))

            val body = statusJson()
            val response = buildString {
                append("HTTP/1.1 200 OK\r\n")
                append("Content-Type: application/json; charset=utf-8\r\n")
                append("Content-Length: ${body.toByteArray().size}\r\n")
                append("Connection: close\r\n")
                append("\r\n")
                append(body)
            }
            client.getOutputStream().apply {
                write(response.toByteArray())
                flush()
            }
        } catch (e: Exception) {
            Log.w(TAG, "istek islenemedi: ${e.message}")
        }
    }

    private fun parseQuery(path: String): Map<String, String> {
        val query = path.substringAfter('?', "")
        if (query.isEmpty()) return emptyMap()
        return query.split("&").mapNotNull { part ->
            val key = part.substringBefore('=')
            if (key.isEmpty()) return@mapNotNull null
            val value = runCatching {
                URLDecoder.decode(part.substringAfter('=', ""), "UTF-8")
            }.getOrDefault("")
            key to value
        }.toMap()
    }

    companion object {
        private const val TAG = "OwnCam/Status"
        const val DEFAULT_PORT = 5300
    }
}
