#!/usr/bin/env python3
"""
OwnCam masaustu kontrol uygulamasi.

Telefondan gelen goruntuyu canli gosterir ve ayarlari telefona uygular.

Tasarim karari: bu uygulama **alicinin kendisi**. ffmpeg'i tek sefer
calistirip ciktiyi ikiye ayiriyor:

    telefon (TCP) -> ffmpeg -+-> /dev/video11   (OBS, Zoom, Meet...)
                             +-> ham RGB -> bu pencere

Boylece `/dev/video11` uzerinde okuma-yazma cekismesi olmuyor. Ayri bir
alici + ayri bir izleyici calistirmak tam olarak o cekismeyi yaratiyor ve
akisi kilitliyordu.

Ayarlar telefonun :5300 ucuna HTTP ile gonderiliyor.
"""
import json
import math
import os
import shutil
import signal
import subprocess
import sys
import threading
import urllib.parse
import urllib.request

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("GdkPixbuf", "2.0")
from gi.repository import GdkPixbuf, GLib, Gtk  # noqa: E402

DEVICE = os.environ.get("OWNCAM_DEVICE", "/dev/video11")
STATUS_PORT = int(os.environ.get("OWNCAM_STATUS_PORT", "5300"))
STREAM_PORT = int(os.environ.get("OWNCAM_PORT", "5299"))

RESOLUTIONS = [(640, 480), (1280, 720), (1920, 1080)]
FPS_OPTIONS = [15, 24, 30, 60]
ROTATIONS = [0, 90, 180, 270]

# StreamConfig.FrameMode ile ayni anahtarlar.
FRAME_MODES = [
    ("sigdir", "Sigdir (tam kadraj)"),
    ("doldur", "Doldur (kirp, yatay kare sabit)"),
]

# Onizleme penceresi icin kucultulmus genislik: tam cozunurlukte ham RGB
# okumak bosuna bant genisligi ve CPU. Kare orani korunuyor.
PREVIEW_WIDTH = 640
PREVIEW_FPS = 15


def discover_phone(timeout=5):
    """Telefonu mDNS ile bul; bulunamazsa None."""
    if not shutil.which("avahi-browse"):
        return None
    try:
        out = subprocess.run(
            ["avahi-browse", "-rtpk", "_owncam._tcp"],
            capture_output=True, text=True, timeout=timeout,
        ).stdout
    except Exception:
        return None
    for line in out.splitlines():
        parts = line.split(";")
        if len(parts) > 8 and parts[0] == "=" and parts[2] == "IPv4":
            return parts[7]
    return None


class Phone:
    """Telefonun :5300 ucuyla konusan ince katman."""

    def __init__(self, host):
        self.host = host

    def _get(self, path, params=None):
        url = f"http://{self.host}:{STATUS_PORT}{path}"
        if params:
            url += "?" + urllib.parse.urlencode(params)
        with urllib.request.urlopen(url, timeout=4) as response:
            return json.loads(response.read().decode())

    def status(self):
        return self._get("/status")

    def configure(self, **params):
        return self._get("/config", params)

    def start(self):
        return self._get("/start")

    def stop(self):
        return self._get("/stop")


