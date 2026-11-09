#!/usr/bin/env sh
# Render og.html to app/assets/og-image.png (1200x630) with headless Chrome.
# Fonts load from Google Fonts, so run it online. Set CHROME to your Chrome or Chromium binary if needed.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
chrome=${CHROME:-"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"}
"$chrome" --headless=new --disable-gpu --hide-scrollbars --window-size=1200,630 \
  --virtual-time-budget=4000 --screenshot="$here/../assets/og-image.png" "file://$here/og.html"
