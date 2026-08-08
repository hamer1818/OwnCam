#!/usr/bin/env bash
#
# Telefonun o anki ayarlarini PC'den oku.
#
#   owncam-status.sh                 # ozet tablo
#   owncam-status.sh --json          # ham JSON
#   owncam-status.sh --rotate 90     # donusu ayarla ve telefona kaydet
#
# Dogru donusu gozle bulmak icin: owncam-calibrate.sh
#
# Telefon mDNS ile bulunur; OWNCAM_HOST ile elle de verilebilir.
set -uo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
STATUS_PORT="${OWNCAM_STATUS_PORT:-5300}"

resolve_host() {
    if [[ -n "${OWNCAM_HOST:-}" ]]; then
        echo "$OWNCAM_HOST"
        return 0
    fi
    local found
    if found=$("$SCRIPT_DIR/owncam-discover.sh" 2>/dev/null); then
        echo "${found%% *}"
        return 0
    fi
    return 1
}

host=$(resolve_host) || { echo "telefon bulunamadi (mDNS)" >&2; exit 1; }

path="/status"
mode="table"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --json) mode="json"; shift ;;
        --rotate) path="/rotate?deg=${2:-0}"; shift 2 ;;
        *) echo "bilinmeyen secenek: $1" >&2; exit 1 ;;
    esac
done

json=$(curl -fsS --max-time 5 "http://${host}:${STATUS_PORT}${path}" 2>/dev/null) || {
    echo "durum ucuna ulasilamadi: http://${host}:${STATUS_PORT}" >&2
    echo "telefonda akis calisiyor mu?" >&2
    exit 1
}

if [[ "$mode" == "json" ]]; then
    echo "$json"
    exit 0
fi

python3 - <<PY
import json, sys
d = json.loads('''$json''')
rows = [
    ("telefon",        "$host"),
    ("yakalama",       d.get("resolution")),
    ("gonderilen kare",d.get("frame")),
    ("kare hizi",      f"{d.get('fps')} fps"),
    ("bit hizi",       f"{d.get('bitrate', 0) // 1_000_000} Mbit"),
    ("kamera",         d.get("camera")),
    ("sensor acisi",   d.get("sensorOrientation")),
    ("goruntu donusu", d.get("imageRotation")),
    ("otomatik donus", d.get("autoRotate")),
    ("telefon yonu",   d.get("deviceOrientation")),
    ("onizleme",       d.get("preview")),
    ("kadraj modu",    d.get("frameMode")),
    ("uygulanan donus",d.get("appliedRotation")),
    ("kadraj",         "DAR (kenarlar siyah)" if d.get("narrow") else "tam kare"),
    ("pozlama kilidi", d.get("exposureLocked")),
    ("kamera->GL",     f"{d.get('cameraFrames')} kare"),
    ("GL->kodlayici",  f"{d.get('glDraws')} kare"),
    ("kodlayici cikti",f"{d.get('encoderOutputs')} kare"),
    ("gonderilen",     f"{d.get('framesSent')} kare"),
    ("dusen",          d.get("framesDropped")),
    ("atlanan",        d.get("framesSkipped")),
    ("bagli PC",       d.get("client")),
]
width = max(len(k) for k, _ in rows)
for k, v in rows:
    print(f"{k.ljust(width)} : {v}")
PY
