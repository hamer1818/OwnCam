package com.owncam.gl

import android.opengl.EGL14
import android.opengl.EGLConfig
import android.opengl.EGLContext
import android.opengl.EGLDisplay
import android.opengl.EGLExt
import android.opengl.EGLSurface
import android.view.Surface

/**
 * Asgari EGL kurulumu. Tek bir GLES2 baglami, birden fazla pencere yuzeyi:
 * biri MediaCodec'in giris yuzeyi, digeri (varsa) ekrandaki onizleme.
 *
 * Ikisi ayni baglami paylastigi icin kamera dokusu bir kez yuklenip iki kez
 * ciziliyor - onizleme ek bir kamera akisi acmiyor.
 */
class EglCore {

    private var display: EGLDisplay = EGL14.EGL_NO_DISPLAY
    private var context: EGLContext = EGL14.EGL_NO_CONTEXT
    private var config: EGLConfig? = null

    init {
        display = EGL14.eglGetDisplay(EGL14.EGL_DEFAULT_DISPLAY)
        check(display != EGL14.EGL_NO_DISPLAY) { "eglGetDisplay basarisiz" }

        val version = IntArray(2)
        check(EGL14.eglInitialize(display, version, 0, version, 1)) {
            "eglInitialize basarisiz"
        }

        // EGL_RECORDABLE_ANDROID sart: bu bayrak olmadan secilen config
        // MediaCodec'in giris yuzeyiyle uyumsuz olabilir.
        val attribs = intArrayOf(
            EGL14.EGL_RED_SIZE, 8,
            EGL14.EGL_GREEN_SIZE, 8,
            EGL14.EGL_BLUE_SIZE, 8,
            EGL14.EGL_ALPHA_SIZE, 8,
            EGL14.EGL_RENDERABLE_TYPE, EGL14.EGL_OPENGL_ES2_BIT,
            EGL_RECORDABLE_ANDROID, 1,
            EGL14.EGL_NONE
        )
        val configs = arrayOfNulls<EGLConfig>(1)
        val numConfigs = IntArray(1)
        check(
            EGL14.eglChooseConfig(display, attribs, 0, configs, 0, 1, numConfigs, 0) &&
                numConfigs[0] > 0
        ) { "uygun EGL config bulunamadi" }
        config = configs[0]

        val contextAttribs = intArrayOf(EGL14.EGL_CONTEXT_CLIENT_VERSION, 2, EGL14.EGL_NONE)
        context = EGL14.eglCreateContext(display, config, EGL14.EGL_NO_CONTEXT, contextAttribs, 0)
        check(context != EGL14.EGL_NO_CONTEXT) { "eglCreateContext basarisiz" }
    }

    fun createWindowSurface(surface: Surface): EGLSurface {
        val attribs = intArrayOf(EGL14.EGL_NONE)
        val eglSurface = EGL14.eglCreateWindowSurface(display, config, surface, attribs, 0)
        check(eglSurface != null && eglSurface != EGL14.EGL_NO_SURFACE) {
            "eglCreateWindowSurface basarisiz (0x${Integer.toHexString(EGL14.eglGetError())})"
        }
        return eglSurface
    }

    fun makeCurrent(eglSurface: EGLSurface) {
        check(EGL14.eglMakeCurrent(display, eglSurface, eglSurface, context)) {
            "eglMakeCurrent basarisiz"
        }
    }

    fun swapBuffers(eglSurface: EGLSurface): Boolean =
        EGL14.eglSwapBuffers(display, eglSurface)

    /**
     * 0 = vsync'i bekleme.
     *
     * Onizleme yuzeyi varsayilan olarak vsync'e kilitli; SurfaceView gorunmez
     * oldugunda `eglSwapBuffers` suresiz bloklayabiliyor. GL is parcacigi orada
     * kilitlenince kodlayiciya kare gitmiyor ve **yayin tumden duruyor**.
     * Onizlemenin yayini durdurma yetkisi olmamali.
     */
    fun setSwapInterval(interval: Int) {
        EGL14.eglSwapInterval(display, interval)
    }

    /**
     * Kodlayici yuzeyinde kare zaman damgasi. SurfaceTexture'in nanosaniye
     * cinsinden damgasini oldugu gibi geciriyoruz ki kodlayici kendi saatini
     * uydurmasin.
     */
    fun setPresentationTime(eglSurface: EGLSurface, nsecs: Long) {
        EGLExt.eglPresentationTimeANDROID(display, eglSurface, nsecs)
    }

    fun releaseSurface(eglSurface: EGLSurface) {
        EGL14.eglDestroySurface(display, eglSurface)
    }

    fun release() {
        if (display != EGL14.EGL_NO_DISPLAY) {
            EGL14.eglMakeCurrent(
                display, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_CONTEXT
            )
            EGL14.eglDestroyContext(display, context)
            EGL14.eglReleaseThread()
            EGL14.eglTerminate(display)
        }
        display = EGL14.EGL_NO_DISPLAY
        context = EGL14.EGL_NO_CONTEXT
        config = null
    }

    private companion object {
        const val EGL_RECORDABLE_ANDROID = 0x3142
    }
}
