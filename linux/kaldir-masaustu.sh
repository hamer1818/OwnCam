#!/usr/bin/env bash
# `kur-masaustu.sh`in yazdigi her seyi geri alir. Ayarlar ve arka plan
# fotograflari kullanicinin kendi dosyalari; onlara dokunulmuyor.
set -euo pipefail

bin_dizin="$HOME/.local/bin"
uygulama_dizin="$HOME/.local/share/applications"
simge_dizin="$HOME/.local/share/icons/hicolor"

rm -fv "$bin_dizin/owncam" "$uygulama_dizin/owncam.desktop" \
       "$simge_dizin/scalable/apps/owncam.svg"
for n in 16 24 32 48 64 128 256; do
    rm -fv "$simge_dizin/${n}x${n}/apps/owncam.png"
done

command -v update-desktop-database >/dev/null 2>&1 &&
    update-desktop-database "$uygulama_dizin" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null 2>&1 &&
    gtk-update-icon-cache -f -t "$simge_dizin" 2>/dev/null || true

echo "OwnCam masaustu girdisi kaldirildi."
