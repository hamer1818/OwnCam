#!/usr/bin/env bash
#
# /dev/video11'in etiketini "Iriun Webcam" yerine "OwnCam" yapar.
# Sadece kozmetik: OBS ve diger uygulamalar kaynagi bu isimle gosterir.
#
# Modulu yeniden yuklemek gerekiyor, bu yuzden cihazi tutan her sey once
# birakmali: owncam servisi, elle baslatilmis aliciler ve OBS.
set -euo pipefail

CONF=/etc/modprobe.d/v4l2loopback.conf
NEW='options v4l2loopback devices=2 video_nr=10,11 card_label="OBS Virtual Camera,OwnCam" exclusive_caps=1,1'

if [[ $EUID -ne 0 ]]; then
    echo "sudo ile calistir: sudo $0" >&2
    exit 1
fi

# sudo altinda calisiyoruz; kullanici servisine erismek icin gercek kullanici.
REAL_USER="${SUDO_USER:-}"
REAL_UID="$(id -u "${REAL_USER:-root}" 2>/dev/null || echo 0)"

user_systemctl() {
    [[ -n "$REAL_USER" ]] || return 0
    runuser -u "$REAL_USER" -- \
        env "XDG_RUNTIME_DIR=/run/user/$REAL_UID" systemctl --user "$@" 2>/dev/null || true
}

if pgrep -x obs >/dev/null 2>&1; then
    echo "OBS calisiyor. Once kapat, sonra tekrar calistir." >&2
    exit 1
fi

# --- config ---------------------------------------------------------------
if grep -q 'card_label="OBS Virtual Camera,OwnCam"' "$CONF" 2>/dev/null; then
    echo "config zaten OwnCam; sadece modul yeniden yuklenecek."
else
    cp -a "$CONF" "$CONF.oncesi.$(date +%Y%m%d%H%M%S)"
    echo "$NEW" > "$CONF"
    echo "yazildi: $CONF (yedek alindi)"
fi

# --- cihazi birak ---------------------------------------------------------
was_active=0
if [[ "$(user_systemctl is-active owncam)" == "active" ]]; then
    was_active=1
    echo "owncam servisi durduruluyor..."
    user_systemctl stop owncam
fi

# Servis disinda elle baslatilmis alicilar da olabilir.
for dev in /dev/video10 /dev/video11; do
    for pid in $(fuser "$dev" 2>/dev/null); do
        echo "$dev'i tutan pid $pid sonlandiriliyor"
        kill "$pid" 2>/dev/null || true
    done
done

for i in $(seq 10); do
    fuser /dev/video10 /dev/video11 >/dev/null 2>&1 || break
    sleep 0.5
done

if fuser /dev/video10 /dev/video11 >/dev/null 2>&1; then
    echo "cihazlar hala kullanimda:" >&2
    fuser -v /dev/video10 /dev/video11 >&2 || true
    echo "bunlari kapatip tekrar dene. (config yazildi, sonraki acilista da gecerli)" >&2
    exit 1
fi

# --- modulu yeniden yukle -------------------------------------------------
echo "modul yeniden yukleniyor..."
modprobe -r v4l2loopback
modprobe v4l2loopback

if [[ $was_active -eq 1 ]]; then
    echo "owncam servisi yeniden baslatiliyor..."
    user_systemctl start owncam
fi

echo
v4l2-ctl --list-devices | grep -A1 -E "OwnCam|OBS Virtual" || true
