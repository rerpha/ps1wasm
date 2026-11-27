#!/bin/bash
# Downloads freely-licensed test assets into test-assets/:
#  - OpenBIOS (MIT) extracted from the PCSX-Redux Linux AppImage
#  - JaCzekanski/ps1-tests prebuilt EXEs + real-hardware reference logs
set -e
cd "$(dirname "$0")/.."
mkdir -p test-assets && cd test-assets
tmp=$(mktemp -d)
if [ ! -f openbios.bin ]; then
  path=$(curl -sSL https://distrib.app/storage/manifests/pcsx-redux/dev-linux-x64/manifest.json \
    | grep -o '"id": *[0-9]*' | head -1 | grep -o '[0-9]*')
  url=$(curl -sSL "https://distrib.app/storage/manifests/pcsx-redux/dev-linux-x64/manifest-$path.json" \
    | grep -o '"path": *"[^"]*zip"' | head -1 | sed 's/.*"\(\/storage[^"]*\)"/\1/')
  curl -sSL -o "$tmp/redux.zip" "https://distrib.app$url"
  (cd "$tmp" && unzip -q redux.zip && chmod +x ./*.AppImage && ./*.AppImage --appimage-extract >/dev/null)
  cp "$tmp/squashfs-root/usr/share/pcsx-redux/resources/openbios.bin" .
  echo "openbios.bin ok"
fi
if [ ! -d ps1-tests ]; then
  curl -sSL -o "$tmp/tests.zip" https://github.com/JaCzekanski/ps1-tests/releases/download/build-158/tests.zip
  mkdir ps1-tests && (cd ps1-tests && unzip -q "$tmp/tests.zip")
  echo "ps1-tests ok"
fi
rm -rf "$tmp"
# PeterLemon/PSX bare-metal tests (expected screenshots are emulator captures)
if [ ! -d test-assets/peterlemon ]; then
  cd "$(dirname "$0")/.."
  mkdir -p test-assets/peterlemon && cd test-assets/peterlemon
  curl -sSL "https://api.github.com/repos/PeterLemon/PSX/git/trees/master?recursive=1" \
    | grep -o '"path": *"[^"]*\.\(exe\|png\)"' | sed 's/"path": *"//; s/"$//' > list.txt
  while read f; do mkdir -p "$(dirname "$f")"; curl -sSL -o "$f" "https://raw.githubusercontent.com/PeterLemon/PSX/master/$f"; done < list.txt
  echo "peterlemon ok"
fi
