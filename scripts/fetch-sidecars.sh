#!/usr/bin/env bash
# Downloads the pandoc and typst release binaries for a Rust target triple and installs them as
# Tauri sidecars: src-tauri/binaries/<tool>-<triple>[.exe] (the names `bundle.externalBin` expects).
#
#   scripts/fetch-sidecars.sh                          # host triple (from `rustc -vV`)
#   scripts/fetch-sidecars.sh aarch64-apple-darwin
#
# Versions and sha256 digests are pinned below; every download is verified before it is installed.
# Needs: bash, curl, tar (with xz support), unzip (or a bsdtar that reads zip), sha256sum or shasum.
set -euo pipefail

PANDOC_VERSION=3.12
TYPST_VERSION=0.15.1

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="$ROOT/src-tauri/binaries"

TRIPLE="${1:-}"
if [[ -z "$TRIPLE" ]]; then
  TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
fi
[[ -n "$TRIPLE" ]] || { echo "error: no target triple given and rustc not found" >&2; exit 1; }

# Asset names and digests, from the GitHub releases API ("digest" field of each asset).
pandoc_asset() {
  local v=$PANDOC_VERSION
  case "$1" in
    x86_64-unknown-linux-gnu|x86_64-unknown-linux-musl)
      echo "pandoc-$v-linux-amd64.tar.gz 67d7d011fed8c8543306022b985b9b2499ab9b74818df91d8727c7e9ebc5ba06" ;;
    aarch64-unknown-linux-gnu|aarch64-unknown-linux-musl)
      echo "pandoc-$v-linux-arm64.tar.gz 6cefcf7100e23a99447c26f89d1ff5b253f3407fcef99a9e27ae06f3ed16cb82" ;;
    x86_64-apple-darwin)
      echo "pandoc-$v-x86_64-macOS.zip 18577f9460c3dc5d2651ad3bab37d513bc2034a5a777fbe18fa0a5acf2e936ea" ;;
    aarch64-apple-darwin)
      echo "pandoc-$v-arm64-macOS.zip f148ca09c9f36594db527a9fc988ad736290ce428f79594c50208cd1ec58b3c0" ;;
    x86_64-pc-windows-msvc|aarch64-pc-windows-msvc)
      # pandoc ships no Windows ARM build; the x64 one runs under Windows' emulation.
      echo "pandoc-$v-windows-x86_64.zip 2a77ebc2517d13e95056e76b1cd5b574cfe958ac61aa6058117d80c22ca19b79" ;;
    *) return 1 ;;
  esac
}

typst_asset() {
  case "$1" in
    x86_64-unknown-linux-gnu|x86_64-unknown-linux-musl)
      echo "typst-x86_64-unknown-linux-musl.tar.xz a6d077d0a95eed5a2eba715b2dae06be954f624ccbf85758a03f389ded33118c" ;;
    aarch64-unknown-linux-gnu|aarch64-unknown-linux-musl)
      echo "typst-aarch64-unknown-linux-musl.tar.xz 5aa8d74a3d906e60ea12a66ac2f37f8eef1b14cbad7182a745e393a10c23dcee" ;;
    x86_64-apple-darwin)
      echo "typst-x86_64-apple-darwin.tar.xz 7f9fdd9584866245de9a79e0add8f9236fae6f40a8a45e2c4771ccc14db4e0fa" ;;
    aarch64-apple-darwin)
      echo "typst-aarch64-apple-darwin.tar.xz 48f62ed034aa3a7978309579ac6ca00045e2ef0da73114e8af27cfd8e74dc05a" ;;
    x86_64-pc-windows-msvc)
      echo "typst-x86_64-pc-windows-msvc.zip 19ce3551153c2fe7ee9fa2f95208310c8f4d3209fedb699e0333faf8913f6736" ;;
    aarch64-pc-windows-msvc)
      echo "typst-aarch64-pc-windows-msvc.zip 4ab28e1b71ec3184d38d580ab797f499b6770d952b6b19167be5cea5c2662e14" ;;
    *) return 1 ;;
  esac
}

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

extract() { # archive dir
  case "$1" in
    *.zip) if command -v unzip >/dev/null; then unzip -q "$1" -d "$2"; else tar -xf "$1" -C "$2"; fi ;;
    *) tar -xf "$1" -C "$2" ;;
  esac
}

EXE=""
[[ "$TRIPLE" == *windows* ]] && EXE=".exe"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$DEST"

install_tool() { # tool url sha256
  local tool=$1 url=$2 want=$3
  local archive="$WORK/${url##*/}"
  echo "==> $tool: $url"
  curl -fL --retry 3 --progress-bar -o "$archive" "$url"
  local got
  got="$(sha256 "$archive")"
  if [[ "$got" != "$want" ]]; then
    echo "error: checksum mismatch for ${url##*/}: expected $want, got $got" >&2
    exit 1
  fi
  local out="$WORK/$tool"
  mkdir -p "$out"
  extract "$archive" "$out"
  local bin
  bin="$(find "$out" -type f -name "$tool$EXE" | head -n 1)"
  [[ -n "$bin" ]] || { echo "error: $tool$EXE not found in ${url##*/}" >&2; exit 1; }
  install -m 755 "$bin" "$DEST/$tool-$TRIPLE$EXE"
  echo "    -> src-tauri/binaries/$tool-$TRIPLE$EXE"
}

read -r P_ASSET P_SHA < <(pandoc_asset "$TRIPLE") || { echo "error: no pandoc build for $TRIPLE" >&2; exit 1; }
read -r T_ASSET T_SHA < <(typst_asset "$TRIPLE") || { echo "error: no typst build for $TRIPLE" >&2; exit 1; }

install_tool pandoc "https://github.com/jgm/pandoc/releases/download/$PANDOC_VERSION/$P_ASSET" "$P_SHA"
install_tool typst "https://github.com/typst/typst/releases/download/v$TYPST_VERSION/$T_ASSET" "$T_SHA"

echo "Done. pandoc $PANDOC_VERSION and typst $TYPST_VERSION are ready for $TRIPLE."
