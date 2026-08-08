#!/usr/bin/env bash
#
# Plan bolum 9 olcumu: kare hizi, ortalama aralik, jitter, en uzun bosluk.
# Faz 0 cikis kriteri: fps >= 25 ve jitter < 15 ms.
#
# Kullanim: owncam-measure.sh [saniye] [cihaz]
set -euo pipefail

DURATION="${1:-12}"
DEVICE="${2:-${OWNCAM_DEVICE:-/dev/video11}}"
PTS_FILE="$(mktemp -t owncam-pts.XXXXXX)"
trap 'rm -f "$PTS_FILE"' EXIT

echo "[owncam] $DEVICE uzerinden ${DURATION}s olculuyor..." >&2

# Cozunurluk de plandaki basari metriklerinden biri; olcumle birlikte raporla.
FMT=""
if command -v v4l2-ctl >/dev/null 2>&1; then
    FMT=$(v4l2-ctl -d "$DEVICE" --get-fmt-video 2>/dev/null \
          | awk -F': *' '/Width\/Height/ {print $2}' | tr -d ' ')
fi

ffmpeg -hide_banner -f v4l2 -i "$DEVICE" -t "$DURATION" -vf showinfo -f null - 2>&1 \
  | grep -oE "pts_time:[0-9.]+" | sed 's/pts_time://' > "$PTS_FILE"

if [[ ! -s "$PTS_FILE" ]]; then
    echo "[owncam] hic kare gelmedi. Alici calisiyor mu?" >&2
    exit 1
fi

python3 - "$PTS_FILE" "${FMT:-?}" <<'PY'
import sys

t = [float(x) for x in open(sys.argv[1]) if x.strip()]
fmt = sys.argv[2].replace("/", "x")
if len(t) < 3:
    print("yeterli kare yok:", len(t))
    sys.exit(1)

d = [(b - a) * 1000 for a, b in zip(t, t[1:])]
n = len(d)
mean = sum(d) / n
var = sum((x - mean) ** 2 for x in d) / n
jitter = var ** 0.5
fps = len(t) / (t[-1] - t[0])
gaps = sum(1 for x in d if x > 100)

print(f"cozunurluk: {fmt}")
print(f"kare      : {len(t)}")
print(f"fps       : {fps:.1f}      (hedef >= 25)")
print(f"ort aralik: {mean:.0f} ms")
print(f"jitter    : {jitter:.0f} ms     (hedef < 15)")
print(f"en uzun   : {max(d):.0f} ms")
print(f">100ms    : {gaps}          (hedef 0)")
print()
ok = fps >= 25 and jitter < 15 and gaps == 0
print("FAZ 0 CIKIS KRITERI:", "GECTI" if ok else "TUTMADI")
PY
