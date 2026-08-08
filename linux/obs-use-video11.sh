#!/usr/bin/env bash
#
# OBS sahnesindeki v4l2 kaynagini /dev/video0 (DroidCam) yerine
# /dev/video11 (OwnCam) yapar.
#
# OBS KAPALI olmali - acikken sahne dosyasini uzerine yazar.
set -euo pipefail

SCENE_DIR="$HOME/.config/obs-studio/basic/scenes"
TARGET="${OWNCAM_DEVICE:-/dev/video11}"

if pgrep -x obs >/dev/null 2>&1; then
    echo "OBS calisiyor. Once kapat, sonra tekrar calistir." >&2
    exit 1
fi

shopt -s nullglob
changed=0
for scene in "$SCENE_DIR"/*.json; do
    if ! grep -q '"device_id": "/dev/video[0-9]' "$scene"; then
        continue
    fi
    cp -a "$scene" "$scene.owncam-oncesi"
    # Sadece gercek video cihazlarini degistir; "default" (ses) dokunulmaz.
    sed -i -E "s|(\"device_id\": )\"/dev/video[0-9]+\"|\1\"$TARGET\"|g" "$scene"
    echo "guncellendi: $scene  ->  $TARGET  (yedek: $scene.owncam-oncesi)"
    changed=1
done

if [[ $changed -eq 0 ]]; then
    echo "degistirilecek v4l2 kaynagi bulunamadi." >&2
    exit 1
fi
