#!/bin/bash
# Compare full VRAM against real-hardware dumps from ps1-tests.
# Writes our VRAM to /tmp/vram-tests/<name>.png for inspection.
cd "$(dirname "$0")/.."
T=test-assets/ps1-tests
mkdir -p /tmp/vram-tests
for ref in $(find $T -name "vram*.png" | sort); do
  dir=$(dirname $ref); base=$(basename $ref .png); suffix=${base#vram}
  exe=$(ls $dir/*${suffix}.exe 2>/dev/null | grep -v dma | head -1)
  [ -z "$exe" ] && exe=$(ls $dir/*.exe | head -1)
  name="${dir#$T/}$suffix"
  out=/tmp/vram-tests/$(echo $name | tr / _).png
  res=$(timeout 300 ./target/release/ps1wasm-run --bios test-assets/openbios.bin --exe $exe --frames ${FRAMES:-300} \
        --vram $out --compare-vram $ref 2>/dev/null | grep '^\[compare\]')
  printf "%-28s %s\n" "$name" "${res#\[compare\] }"
done
