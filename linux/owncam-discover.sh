#!/usr/bin/env bash
# Telefonu yerel agda mDNS ile bul.
#
# Android tarafi kendini `_owncam._tcp` olarak duyuruyor (MdnsAdvertiser.kt).
# Basarili olursa "IP PORT" yazar, aksi halde bos cikti ve 1 doner.
set -euo pipefail

TIMEOUT="${OWNCAM_DISCOVER_TIMEOUT:-5}"

if ! command -v avahi-browse >/dev/null 2>&1; then
    echo "avahi-browse yok (paket: avahi)" >&2
    exit 1
fi

# -r cozumle, -t bulunca cik, -p ayristirilabilir cikti, -k gereksiz alan yok
# Cikti bicimi: =;iface;IPv4;isim;_owncam._tcp;local;host;ADRES;PORT;txt
line=$(timeout "$TIMEOUT" avahi-browse -rtpk _owncam._tcp 2>/dev/null \
       | awk -F';' '$1=="=" && $3=="IPv4" {print $8, $9; exit}') || true

if [[ -z "${line:-}" ]]; then
    exit 1
fi

echo "$line"
