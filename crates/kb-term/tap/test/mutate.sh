#!/bin/bash
# T3 mutation check: each mutant re-breaks exactly one rule; the suite must fail.
# The temp copy is the whole tap directory with layout preserved (src/, test/,
# package.json; out/ excluded), so relative imports (../src, ../../fixtures)
# resolve. `mut()` takes a target-file argument (src/strip.ts or src/decode.ts);
# that file is restored from the pristine copy before each mutant. Every anchor
# is guarded by `assert a in s`, so a stale anchor fails loudly instead of being
# vacuous. Mutants 1-11 are the brief's required eleven; 12-14 are the twin-site
# guards (SNIFF ESC+not-ST, lone-raw C1, abort()-CAN) so a single-site break
# cannot hide behind a rule proven at only one of its sites.
set -uo pipefail
SRC="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
mkdir -p "$TMP/mp"
cp -r "$SRC" "$TMP/mp/tap"
rm -rf "$TMP/mp/tap/out"   # spec 8: exclude out/ (metrics/junit from the ci.sh run)
cp -r "$SRC/../fixtures" "$TMP/mp/fixtures"
# pristine copies of the mutable production files
cp "$SRC/src/strip.ts" "$TMP/strip.ts.orig"
cp "$SRC/src/decode.ts" "$TMP/decode.ts.orig"
cd "$TMP/mp/tap"
# Preflight (spec 8): --bail must be accepted by this bun. `bun test --bail`
# changes the summary format, so kill detection below uses the exit code
# (plus a fail-count excerpt for the message), not the old anchored grep.
# If bun rejects the flag, fail loudly with the raw output as evidence.
if ! bun test --bail --timeout 60000 test/decode.test.ts > "$TMP/bail-sanity.log" 2>&1; then
  echo "mutate.sh: bun rejected --bail (spec 8 flag unsupported); raw output follows"
  cat "$TMP/bail-sanity.log"
  exit 1
fi

mut() {
  local target="$1" a="$2" b="$3"
  cp "$TMP/$(basename "$target").orig" "$TMP/mp/tap/$target"
  if ! python3 -I -c "
import sys
s=open('$target').read(); a,b=sys.argv[1],sys.argv[2]
assert a in s, 'stale anchor: '+a[:80]
open('$target','w').write(s.replace(a,b,1))" "$a" "$b"; then
    echo "mutate.sh: stale anchor for mutant $((n+1)) in $target; aborting" >&2
    exit 1
  fi
  out=$(bun test --bail --timeout 300000 test/fuzz.test.ts test/strip.test.ts test/differential.test.ts test/decode.test.ts 2>&1)
  ec=$?
  n=$((n+1))
  if [ "$ec" -ne 0 ]; then
    fails=$(printf '%s\n' "$out" | grep -E '[0-9]+ fail' | tail -1 | tr -d ' ')
    echo "killed (${fails:-fail, exit=$ec}) mutant $n: ${a:0:80}"
  else
    echo "SURVIVED mutant $n: ${a:0:80}"
    printf '%s\n' "$out" | tail -n 25
    surv=$((surv+1))
  fi
}
n=0; surv=0

