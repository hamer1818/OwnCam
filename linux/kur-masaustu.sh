#!/usr/bin/env bash
# OwnCam masaustu uygulamasini kullanicinin kendi dizinine kurar.
#
# sudo yok, sistem dizinlerine dokunmaz: her sey ~/.local altinda. Kaldirmak
# icin `kaldir-masaustu.sh`.
set -euo pipefail

kaynak_dizin="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Betik hem surum arsivinden (ikili betigin yaninda) hem de depo icinden
# (linux/ altinda, ikili desktop/target'ta) calisabiliyor.
for aday in "$kaynak_dizin/owncam" \
            "$kaynak_dizin/../owncam" \
            "$kaynak_dizin/../desktop/target/release/owncam"; do
    [ -x "$aday" ] && ikili="$aday" && break
done
if [ -z "${ikili:-}" ]; then
    echo "hata: owncam ikilisi bulunamadi." >&2
    echo "  Surum arsivinden kuruyorsan bu betigi arsivin icinden calistir." >&2
    echo "  Depodan kuruyorsan once: cd desktop && cargo build --release" >&2
    exit 1
fi

bin_dizin="$HOME/.local/bin"
uygulama_dizin="$HOME/.local/share/applications"
simge_dizin="$HOME/.local/share/icons/hicolor"

install -Dm755 "$ikili" "$bin_dizin/owncam"

# Simge: olceklenebilir SVG ana kaynak. Bazi masaustleri (ozellikle eski
# tepsi/gorev cubugu gerceklemeleri) SVG okumadigi icin birkac sabit boyut da
# yaziliyor - varsa rsvg-convert ile, yoksa yalnizca SVG kaliyor.
install -Dm644 "$kaynak_dizin/owncam.svg" "$simge_dizin/scalable/apps/owncam.svg"
if command -v rsvg-convert >/dev/null 2>&1; then
    for n in 16 24 32 48 64 128 256; do
        hedef="$simge_dizin/${n}x${n}/apps/owncam.png"
        mkdir -p "$(dirname "$hedef")"
        rsvg-convert -w "$n" -h "$n" "$kaynak_dizin/owncam.svg" -o "$hedef"
    done
fi

# Masaustu girdisi. `Exec` mutlak yola yaziliyor: menuden calistirilan
# uygulamalar oturumun PATH'ini kullaniyor ve ~/.local/bin cogu dagitimda
# orada olmuyor - "menude gorunuyor ama tiklayinca acilmiyor" tam olarak bu.
mkdir -p "$uygulama_dizin"
sed "s|^Exec=owncam$|Exec=$bin_dizin/owncam|" \
    "$kaynak_dizin/owncam.desktop" > "$uygulama_dizin/owncam.desktop"
chmod 644 "$uygulama_dizin/owncam.desktop"

# Onbellekleri tazele; olmayan araclar sessizce atlaniyor.
command -v update-desktop-database >/dev/null 2>&1 &&
    update-desktop-database "$uygulama_dizin" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null 2>&1 &&
    gtk-update-icon-cache -f -t "$simge_dizin" 2>/dev/null || true

echo "Kuruldu:"
echo "  $bin_dizin/owncam"
echo "  $uygulama_dizin/owncam.desktop"
echo "  $simge_dizin/scalable/apps/owncam.svg"
echo
echo "Uygulama menusunde 'OwnCam' olarak gorunuyor; arama kutusuna"
echo "'webcam', 'kamera' ya da 'telefon' yazinca da cikiyor."

case ":$PATH:" in
    *":$bin_dizin:"*) ;;
    *)
        echo
        echo "Not: $bin_dizin PATH'te degil. Menuden calismasi icin sorun yok"
        echo "(mutlak yol yazildi) ama terminalden 'owncam' diye cagirmak icin"
        echo "kabuk yapilandirmana ekle."
        ;;
esac

# v4l2loopback olmadan uygulama acilir ama yazacak cihaz bulamaz. Kurulumun
# en cok atlanan adimi bu, o yuzden burada bir kez daha soyleniyor.
if [ ! -e /dev/video11 ] && ! v4l2-ctl --list-devices 2>/dev/null | grep -qi owncam; then
    echo
    echo "UYARI: sanal kamera cihazi (/dev/video11) yok."
    echo "v4l2loopback kurulumu icin KURULUM.md bolum 2'ye bak."
fi
