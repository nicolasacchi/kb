#!/usr/bin/env bash
# Self-test for scripts/check-doc-anchors.sh (`check-doc-anchors.sh --self-test`,
# run by `just ci-selfcheck`). Builds a scratch git repo with a copy of the
# gate and proves the classes it exists for, including the one the old
# bounds-only gate could not see: the RIGHT file, the WRONG line.
set -uo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
fail=0

new_repo() { # new_repo <weak-ceiling>
  rm -rf "$work/r"; mkdir -p "$work/r/scripts" "$work/r/docs/research" "$work/r/src" "$work/r/pad"
  cp "$here/check-doc-anchors.sh" "$work/r/scripts/"
  sed -i "s/^WEAK_CEILING=.*/WEAK_CEILING=$1/" "$work/r/scripts/check-doc-anchors.sh"
  # the gate refuses a tree with <100 files (a broken walk would pass vacuously)
  for i in $(seq 1 120); do echo x > "$work/r/pad/f$i.txt"; done
  { for i in $(seq 1 19); do echo "// filler $i"; done; echo "pub fn target_fn() {}"; for i in $(seq 1 10); do echo "// tail $i"; done; } > "$work/r/src/x.rs"
  ( cd "$work/r" && git init -q . && git config user.email t@example.invalid && git config user.name t )
}
gate() { ( cd "$work/r" && export KB_ANCHOR_SRC_PATHS="src/*.rs" && git add -A >/dev/null 2>&1 && bash scripts/check-doc-anchors.sh "$@" 2>&1 ); }
expect() { # expect <rc> <label> <needle|-> <gate args...>
  local want="$1" label="$2" needle="$3"; shift 3
  local out rc
  out="$(gate "$@")"; rc=$?
  if [ "$rc" -ne "$want" ]; then echo "SELFTEST FAIL: $label: want exit $want, got $rc" >&2; echo "$out" | tail -15 >&2; fail=1; return; fi
  if [ "$needle" != "-" ] && ! grep -q -- "$needle" <<<"$out"; then echo "SELFTEST FAIL: $label: output lacks '$needle'" >&2; echo "$out" | tail -15 >&2; fail=1; return; fi
  echo "ok: $label (exit $rc)"
}

# 1. a correct symbol-paired anchor passes, and is counted as verified
new_repo 0
echo 'The entry point `target_fn` (`src/x.rs:20`) is here.' > "$work/r/docs/a.md"
expect 0 "paired anchor on the right line" "paired OK     1"

# 2. RIGHT FILE, WRONG LINE: in range, one file, passes every bounds check --
#    only the symbol pairing can see it.
echo 'The entry point `target_fn` (`src/x.rs:3`) is here.' > "$work/r/docs/a.md"
expect 1 "right file, wrong line is caught" "SYMBOL"
# 2b. ...and --fix re-points it, after which the gate is green
gate --fix >/dev/null
grep -q 'src/x.rs:20' "$work/r/docs/a.md" || { echo "SELFTEST FAIL: --fix did not re-point to :20 ($(cat "$work/r/docs/a.md"))" >&2; fail=1; }
expect 0 "green after --fix" "paired OK     1"

# 3. an unpaired anchor is WEAK: fatal above the ceiling, fine at it
echo 'Somewhere around `src/x.rs:20` there is a function.' > "$work/r/docs/a.md"
expect 1 "unpaired anchor exceeds a ceiling of 0" "WEAK"
new_repo 1
echo 'Somewhere around `src/x.rs:20` there is a function.' > "$work/r/docs/a.md"
expect 0 "unpaired anchor at a ceiling of 1" "ceiling 1;"

# 3b. ...and a count BELOW the ceiling fails too (the ratchet is exact; the
#     constant must be lowered in the same PR)
new_repo 3
echo 'Somewhere around `src/x.rs:20` there is a function.' > "$work/r/docs/a.md"
expect 1 "WEAK below the ceiling fails until it is lowered" "below the ceiling 3"

# 4. a symbol that exists nowhere in the file
new_repo 0
echo 'It is `no_such_symbol` (`src/x.rs:20`).' > "$work/r/docs/a.md"
expect 1 "symbol absent from the file" "does not occur anywhere"

# 5. dangling / past-EOF still fatal
echo 'Gone: `src/missing.rs:3`.' > "$work/r/docs/a.md"
expect 1 "dangling path" "DANGLING"
echo 'Far: `src/x.rs:9999`.' > "$work/r/docs/a.md"
expect 1 "past EOF" "PAST-EOF"

# 6. a CLAUDE.md is in scope (it used to be ungated)
new_repo 5
echo 'Gone: `src/missing.rs:3`.' > "$work/r/CLAUDE.md"
echo 'clean' > "$work/r/docs/a.md"
expect 1 "CLAUDE.md anchors are gated" "DANGLING"

# 7. docs/research/** is a dated record: never scanned, never rewritten
new_repo 0
echo 'Stale: `target_fn` (`src/x.rs:3`).' > "$work/r/docs/research/old.md"
echo 'clean' > "$work/r/docs/a.md"
expect 0 "docs/research is out of scope" "OK"
gate --fix >/dev/null
grep -q 'src/x.rs:3' "$work/r/docs/research/old.md" || { echo "SELFTEST FAIL: --fix rewrote docs/research" >&2; fail=1; }

# 8. source comments: a symbol-paired citation that drifted is caught; an
#    unpaired example (`src/lib.rs:42`) is ignored
new_repo 0
echo 'clean' > "$work/r/docs/a.md"
printf '// see `target_fn` (`src/x.rs:2`) for the entry point\nfn other() {}\n' > "$work/r/src/user_code.rs"
expect 1 "stale paired citation in a source comment" "SYMBOL"
printf '// e.g. `src/lib.rs:42` is just an example\nfn other() {}\n' > "$work/r/src/user_code.rs"
expect 0 "unpaired example in a source comment is ignored" "OK"

exit "$fail"