# 1. ESC state: an incoming ESC forwards the held bytes instead of dropping them (splice bug)
mut src/strip.ts "        if (c === ESC) { this.aborted++; this.restartEsc(); return; }
        if (c === CAN || c === SUB) { this.aborted++; this.toG(); this.out.push(c); return; }
        if (c < 0x20 || c === 0x7f) { this.out.push(c); return; }
        this.buf.push(c);
        if (c >= 0x40 && c <= 0x7e) {" "        if (c === ESC) { this.emit(this.buf); this.aborted++; this.restartEsc(); return; }
        if (c === CAN || c === SUB) { this.aborted++; this.toG(); this.out.push(c); return; }
        if (c < 0x20 || c === 0x7f) { this.out.push(c); return; }
        this.buf.push(c);
        if (c >= 0x40 && c <= 0x7e) {"
# 2. DECREQTPARM classification disabled
mut src/strip.ts "case 'x': return pre === '' && (p0 === 0 || p0 === 1) ? 'DECREQTPARM' : null;" "case 'x': return null;"
# 3. OSC/SNIFF ESC+not-ST forwards the partial instead of dropping it
mut src/strip.ts "          if (c === BS) { this.buf.push(ESC); this.buf.push(c); this.finishOsc(); return; }
          this.aborted++; this.restartEsc(); this.byte(c); return;" "          if (c === BS) { this.buf.push(ESC); this.buf.push(c); this.finishOsc(); return; }
          this.emit(this.buf); this.restartEsc(); this.byte(c); return;"
# 4. BOUNDARY ignored by the Stripper (deleted bytes can splice)
mut src/strip.ts "if (c === BOUNDARY) { this.abort(); return; }" "if (c === BOUNDARY) { return; }"
# 5. C1 deletion no longer emits BOUNDARY (the C1 delete splices)
mut src/strip.ts "      if (c >= 0x80 && c <= 0x9f) { this.out.push(BOUNDARY); return; }  // U+0080..U+009F deleted" "      if (c >= 0x80 && c <= 0x9f) { return; }"
# 6. n final skips classification (DSR/CPR no longer stripped)
mut src/strip.ts "        if (c >= 0x40 && c <= 0x7e) {
          const q = csiQueryGet" "        if (c >= 0x40 && c <= 0x7e && c !== 0x6e) {
          const q = csiQueryGet"
# 7. PASS-state CAN on abort removed
mut src/strip.ts "this.pendEsc = false; this.out.push(CAN); this.aborted++; this.restartEsc(); this.byte(c); return;" "this.pendEsc = false; this.aborted++; this.restartEsc(); this.byte(c); return;"
# 8. Fast path also skips bytes >= 0x80 (C1/UTF-8 escapes the fast path)
mut src/strip.ts "if (chunk[j] >= 0x80) { h = j; break; }" "if (chunk[j] >= 0x80) { h = chunk.length; break; }"
# 9. Fast path runs while a stage is not idle (ignores pending())
mut src/strip.ts "      if (!this.isIdle()) {" "      if (false) {"
# 10. Decoder treats a line inside a %begin..%end block as a notification
mut src/decode.ts "    if (this.block) {" "    if (false) {"
# 11. Octal decoder drops the third digit of an escape (wrong byte for \134 etc.)
mut src/decode.ts "      const val = ((v[i + 1] - 0x30) << 6) | ((v[i + 2] - 0x30) << 3) | (v[i + 3] - 0x30);" "      const val = ((v[i + 1] - 0x30) << 6) | ((v[i + 2] - 0x30) << 3);"
# 12. SNIFF twin of 3: ESC+not-ST forwards the partial instead of dropping it
mut src/strip.ts "          if (c === BS) { this.buf.push(ESC); this.buf.push(c); this.endSniff(true); return; }
          this.aborted++; this.restartEsc(); this.byte(c); return;" "          if (c === BS) { this.buf.push(ESC); this.buf.push(c); this.endSniff(true); return; }
          this.emit(this.buf); this.restartEsc(); this.byte(c); return;"
# 13. Lone-raw C1 twin of 5: deletion no longer emits BOUNDARY (the C1 delete splices)
mut src/strip.ts "    if (c <= 0x9f) { this.out.push(BOUNDARY); return; }                 // lone raw C1 byte" "    if (c <= 0x9f) { return; }"
# 14. abort() twin of 7: a forwarded string aborted mid-way gets no CAN
mut src/strip.ts "    if (this.st === 'PASS') this.out.push(CAN);" "    if (this.st === 'PASS') { /* CAN removed */ }"

echo "mutants=$n survivors=$surv"
[ "$n" = 14 ] && [ "$surv" = 0 ]
