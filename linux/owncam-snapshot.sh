#!/usr/bin/env bash
#
# Sanal kameradan tek kare yakala. Yon/kadraj sorunlarini goze bakarak
# dogrulamak icin - "ters mi duz mu, dar mi genis mi" sorusunun cevabi.
#
#   owncam-snapshot.sh              # /tmp/owncam-kare.png
#   owncam-snapshot.sh /yol/a.png
set -euo pipefail

OUT="${1:-/tmp/owncam-kare.png}"
DEVICE="${OWNCAM_DEVICE:-/dev/video11}"

if [[ ! -r "$DEVICE" ]]; then
    echo "$DEVICE okunamiyor" >&2
    exit 1
fi

ffmpeg -hide_banner -loglevel error -f v4l2 -i "$DEVICE" -frames:v 1 -y "$OUT"

size=$(ffprobe -v error -select_streams v:0 -show_entries stream=width,height \
       -of csv=p=0:s=x "$OUT" 2>/dev/null || echo "?")
echo "$OUT  ($size)"
