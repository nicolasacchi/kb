#!/bin/bash
SRC="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d)"
rm -rf "$TMP/mp"; mkdir -p $TMP/mp/strip; cp -r $SRC/gen $TMP/mp/; cp $SRC/strip/*.ts $TMP/mp/strip/; cd $TMP/mp/strip
mut(){ cp $SRC/strip/strip.ts strip.ts; python3 -I -c "
import sys
s=open('strip.ts').read(); a,b=sys.argv[1],sys.argv[2]
assert a in s, a; open('strip.ts','w').write(s.replace(a,b,1))" "$1" "$2" || return; r=$(bun test --timeout 300000 fuzz.test.ts strip.test.ts 2>&1 | grep -E "^ *[0-9]+ fail" | tr -d ' '); n=$((n+1)); case "$r" in 0fail|"") echo "SURVIVED mutant $n: ${1:0:60}"; surv=$((surv+1));; *) echo "killed ($r) mutant $n: ${1:0:60}";; esac; }
n=0; surv=0
mut "        if (c === ESC) { this.aborted++; this.restartEsc(); return; }
        if (c === CAN || c === SUB) { this.aborted++; this.toG(); this.out.push(c); return; }
        if (c < 0x20 || c === 0x7f) { this.out.push(c); return; }
        this.buf.push(c);" "        if (c === ESC) { this.emit(this.buf); this.aborted++; this.restartEsc(); return; }
        if (c === CAN || c === SUB) { this.aborted++; this.toG(); this.out.push(c); return; }
        if (c < 0x20 || c === 0x7f) { this.out.push(c); return; }
        this.buf.push(c);"
mut "case 'x': return pre === '' && (p0 === 0 || p0 === 1) ? 'DECREQTPARM' : null;" "case 'x': return null;"
mut "this.aborted++; this.restartEsc(); this.byte(c); return;          // ESC + not-ST: partial dropped, new sequence" "this.emit(this.buf); this.restartEsc(); this.byte(c); return;"
mut "if (c === BOUNDARY) { this.abort(); return; }" "if (c === BOUNDARY) { return; }"
mut "      if (c >= 0x80 && c <= 0x9f) { this.out.push(BOUNDARY); return; }  // U+0080..U+009F deleted" "      if (c >= 0x80 && c <= 0x9f) { return; }"
mut "if (c >= 0x40 && c <= 0x7e) {
          const q = csiQuery" "if (c >= 0x40 && c <= 0x7e && c !== 0x6e) {
          const q = csiQuery"
mut "this.out.push(CAN); this.aborted++; this.restartEsc(); this.byte(c); return;" "this.aborted++; this.restartEsc(); this.byte(c); return;"

echo "mutants=$n survivors=$surv"
[ "$surv" = 0 ]
