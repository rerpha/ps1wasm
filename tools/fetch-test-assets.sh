#!/bin/bash
# Downloads freely-licensed test assets into test-assets/ (optional: the
# tests that need them skip without). OpenBIOS is bundled in web/static.
#  - JaCzekanski/ps1-tests prebuilt EXEs + real-hardware reference logs
#  - PeterLemon/PSX test programs + expected screenshots
set -e
cd "$(dirname "$0")/.."
mkdir -p test-assets && cd test-assets
tmp=$(mktemp -d)
if [ ! -d ps1-tests ]; then
  curl -sSL -o "$tmp/tests.zip" https://github.com/JaCzekanski/ps1-tests/releases/download/build-158/tests.zip
  mkdir ps1-tests && (cd ps1-tests && unzip -q "$tmp/tests.zip")
  echo "ps1-tests ok"
fi
# PeterLemon/PSX bare-metal tests (expected screenshots are emulator captures)
if [ ! -d peterlemon ]; then
  mkdir peterlemon && cd peterlemon
  curl -sSL "https://api.github.com/repos/PeterLemon/PSX/git/trees/master?recursive=1" \
    | grep -o '"path": *"[^"]*\.\(exe\|png\)"' | sed 's/"path": *"//; s/"$//' > list.txt
  while read -r f; do mkdir -p "$(dirname "$f")"; curl -sSL -o "$f" "https://raw.githubusercontent.com/PeterLemon/PSX/master/$f"; done < list.txt
  cd ..
  echo "peterlemon ok"
fi
rm -rf "$tmp"
