#!/usr/bin/env bash
# Downloads the prebuilt pdfium library (bblanchon/pdfium-binaries) for a Rust target triple into
# src-tauri/resources/pdfium/, which tauri.conf.json bundles as the `pdfium/` resource folder.
#
#   scripts/fetch-pdfium.sh                          # host triple (from `rustc -vV`)
#   scripts/fetch-pdfium.sh aarch64-apple-darwin
#
# The release is pinned to the pdfium API version pdfium-render is built against (`pdfium_7881`,
# its `pdfium_latest` in 0.9.4); bump both together. Downloads are sha256-verified.
# Needs: bash, curl, tar, sha256sum or shasum.
set -euo pipefail

PDFIUM_RELEASE=7881

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="$ROOT/src-tauri/resources/pdfium"

TRIPLE="${1:-}"
if [[ -z "$TRIPLE" ]]; then
  TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
fi
[[ -n "$TRIPLE" ]] || { echo "error: no target triple given and rustc not found" >&2; exit 1; }

# Asset, digest (from the GitHub releases API "digest" field) and library file name.
asset() {
  case "$1" in
    x86_64-unknown-linux-gnu)
      echo "pdfium-linux-x64.tgz 1470e21b8b4a3b4ad7f85684e2da11d94f3b69a86d81dee11b9b6709d927ac1d libpdfium.so" ;;
    x86_64-unknown-linux-musl)
      echo "pdfium-linux-musl-x64.tgz 8a8cdfc6c79865269f74bf0e9ca476cdd8a9908ea7e0b0abcfc1525d59fb7257 libpdfium.so" ;;
    aarch64-unknown-linux-gnu)
      echo "pdfium-linux-arm64.tgz ee7f7b7d5468958336a818c1cd580bdd20972846b7377b13f9a923d92d1d4674 libpdfium.so" ;;
    aarch64-unknown-linux-musl)
      echo "pdfium-linux-musl-arm64.tgz 4c6e678e820c390c3ffc1f720a25386e3b637aa42ecf99446672146d793dac46 libpdfium.so" ;;
    x86_64-apple-darwin)
      echo "pdfium-mac-x64.tgz 6dedf83990e0e3d6b7c93c9e7589c5a126b0ae14b7464d76120cff7a26afb18b libpdfium.dylib" ;;
    aarch64-apple-darwin)
      echo "pdfium-mac-arm64.tgz 52e94ca5aa8847934330daf3f8150c190682c5ca93831468794f8b90d4392e40 libpdfium.dylib" ;;
    universal-apple-darwin)
      echo "pdfium-mac-univ.tgz df451a413c3609585e84a4a91110a9bc889cff05fe3b2db0ed817c9e90c3f7d3 libpdfium.dylib" ;;
    x86_64-pc-windows-msvc)
      echo "pdfium-win-x64.tgz 73cc0de638ac2095e7445bf56a38200a5b7c7ca0e9f4ba144598f2457377ac08 pdfium.dll" ;;
    aarch64-pc-windows-msvc)
      echo "pdfium-win-arm64.tgz d3035d4d2cacac6ecd1a2ece197a3d702a1b2a58466276b9f870b8cb278a9d84 pdfium.dll" ;;
    i686-pc-windows-msvc)
      echo "pdfium-win-x86.tgz cf02980c66a93eef007cfff861fe79d291f3c3dcd43258abf6e3099b682e7ec6 pdfium.dll" ;;
    *) return 1 ;;
  esac
}

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

read -r ASSET SHA LIB < <(asset "$TRIPLE") || { echo "error: no pdfium build for $TRIPLE" >&2; exit 1; }
URL="https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F$PDFIUM_RELEASE/$ASSET"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "==> pdfium: $URL"
curl -fL --retry 3 --progress-bar -o "$WORK/$ASSET" "$URL"
GOT="$(sha256 "$WORK/$ASSET")"
if [[ "$GOT" != "$SHA" ]]; then
  echo "error: checksum mismatch for $ASSET: expected $SHA, got $GOT" >&2
  exit 1
fi
mkdir -p "$WORK/x"
tar -xzf "$WORK/$ASSET" -C "$WORK/x"
BIN="$(find "$WORK/x" -type f -name "$LIB" | head -n 1)"
[[ -n "$BIN" ]] || { echo "error: $LIB not found in $ASSET" >&2; exit 1; }

# One platform's library at a time; the license files must ship with it (BSD-3 + third parties).
rm -rf "$DEST"
mkdir -p "$DEST"
install -m 644 "$BIN" "$DEST/$LIB"
[[ -f "$WORK/x/LICENSE" ]] && install -m 644 "$WORK/x/LICENSE" "$DEST/LICENSE"
[[ -d "$WORK/x/licenses" ]] && cp -R "$WORK/x/licenses" "$DEST/licenses"
echo "    -> src-tauri/resources/pdfium/$LIB"
echo "Done. pdfium chromium/$PDFIUM_RELEASE is ready for $TRIPLE."
