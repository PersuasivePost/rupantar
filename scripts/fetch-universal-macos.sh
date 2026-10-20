#!/usr/bin/env bash
# Prepares everything a universal macOS build (`tauri build --target universal-apple-darwin`)
# needs: fetches pandoc and typst for both architectures and joins them with lipo into
# src-tauri/binaries/<tool>-universal-apple-darwin, then fetches pdfium for both architectures
# and lipos the two libraries into one in src-tauri/resources/pdfium/.
#
#   scripts/fetch-universal-macos.sh
#
# Needs macOS (lipo) and what fetch-sidecars.sh / fetch-pdfium.sh need.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/src-tauri/binaries"
PDFIUM="$ROOT/src-tauri/resources/pdfium"
ARM=aarch64-apple-darwin
X64=x86_64-apple-darwin

command -v lipo >/dev/null || { echo "error: lipo not found (this script needs macOS)" >&2; exit 1; }

"$ROOT/scripts/fetch-sidecars.sh" "$ARM"
"$ROOT/scripts/fetch-sidecars.sh" "$X64"
for tool in pandoc typst; do
  lipo -create -output "$BIN/$tool-universal-apple-darwin" "$BIN/$tool-$ARM" "$BIN/$tool-$X64"
  chmod 755 "$BIN/$tool-universal-apple-darwin"
  echo "    -> src-tauri/binaries/$tool-universal-apple-darwin"
done

# fetch-pdfium.sh keeps one platform's library at a time, so stash the first one.
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
"$ROOT/scripts/fetch-pdfium.sh" "$ARM"
cp "$PDFIUM/libpdfium.dylib" "$WORK/arm64.dylib"
"$ROOT/scripts/fetch-pdfium.sh" "$X64"   # leaves libpdfium.dylib plus the license files
lipo -create -output "$PDFIUM/libpdfium.dylib" "$WORK/arm64.dylib" "$PDFIUM/libpdfium.dylib"
echo "    -> src-tauri/resources/pdfium/libpdfium.dylib (universal)"
echo "Done. Sidecars and pdfium are ready for universal-apple-darwin."