class Receiver:
    """ffmpeg alicisi: /dev/video11'e yazar, ayrica ham kare akitir."""

    def __init__(self, host, width, height, fps, on_frame, on_state):
        self.host = host
        self.width = width
        self.height = height
        self.fps = fps
        self.on_frame = on_frame
        self.on_state = on_state
        self.proc = None
        self.thread = None
        self.stop_flag = threading.Event()
        self.last_error = ""
        self.pending = threading.Semaphore(1)

        # Olcegi genislige degil **alana** gore seciyoruz: donus 90/270 iken
        # telefon dik kare gonderiyor (720x1280) ve genislige gore olceklemek
        # 640x1138'lik bir onizleme uretip boru trafigini uce katliyordu.
        # Alan sabit kalinca yatay da dik de ayni maliyette.
        budget = PREVIEW_WIDTH * PREVIEW_WIDTH * 9 / 16
        scale = math.sqrt(budget / float(width * height))
        self.pw = max(2, int(round(width * scale / 2)) * 2)
        self.ph = max(2, int(round(height * scale / 2)) * 2)

    def _deliver(self, data):
        try:
            self.on_frame(data, self.pw, self.ph)
        finally:
            self.pending.release()
        return False

    def start(self):
        if self.proc:
            return
        self.stop_flag.clear()
        self.thread = threading.Thread(target=self._run, daemon=True)
        self.thread.start()

    def stop(self):
        self.stop_flag.set()
        if self.proc:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.proc.kill()
            self.proc = None

    def _command(self):
        return [
            "ffmpeg", "-hide_banner", "-loglevel", "error",
            # Gecikme icin: ffmpeg'in ic tamponunu ve akis analizini kapat
            "-fflags", "nobuffer+discardcorrupt",
            "-flags", "low_delay",
            "-avioflags", "direct",
            # `-vf scale` filtre grafigi kare boyutunu onceden bilmek zorunda.
            # probesize 32 ile ffmpeg SPS'i cozemeden vazgecip
            # "unspecified size" ile cikiyordu. Buradaki degerler yalnizca
            # ilk baglanma anini etkiler; surekli gecikmeyi nobuffer/low_delay
            # belirliyor.
            "-probesize", "500000",
            "-analyzeduration", "500000",
            "-max_delay", "0",
            # Ham Annex-B'de zaman damgasi yok; kare hizini demuxer'a soyle
            "-r", str(self.fps),
            "-f", "h264", "-i", f"tcp://{self.host}:{STREAM_PORT}",
            # 1. cikis: sanal kamera
            "-map", "0:v", "-fps_mode", "passthrough",
            "-pix_fmt", "yuv420p", "-f", "v4l2", DEVICE,
            # 2. cikis: bu pencere. Onizlemeye 30 fps gerekmiyor; 15'e
            # dusurmek boru trafigini ve CPU'yu yariya indiriyor.
            "-map", "0:v",
            "-vf", f"scale={self.pw}:{self.ph},fps={PREVIEW_FPS}",
            "-pix_fmt", "rgb24", "-f", "rawvideo", "-",
        ]

    @staticmethod
    def _read_exactly(stream, count):
        """Boru okumasi kisa donebilir; tam kare gelene kadar birlestir."""
        chunks = []
        remaining = count
        while remaining > 0:
            chunk = stream.read(remaining)
            if not chunk:
                return None
            chunks.append(chunk)
            remaining -= len(chunk)
        return b"".join(chunks)

    def _drain_stderr(self, proc):
        try:
            for raw in iter(proc.stderr.readline, b""):
                line = raw.decode(errors="replace").strip()
                if line:
                    self.last_error = line
        except Exception:
            pass

    def _run(self):
        frame_bytes = self.pw * self.ph * 3
        while not self.stop_flag.is_set():
            try:
                self.proc = subprocess.Popen(
                    self._command(),
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    bufsize=0,
                )
                # stderr'i okuyan olmazsa boru dolunca ffmpeg yazarken
                # bloklanir ve tum isleme durur. Ayri bir is parcaciginda
                # surekli bosaltiyoruz, son satiri da teshis icin tutuyoruz.
                threading.Thread(
                    target=self._drain_stderr, args=(self.proc,), daemon=True
                ).start()
            except FileNotFoundError:
                GLib.idle_add(self.on_state, "ffmpeg bulunamadi")
                return

            GLib.idle_add(self.on_state, f"baglaniyor: {self.host}")
            got_frame = False
            while not self.stop_flag.is_set():
                data = self._read_exactly(self.proc.stdout, frame_bytes)
                if data is None:
                    break
                if not got_frame:
                    got_frame = True
                    GLib.idle_add(self.on_state, "yayinda")
                # Arayuz geride kaldiysa kare biriktirmiyoruz: en yenisi
                # gosterilir, eskisi atilir. Onizleme gecikmesi buyumesin.
                if self.pending.acquire(blocking=False):
                    GLib.idle_add(self._deliver, data)

            if self.proc:
                self.proc.kill()
                self.proc = None
            if self.stop_flag.is_set():
                return
            # ffmpeg'in son hata satirini goster; sessizce yeniden denemek
            # "baglaniyor" yazip duran bir pencere birakiyordu.
            detail = f" ({self.last_error})" if self.last_error else ""
            GLib.idle_add(self.on_state, f"baglanti koptu, yeniden deneniyor{detail}")
            self.stop_flag.wait(1.5)


