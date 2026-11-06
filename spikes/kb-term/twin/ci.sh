#!/usr/bin/env bash
# T1 twin spike CI driver. Runs from repo root; npm install already done at spikes/kb-term.
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
HERE="$ROOT/spikes/kb-term/twin"
GEN="$ROOT/spikes/kb-term/gen"
OUT="$HERE/out"
rm -rf "$OUT"; mkdir -p "$OUT"
STREAMS="$(mktemp -d)"
cd "$ROOT/spikes/kb-term"
node --version | tee "$OUT/versions.txt"
npm ls --depth=0 2>&1 | tee -a "$OUT/versions.txt" || true

# synthetic streams (python3 -I, stdlib only); name.COLSxROWS.bin
for spec in "1 100 30 60" "2 100 30 60" "3 100 30 120" "4 80 24 60" "5 200 50 60" "6 40 12 60" "7 100 30 300"; do
  set -- $spec
  python3 -I "$GEN/fake_harness.py" --seed "$1" --cols "$2" --rows "$3" --length "$4" -o "$STREAMS/harness-s$1.$2x$3.bin"
done
python3 -I "$GEN/synth_omp_kitty_stream.py" > "$STREAMS/omp-kitty.100x30.bin"
python3 -I "$GEN/synth_omp_kitty_stream.py" --cols 60 --rows 12 --resize-replay > "$STREAMS/omp-kitty-replay.100x30.bin"
if ls "$ROOT"/spikes/kb-term/fixtures/*.bin >/dev/null 2>&1; then
  for f in "$ROOT"/spikes/kb-term/fixtures/*.bin; do b="$(basename "$f" .bin)"; case "$b" in *x[0-9]*) cp "$f" "$STREAMS/$b.bin";; *) cp "$f" "$STREAMS/fx-$b.100x30.bin";; esac; done
fi
ls -l "$STREAMS" | tee "$OUT/streams.txt"
( cd "$STREAMS" && sha256sum *.bin ) > "$OUT/streams.sha256"

rc=0
node "$HERE/widths.mjs" "$HERE/glyphs.json" "$OUT" 2>&1 | tee "$OUT/widths.log" || rc=1
node "$HERE/run-matrix.mjs" "$STREAMS" "$OUT" 100 2>&1 | tee "$OUT/matrix.log" || rc=1
node "$HERE/report.mjs" "$OUT" 2>&1 | tee "$OUT/report.log" || rc=1
cp "$HERE"/widths-tmux-3.7c.json "$HERE"/widths-bun.json "$OUT"/
echo "ci.sh finished rc=$rc"
[ -s "$OUT/report.md" ] || rc=1
exit $rc
