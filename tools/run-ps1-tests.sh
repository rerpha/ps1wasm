#!/bin/bash
# Run JaCzekanski/ps1-tests that have real-hardware logs (psx.log) under
# OpenBIOS. PASS = every reference line appears in our output and we print
# no "fail" lines. (Reference logs may come from older test builds.)
cd "$(dirname "$0")/.."
T=test-assets/ps1-tests
BIOS=test-assets/openbios.bin
pass=0; fail=0
for ref in $(find $T -name psx.log | sort); do
  dir=$(dirname $ref); exe=$(ls $dir/*.exe 2>/dev/null | head -1)
  [ -z "$exe" ] && continue
  name=${dir#$T/}
  sed 's/^% \{0,1\}//; s/\r$//; s/[[:space:]]*$//' $ref | sed '/^$/d' > /tmp/ref.txt
  timeout 300 ./target/release/ps1wasm-run --bios $BIOS --exe $exe --frames ${FRAMES:-900} 2>/dev/null \
    | sed 's/\r$//; s/[[:space:]]*$//' | sed '/^$/d' > /tmp/out.txt
  missing=$(grep -vxFf /tmp/out.txt /tmp/ref.txt | wc -l)
  fails=$(grep -c '^fail' /tmp/out.txt)
  total=$(wc -l < /tmp/ref.txt)
  if [ "$missing" = "0" ] && [ "$fails" = "0" ]; then echo "PASS $name"; pass=$((pass+1));
  else echo "FAIL $name ($missing/$total reference lines missing, $fails fail lines)"; fail=$((fail+1));
    [ -n "$VERBOSE" ] && { grep -vxFf /tmp/out.txt /tmp/ref.txt | head -${VERBOSE}; grep '^fail' /tmp/out.txt | head -${VERBOSE}; }
  fi
done
echo "passed $pass, failed $fail"
