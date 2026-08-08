#!/usr/bin/env bash
#
# OwnCam Faz 0 alicisi: telefondan H.264 (Annex-B/TCP) al, /dev/video11'e yaz.
#
# Bu sistemde /dev/video11 v4l2loopback kaynak cihazi, /dev/video10 ise OBS'in
# sanal kamerasi. Zincir:  telefon -> video11 -> OBS (arka plan kaldirma) -> video10
#
# Kullanim:
#   owncam-receive.sh                 # telefonu mDNS ile bul
#   owncam-receive.sh 192.168.1.42    # IP'yi elle ver
#   owncam-receive.sh 192.168.1.42 5299
#
# Ortam degiskenleri:
#   OWNCAM_DEVICE   hedef v4l2 cihazi        (varsayilan /dev/video11)
#   OWNCAM_PORT     telefonun dinledigi port (varsayilan 5299)
#   OWNCAM_FPS      telefondaki kare hizi    (varsayilan 30)
#   OWNCAM_ONCE     1 ise yeniden baglanma
set -uo pipefail

DEVICE="${OWNCAM_DEVICE:-/dev/video11}"
FPS="${OWNCAM_FPS:-30}"
PORT="${2:-${OWNCAM_PORT:-5299}}"
HOST="${1:-}"
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

log() { printf '[owncam] %s\n' "$*" >&2; }

for tool in ffmpeg; do
    command -v "$tool" >/dev/null 2>&1 || { log "$tool bulunamadi"; exit 1; }
done

if [[ ! -w "$DEVICE" ]]; then
    log "$DEVICE yazilabilir degil. v4l2loopback yuklu mu, 'video' grubunda misin?"
    log "  lsmod | grep v4l2loopback   /   id -nG"
    exit 1
fi

resolve_host() {
    if [[ -n "$HOST" ]]; then
        echo "$HOST $PORT"
        return 0
    fi
    local found
    if found=$("$SCRIPT_DIR/owncam-discover.sh" 2>/dev/null); then
        echo "$found"
        return 0
    fi
    return 1
}

# ffmpeg bayraklari - hepsi gecikme icin:
#   nobuffer + low_delay : ffmpeg'in ic tamponunu kapatir
#   probesize/analyzeduration : baslangictaki akis analizi beklemesini kaldirir
#   fps_mode passthrough : ffmpeg kare cogaltip/dusurup zamanlama duzeltmesin
#   max_delay 0 + avioflags direct : ara tamponlama yok
# Bunlar olmadan ffmpeg tek basina 500 ms+ ekler.
#
# Giristeki `-r $FPS`: ham Annex-B akisinda hic zaman damgasi yok, her kare
# DTS=0 ile geliyor ve ffmpeg saniyede 30 satir "Non-monotonic DTS" uyarisi
# basiyordu. Demuxer'a kare hizini soyleyince damgalar duzgun artiyor.
# Damgalar zaten kurgusal: v4l2loopback kareyi yazildigi anda damgaliyor.
run_stream() {
    local host="$1" port="$2"
    log "baglaniyor: tcp://$host:$port -> $DEVICE"
    ffmpeg -hide_banner -loglevel warning \
        -fflags nobuffer+discardcorrupt \
        -flags low_delay \
        -avioflags direct \
        -probesize 32 \
        -analyzeduration 0 \
        -max_delay 0 \
        -r "$FPS" \
        -f h264 -i "tcp://${host}:${port}" \
        -fps_mode passthrough \
        -pix_fmt yuv420p \
        -f v4l2 "$DEVICE"
}

trap 'log "durduruldu"; exit 0' INT TERM

backoff=1
while true; do
    if ! read -r host port <<<"$(resolve_host)"; then
        log "telefon bulunamadi (mDNS). ${backoff}s sonra tekrar."
        sleep "$backoff"
        backoff=$(( backoff < 8 ? backoff * 2 : 8 ))
        continue
    fi

    started=$SECONDS
    run_stream "$host" "$port"
    status=$?

    if [[ "${OWNCAM_ONCE:-0}" == "1" ]]; then
        exit "$status"
    fi

    # Akis bir sure calistiysa bu gecici bir kopma; geri cekilmeyi sifirla.
    if (( SECONDS - started > 5 )); then
        backoff=1
    fi

    log "akis bitti (cikis $status). ${backoff}s sonra yeniden baglaniliyor."
    sleep "$backoff"
    backoff=$(( backoff < 8 ? backoff * 2 : 8 ))
done