class OwnCamWindow(Gtk.ApplicationWindow):

    def __init__(self, app, host):
        super().__init__(application=app, title="OwnCam")
        self.set_default_size(1000, 620)
        self.phone = Phone(host) if host else None
        self.receiver = None
        self.frame_size = None
        self.applying = False
        self.autostart_sent = False

        root = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, spacing=12)
        root.set_margin_top(12)
        root.set_margin_bottom(12)
        root.set_margin_start(12)
        root.set_margin_end(12)
        self.set_child(root)

        # --- sol: canli goruntu ---
        self.picture = Gtk.Picture()
        self.picture.set_content_fit(Gtk.ContentFit.CONTAIN)
        self.picture.set_hexpand(True)
        self.picture.set_vexpand(True)
        frame = Gtk.Frame()
        frame.set_child(self.picture)
        root.append(frame)

        # --- sag: kontroller ---
        side = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=8)
        side.set_size_request(300, -1)
        root.append(side)

        self.state_label = Gtk.Label(xalign=0)
        self.state_label.set_wrap(True)
        side.append(self.state_label)

        self.resolution = self._combo(
            side, "Cozunurluk", [f"{w}x{h}" for w, h in RESOLUTIONS], 1)
        self.fps = self._combo(
            side, "Kare hizi", [f"{f} fps" for f in FPS_OPTIONS], 2)
        self.rotation = self._combo(
            side, "Goruntu donusu", [f"{r}°" for r in ROTATIONS], 0)

        # Kamera her zaman yatay kare uretir. Telefon dik monte edilmisse o
        # yatay tampon dunyada dik bir alan gosterir; bu iki secenek o alanin
        # kareye nasil oturacagini belirliyor. Ikisi de siyah bant uretmez.
        self.frame_mode = self._combo(
            side, "Kadraj", [label for _, label in FRAME_MODES], 0)

        # Acikken donus telefonun fiziksel yonunden geliyor; elle secim
        # anlamsizlasiyor, o yuzden asagidaki liste kilitleniyor.
        self.auto_rotate = Gtk.CheckButton(label="Otomatik donus (telefonu takip et)")
        self.auto_rotate.set_active(True)
        self.auto_rotate.connect("toggled", self._on_change)
        side.append(self.auto_rotate)

        self.front = Gtk.CheckButton(label="On kamera")
        self.front.set_active(True)
        self.front.connect("toggled", self._on_change)
        side.append(self.front)

        self.exposure = Gtk.CheckButton(label="Pozlamayi kilitle")
        self.exposure.connect("toggled", self._on_change)
        side.append(self.exposure)

        self.preview_on_phone = Gtk.CheckButton(label="Telefonda onizleme")
        self.preview_on_phone.set_active(True)
        self.preview_on_phone.connect("toggled", self._on_change)
        side.append(self.preview_on_phone)

        self.toggle = Gtk.Button(label="Baslat")
        self.toggle.connect("clicked", self._on_toggle)
        side.append(self.toggle)

        self.stats = Gtk.Label(xalign=0)
        self.stats.set_wrap(True)
        self.stats.add_css_class("dim-label")
        side.append(self.stats)

        if not host:
            self._set_state("telefon bulunamadi (mDNS). Uygulama telefonda acik mi?")
        else:
            self._set_state(f"telefon: {host}")
            GLib.timeout_add_seconds(2, self._poll_status)

    def _combo(self, parent, label, items, default):
        parent.append(Gtk.Label(label=label, xalign=0))
        combo = Gtk.DropDown.new_from_strings(items)
        combo.set_selected(default)
        combo.connect("notify::selected", self._on_change)
        parent.append(combo)
        return combo

    # ------------------------------------------------------------ goruntu

    def _on_frame(self, data, width, height):
        pixbuf = GdkPixbuf.Pixbuf.new_from_data(
            data, GdkPixbuf.Colorspace.RGB, False, 8,
            width, height, width * 3,
        )
        self.picture.set_pixbuf(pixbuf)
        return False

    def _set_state(self, text):
        self.state_label.set_text(text)
        return False

    # ---------------------------------------------------------- kontroller

    def _current(self):
        w, h = RESOLUTIONS[self.resolution.get_selected()]
        auto = self.auto_rotate.get_active()
        params = dict(
            width=w, height=h,
            fps=FPS_OPTIONS[self.fps.get_selected()],
            mode=FRAME_MODES[self.frame_mode.get_selected()][0],
            auto=1 if auto else 0,
            front=1 if self.front.get_active() else 0,
            exposure=1 if self.exposure.get_active() else 0,
            preview=1 if self.preview_on_phone.get_active() else 0,
        )
        # Otomatikken donusu gondermiyoruz: telefon zaten kendi yonunden
        # belirliyor, gonderirsek bir sonraki yon okumasi hemen geri alir.
        if not auto:
            params["rotation"] = ROTATIONS[self.rotation.get_selected()]
        return params

    def _on_change(self, *_):
        # Arayuzu telefondan doldururken kendi degisikligimizi geri gondermeyelim
        if self.applying or not self.phone:
            return
        params = self._current()
        threading.Thread(
            target=self._apply, args=(params,), daemon=True).start()

    def _apply(self, params):
        try:
            self.phone.configure(**params)
        except Exception as e:
            GLib.idle_add(self._set_state, f"ayar gonderilemedi: {e}")
            return
        # Cozunurluk degistiyse alicinin da yeni boyuta gecmesi gerekiyor
        GLib.idle_add(self._restart_receiver)

    def _on_toggle(self, _button):
        if not self.phone:
            return
        running = self.receiver is not None
        threading.Thread(
            target=self._toggle_worker, args=(running,), daemon=True).start()

    def _toggle_worker(self, running):
        try:
            if running:
                self.phone.stop()
            else:
                self.phone.configure(**self._current())
                self.phone.start()
        except Exception as e:
            GLib.idle_add(self._set_state, f"komut gonderilemedi: {e}")
            return
        GLib.idle_add(self._restart_receiver if not running else self._stop_receiver)

    def _restart_receiver(self):
        """Aliciyi telefonun **bildirdigi** kare boyutuyla kur.

        Boyut artik onceden tahmin edilemiyor: otomatik donus acikken kare sekli
        telefonun fiziksel yonuyle degisiyor (dik tutunca 720x1280, yan tutunca
        1280x720). Tahmin etmek yerine durum ucundan okuyoruz; `_show_status`
        boyut degistiginde burayi yeniden cagiriyor.
        """
        self._stop_receiver()
        params = self._current()
        width, height = self.frame_size or (params["width"], params["height"])
        self.receiver = Receiver(
            self.phone.host, width, height, params["fps"],
            self._on_frame, self._set_state,
        )
        self.receiver.start()
        self.toggle.set_label("Durdur")
        return False

    def _stop_receiver(self):
        if self.receiver:
            self.receiver.stop()
            self.receiver = None
        self.toggle.set_label("Baslat")
        return False

    # ------------------------------------------------------------- durum

    def _poll_status(self):
        threading.Thread(target=self._poll_worker, daemon=True).start()
        return True

    def _poll_worker(self):
        try:
            status = self.phone.status()
        except Exception:
            GLib.idle_add(self._set_state, "telefona ulasilamiyor")
            return
        GLib.idle_add(self._show_status, status)

    def _show_status(self, s):
        auto = bool(s.get("autoRotate"))
        self.stats.set_text(
            f"{s.get('frame')} @ {s.get('fps')}fps · donus {s.get('appliedRotation')}°"
            + (" (oto)" if auto else "")
            + ("  DAR (kenarlar siyah)" if s.get("narrow") else "")
            + f"\nyakalama {s.get('resolution')}"
            + f" · telefon yonu {s.get('deviceOrientation')}°"
            + f"\ngonderilen {s.get('framesSent')} · dusen {s.get('framesDropped')}"
            + f"\nkamera->kodlayici {s.get('encoderOutputs')}"
        )

        # Kare boyutu degistiyse alici o boyutla yeniden kurulmali; `-vf scale`
        # filtre grafigi boyutu sabit tutuyor, eski boyutla okumaya devam etmek
        # goruntuyu bozar.
        frame = s.get("frame") or ""
        if "x" in frame:
            try:
                size = tuple(int(x) for x in frame.split("x"))
            except ValueError:
                size = None
            if size and size != self.frame_size:
                self.frame_size = size
                if self.receiver is not None:
                    self._restart_receiver()
        # Telefonun gercek ayarlarini arayuze yansit (dongu olmasin diye bayrakli)
        self.applying = True
        try:
            res = s.get("resolution") or ""
            if "x" in res:
                w, h = (int(x) for x in res.split("x"))
                if (w, h) in RESOLUTIONS:
                    self.resolution.set_selected(RESOLUTIONS.index((w, h)))
            if s.get("fps") in FPS_OPTIONS:
                self.fps.set_selected(FPS_OPTIONS.index(s["fps"]))
            if s.get("imageRotation") in ROTATIONS:
                self.rotation.set_selected(ROTATIONS.index(s["imageRotation"]))
            keys = [k for k, _ in FRAME_MODES]
            if s.get("frameMode") in keys:
                self.frame_mode.set_selected(keys.index(s["frameMode"]))
            self.auto_rotate.set_active(auto)
            # Otomatik acikken elle donus anlamsiz: telefon hemen geri alir.
            self.rotation.set_sensitive(not auto)
            self.front.set_active(s.get("camera") == "front")
            self.preview_on_phone.set_active(bool(s.get("preview")))
            self.exposure.set_active(bool(s.get("exposureLocked")))
        finally:
            self.applying = False

        if s.get("streaming"):
            # Telefon yayindaysa alici kendiliginden baglansin: uygulamayi
            # acinca goruntunun gelmesi icin ayrica dugmeye basmak gereksiz.
            if self.receiver is None:
                self._restart_receiver()
        else:
            if self.receiver is not None:
                self._stop_receiver()
            # Telefon yayinda degilse baslatmayi biz isteyelim. Aksi halde
            # uygulama "baglaniyor" yazip duruyor - baglanacak bir sey yok.
            if not self.autostart_sent:
                self.autostart_sent = True
                self._set_state("telefonda yayin kapali, baslatiliyor...")
                threading.Thread(target=self._autostart, daemon=True).start()
        return False

    def _autostart(self):
        try:
            self.phone.start()
        except Exception as e:
            GLib.idle_add(self._set_state, f"telefon baslatilamadi: {e}")
        finally:
            # Bir dahaki durumda tekrar denenebilsin
            GLib.timeout_add_seconds(5, self._clear_autostart)

    def _clear_autostart(self):
        self.autostart_sent = False
        return False

    def do_close_request(self):
        self._stop_receiver()
        return False


class OwnCamApp(Gtk.Application):

    def __init__(self, host):
        super().__init__(application_id="dev.owncam.Desktop")
        self.host = host

    def do_activate(self):
        OwnCamWindow(self, self.host).present()


def main():
    host = os.environ.get("OWNCAM_HOST")
    if not host and len(sys.argv) > 1:
        host = sys.argv[1]
    if not host:
        host = discover_phone()
    return OwnCamApp(host).run([])


if __name__ == "__main__":
    sys.exit(main())
