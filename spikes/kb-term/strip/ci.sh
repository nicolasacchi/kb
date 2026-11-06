#!/usr/bin/env bash
# KBT-STRIP: runs from the repo root (the workflow job 'strip'). Zero dependencies: bun + python3 only.
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
out="$here/out"
rm -rf "$out"; mkdir -p "$out"
cd "$here"
{ bun --version; python3 --version; } > "$out/versions.txt" 2>&1
python3 -I "$here/../gen/fake_harness.py" --seed 1 --length 20 | sha256sum > "$out/harness-sha256.txt"
set +e
bun test --timeout 300000 2>&1 | tee "$out/bun-test.txt"
rc=${PIPESTATUS[0]}
set -e
python3 -I - "$out" "$rc" <<'PY'
import json, sys, glob, os
out, rc = sys.argv[1], int(sys.argv[2])
m = {}
for f in sorted(glob.glob(os.path.join(out, 'metrics-*.json'))):
    m.update(json.load(open(f)))
txt = open(os.path.join(out, 'bun-test.txt')).read()
lines = ['kb-term STRIP summary', f'bun test exit code: {rc}']
for l in txt.splitlines():
    if l.strip().endswith(' pass') or l.strip().endswith(' fail') or l.startswith('Ran '):
        lines.append(l.strip())
lines.append('')
lines.append(json.dumps(m, indent=1, sort_keys=True))
open(os.path.join(out, 'summary.txt'), 'w').write('\n'.join(lines) + '\n')
print('\n'.join(lines))
PY
if [ "$rc" = 0 ]; then
  bash "$here/mutate.sh" 2>&1 | tee "$out/mutation.txt"
  mrc=${PIPESTATUS[0]}
  { echo; echo "mutation check (each mutant re-breaks one rule; the suite must fail):"; cat "$out/mutation.txt"; } >> "$out/summary.txt"
  exit "$mrc"
fi
exit "$rc"
