#!/usr/bin/env bash
# Regenerates assets/readme-banner.png from tools/banner-render.html using
# headless Chrome. ImageMagick cannot rasterize it: CSS gradients, an inline
# SVG waveform and the monospace stack all have to render.
#
#   ./tools/render-banner.sh
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$(pwd)"
CHROME="${CHROME:-/Applications/Google Chrome.app/Contents/MacOS/Google Chrome}"

# 2x so it stays crisp on retina.
"$CHROME" --headless --disable-gpu --no-sandbox --hide-scrollbars \
  --force-device-scale-factor=2 --window-size=1280,440 \
  --virtual-time-budget=12000 --screenshot="$ROOT/assets/readme-banner.png" \
  "file://$ROOT/tools/banner-render.html" >/dev/null 2>&1

printf 'readme-banner.png  %s\n' "$(sips -g pixelWidth -g pixelHeight "$ROOT/assets/readme-banner.png" | tail -2 | tr -d ' \n')"
