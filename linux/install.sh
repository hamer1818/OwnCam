#!/usr/bin/env bash
#
# Alici scriptlerini ~/.local/bin'e, systemd kullanici servisini
# ~/.config/systemd/user'a kurar. sudo gerekmez.
set -euo pipefail

SRC="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
BIN="$HOME/.local/bin"
UNIT="$HOME/.config/systemd/user"

mkdir -p "$BIN" "$UNIT"

for f in owncam-receive.sh owncam-discover.sh owncam-measure.sh \
         owncam-status.sh owncam-snapshot.sh owncam-calibrate.sh \
         owncam-desktop.py; do
    install -m 755 "$SRC/$f" "$BIN/$f"
    echo "kuruldu: $BIN/$f"
done

install -m 644 "$SRC/owncam.service" "$UNIT/owncam.service"
echo "kuruldu: $UNIT/owncam.service"

systemctl --user daemon-reload

cat <<'EOF'

Kurulum tamam.

Elle calistirmak icin:
    owncam-receive.sh                # telefonu mDNS ile bulur
    owncam-receive.sh 192.168.1.42   # IP elle

Servis olarak:
    systemctl --user start owncam
    systemctl --user enable owncam   # acilista otomatik
    journalctl --user -u owncam -f   # gunluk

Goruntu yan geliyorsa donusu bir kez olcup kaydet:
    owncam-calibrate.sh

Olcum (plan bolum 9):
    owncam-measure.sh 12

~/.local/bin PATH'te degilse fish icin:
    fish_add_path ~/.local/bin
EOF
