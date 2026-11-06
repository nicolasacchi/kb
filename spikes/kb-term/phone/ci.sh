#!/usr/bin/env bash
# Runs from the repo root after `npm install` at spikes/kb-term.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
KT="$(cd "$HERE/.." && pwd)"
OUT="$HERE/out"
mkdir -p "$OUT/shots" "$HERE/data"
python3 -I "$HERE/mkdata.py" "$HERE/data" | tee "$OUT/data.txt" || exit 1
node --check "$HERE/server.mjs" || exit 1
cd "$KT"
npx playwright test -c "$HERE/playwright.config.mjs" 2>&1 | tee "$OUT/playwright.log"
rc=${PIPESTATUS[0]}
node "$HERE/report.mjs" || echo "report generation failed"
echo "playwright exit code: $rc"
exit "$rc"
