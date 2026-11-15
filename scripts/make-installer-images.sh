#!/usr/bin/env bash
# Regenerates the Windows installer's header and sidebar bitmaps from src-tauri/icons/source.svg.
# Needs ImageMagick (`convert`). NSIS wants 24-bit BMPs without alpha: 150x57 and 164x314.
set -euo pipefail
cd "$(dirname "$0")/.."
svg=src-tauri/icons/source.svg
out=src-tauri/installer
mkdir -p "$out"
cream='#ece5d6' dark='#1c1d21' terracotta='#d9663a'

# Header: cream strip, the icon on the right (NSIS draws the page title on the left of it).
convert -background none -density 384 "$svg" -resize 49x49 /tmp/rupantar-icon-small.png
convert -size 150x57 "xc:$cream" /tmp/rupantar-icon-small.png -gravity east -geometry +6+0 -composite \
  -type TrueColor BMP3:"$out/header.bmp"

# Sidebar: dark panel, big icon, name and tagline.
convert -background none -density 384 "$svg" -resize 132x132 /tmp/rupantar-icon-big.png
convert -size 164x314 "xc:$dark" /tmp/rupantar-icon-big.png -gravity north -geometry +0+56 -composite \
  -font DejaVu-Sans-Bold -pointsize 22 -fill "$cream" -gravity north -annotate +0+200 'Rupantar' \
  -font DejaVu-Sans -pointsize 10 -fill "$terracotta" -gravity north -annotate +0+232 'flick to convert' \
  -type TrueColor BMP3:"$out/sidebar.bmp"
rm -f /tmp/rupantar-icon-small.png /tmp/rupantar-icon-big.png
