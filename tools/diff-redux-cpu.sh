#!/bin/bash
# Differential CPU fuzzing: random programs run on ps1wasm-run and PCSX-Redux
# (interpreter, same OpenBIOS); register/memory dumps must be identical.
# Usage: tools/diff-redux-cpu.sh [FIRST_SEED] [COUNT] [OPS]
cd "$(dirname "$0")/.."
REDUX=${REDUX:-/tmp/redux/squashfs-root/AppRun}
first=${1:-1}; count=${2:-20}; ops=${3:-400}
pass=0; fail=0
for s in $(seq $first $((first+count-1))); do
  ./target/release/ps1wasm-fuzzexe $s $ops /tmp/fz.exe
  ./target/release/ps1wasm-run --exe /tmp/fz.exe --frames 300 2>/dev/null \
    | grep -E '^([0-9A-F]{8}|END)$' > /tmp/fz_ours.txt
  timeout 20 $REDUX -cli -bios web/static/openbios/openbios.bin -exe /tmp/fz.exe -run -interpreter -softgpu -noupdate 2>/dev/null | sed '/^END$/q' \
    | grep -E '^([0-9A-F]{8}|END)$' > /tmp/fz_redux.txt
  if [ "$(tail -1 /tmp/fz_redux.txt)" != "END" ]; then echo "seed $s: redux produced no complete output"; continue; fi
  if cmp -s /tmp/fz_ours.txt /tmp/fz_redux.txt; then pass=$((pass+1));
  else fail=$((fail+1)); echo "seed $s: DIFFERENT"; diff /tmp/fz_ours.txt /tmp/fz_redux.txt | head -6; fi
done
echo "identical: $pass, different: $fail"
