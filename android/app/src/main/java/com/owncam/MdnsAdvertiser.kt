package com.owncam

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.util.Log

/**
 * Telefonu yerel agda `_owncam._tcp` olarak duyurur.
 *
 * Boylece Linux tarafinda IP elle yazilmaz; `owncam-discover.sh` telefonu
 * avahi ile bulur (plan bolum 6, mDNS maddesi).
 */
class MdnsAdvertiser(context: Context) {

    private val nsdManager =
        context.getSystemService(Context.NSD_SERVICE) as NsdManager

    private var listener: NsdManager.RegistrationListener? = null

    fun register(port: Int) {
        if (listener != null) return

        val info = NsdServiceInfo().apply {
            serviceName = SERVICE_NAME
            serviceType = SERVICE_TYPE
            setPort(port)
        }

        val registrationListener = object : NsdManager.RegistrationListener {
            override fun onServiceRegistered(info: NsdServiceInfo) {
                Log.i(TAG, "mDNS kaydi: ${info.serviceName}")
            }

            override fun onRegistrationFailed(info: NsdServiceInfo, errorCode: Int) {
                // Kritik degil: kullanici IP'yi elle de girebilir.
                Log.w(TAG, "mDNS kaydi basarisiz: $errorCode")
            }

            override fun onServiceUnregistered(info: NsdServiceInfo) {
                Log.i(TAG, "mDNS kaydi silindi")
            }

            override fun onUnregistrationFailed(info: NsdServiceInfo, errorCode: Int) {
                Log.w(TAG, "mDNS silme basarisiz: $errorCode")
            }
        }

        listener = registrationListener
        runCatching { nsdManager.registerService(info, NsdManager.PROTOCOL_DNS_SD, registrationListener) }
            .onFailure { Log.w(TAG, "mDNS baslatilamadi: ${it.message}") }
    }

    fun unregister() {
        val current = listener ?: return
        listener = null
        runCatching { nsdManager.unregisterService(current) }
    }

    companion object {
        private const val TAG = "OwnCam/mDNS"
        const val SERVICE_NAME = "OwnCam"
        const val SERVICE_TYPE = "_owncam._tcp."
    }
}
