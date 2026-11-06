#!/usr/bin/env bash
# T2 renderer spike. Runs from the repo root (workflow job 'renderer', after npm install in spikes/kb-term).
set -u
here="$(cd "$(dirname "$0")" && pwd)"
kb="$(cd "$here/.." && pwd)"
rm -rf "$here/out"; mkdir -p "$here/out/screenshots"
cd "$kb"
node --version
npx playwright --version
npx playwright test -c "$here/playwright.config.mjs" 2>&1 | tee "$here/out/playwright.log"
rc=${PIPESTATUS[0]}
node "$here/report.mjs" || echo "report.mjs failed"
echo "playwright exit code: $rc"
exit "$rc"
