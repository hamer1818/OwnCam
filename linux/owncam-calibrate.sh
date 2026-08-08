#!/usr/bin/env bash
#
# Dogru goruntu donusunu gozle olcup telefona kaydet.
#
# Donus sensor acisindan **turetilemiyor**: elde iki bagimsiz gozlem var ve
# ikisi de birbirinden farkli iki formulle ayni derecede uyusuyor, yani veri
# formulu secmeye yetmiyor (bkz. CLAUDE.md "Known issues"). Turetmek yerine
# olcuyoruz: dort acinin her birinde bir kare yakalayip yan yana koyuyoruz,
# dogru olani sen seciyorsun, secim telefona kaydediliyor.
#
#   owncam-calibrate.sh          # dort kareyi yakala, sor, kaydet
#   owncam-calibrate.sh --sheet  # yalnizca kontak sayfasini uret, sorma
#
# On kosul: telefon yayinda ve alici calisiyor (owncam-receive.sh ya da
# masaustu uygulamasi), yani $OWNCAM_DEVICE guncel kareleri tasiyor.
set -uo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
DEVICE="${OWNCAM_DEVICE:-/dev/video11}"
STATUS_PORT="${OWNCAM_STATUS_PORT:-5300}"
OUT_DIR="${OWNCAM_CALIBRATE_DIR:-${TMPDIR:-/tmp}/owncam-kalibrasyon}"
SHEET="$OUT_DIR/karsilastirma.png"
ROTATIONS=(0 90 180 270)

ask=1
[[ "${1:-}" == "--sheet" ]] && ask=0

log() { printf '[owncam] %s\n' "$*" >&2; }

for tool in ffmpeg curl montage; do
    command -v "$tool" >/dev/null 2>&1 || { log "$tool bulunamadi"; exit 1; }
done

host="${OWNCAM_HOST:-}"
if [[ -z "$host" ]]; then
    found=$("$SCRIPT_DIR/owncam-discover.sh" 2>/dev/null) || {
        log "telefon bulunamadi (mDNS). OWNCAM_HOST ile elle verebilirsin."
        exit 1
    }
    host="${found%% *}"
fi

phone() { curl -fsS --max-time 5 "http://${host}:${STATUS_PORT}$1" 2>/dev/null; }

status=$(phone "/status") || { log "durum ucuna ulasilamadi: $host:$STATUS_PORT"; exit 1; }
original=$(printf '%s' "$status" | sed -n 's/.*"imageRotation": *\([0-9]*\).*/\1/p')
log "telefon $host · mevcut donus ${original:-?}°"

if [[ ! -r "$DEVICE" ]]; then
    log "$DEVICE okunamiyor. Alici calisiyor mu? (owncam-receive.sh)"
    exit 1
fi

mkdir -p "$OUT_DIR"

# Kare boyutu 0/180 ile 90/270 arasinda degistigi icin telefon o gecislerde
# akisi yeniden kuruyor ve alicinin yeniden baglanmasi bir saniye suruyor.
# Bu yuzden yakalamayi birkac kez deniyoruz.
capture() {
    local target="$1" attempt
    for attempt in 1 2 3 4 5 6; do
        if ffmpeg -hide_banner -loglevel error -f v4l2 -i "$DEVICE" \
                  -frames:v 1 -y "$target" 2>/dev/null; then
            [[ -s "$target" ]] && return 0
        fi
        sleep 1
    done
    return 1
}

shots=()
for deg in "${ROTATIONS[@]}"; do
    phone "/rotate?deg=$deg" >/dev/null || { log "donus $deg ayarlanamadi"; exit 1; }
    # Yeni ayarin kodlayiciya ve alicinin cozucusune ulasmasi icin.
    sleep 3
    shot="$OUT_DIR/donus-$deg.png"
    if capture "$shot"; then
        size=$(ffprobe -v error -select_streams v:0 -show_entries stream=width,height \
               -of csv=p=0:s=x "$shot" 2>/dev/null || echo "?")
        log "donus ${deg}° yakalandi ($size)"
        shots+=("$shot")
    else
        log "donus ${deg}° yakalanamadi, atlaniyor"
    fi
done

if [[ ${#shots[@]} -eq 0 ]]; then
    log "hic kare yakalanamadi"
    [[ -n "$original" ]] && phone "/rotate?deg=$original" >/dev/null
    exit 1
fi

# Kareler artik farkli boyutlarda olabiliyor (yatay 1280x720, dik 720x1280);
# montage hepsini ayni kutuya sigdirip altina aciyi yaziyor.
montage -label '%f' -background '#202020' -fill white \
        -tile "${#shots[@]}x1" -geometry '360x360>+8+8' \
        "${shots[@]}" "$SHEET" 2>/dev/null

log "kontak sayfasi: $SHEET"

if [[ $ask -eq 0 ]]; then
    [[ -n "$original" ]] && phone "/rotate?deg=$original" >/dev/null
    exit 0
fi

command -v xdg-open >/dev/null 2>&1 && xdg-open "$SHEET" >/dev/null 2>&1 &

echo
echo "Dort kare $SHEET dosyasinda yan yana."
echo "Goruntunun duz durdugu aciyi sec."
read -rp "Donus [0/90/180/270, bos = ${original:-0}° kalsin]: " choice

choice="${choice:-$original}"
case "$choice" in
    0|90|180|270) ;;
    *) log "gecersiz deger, donus ${original:-0}° birakiliyor"; choice="$original" ;;
esac

phone "/rotate?deg=$choice" >/dev/null || { log "kaydedilemedi"; exit 1; }
log "donus ${choice}° telefona kaydedildi (yeniden baslatmada korunur)"
