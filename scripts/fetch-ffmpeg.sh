#!/bin/bash
# Downloads the static ffmpeg + ffprobe that ship inside Clipper.app (Apple Silicon), pinned
# and checksum-verified, into vendor/ffmpeg/. Builds by Martin Riedl (ffmpeg.martin-riedl.de),
# FFmpeg 9.0.2, GPLv3 (see assets/licenses/FFmpeg.txt).
set -euo pipefail
cd "$(dirname "$0")/.."

BASE="https://ffmpeg.martin-riedl.de/download/macos/arm64/1789931890_9.0.2"
declare -a NAMES=(ffmpeg ffprobe)
declare -a SUMS=(
  c8ed4c4e6978a03c485edbfe4e0a5dc2380f8a30bba5150531b31b094492d924
  fcbe839537485eaee7a7a8bc5cbc0f90d53617e80943e8a5b2e31cb851197ea6
)

mkdir -p vendor/ffmpeg
TMP=$(mktemp -d)
for i in 0 1; do
  name=${NAMES[$i]}
  if [[ -x "vendor/ffmpeg/$name" ]]; then
    continue
  fi
  echo "Downloading ${name}..."
  curl -fsSL "$BASE/$name.zip" -o "$TMP/$name.zip"
  got=$(shasum -a 256 "$TMP/$name.zip" | cut -d' ' -f1)
  if [[ "$got" != "${SUMS[$i]}" ]]; then
    echo "Checksum mismatch for $name.zip ($got)" >&2
    exit 1
  fi
  unzip -q -o "$TMP/$name.zip" -d "$TMP/$name"
  cp "$TMP/$name/$name" "vendor/ffmpeg/$name"
  chmod +x "vendor/ffmpeg/$name"
done
rm -rf "$TMP"
vendor/ffmpeg/ffmpeg -hide_banner -version | head -1
