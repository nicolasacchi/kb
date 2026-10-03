#!/usr/bin/env bash
# Self-test for scripts/ci/code-changed.sh. Compile-free; run by `just
# ci-selfcheck` (supply-chain job).
#
# The load-bearing case is the 200 KB name list: the match is on the FIRST
# line, so the old `printf list | grep -qE` form (grep exits at once, the
# writer is still blocked on a full 64 KB pipe and dies of SIGPIPE, 141, which
# `pipefail` turns into "no match") reports SKIP, while the script must say
# RUN. The old form is exercised too, but only as a printed note -- whether it
# really misfires is a kernel-pipe property, the script's verdict is not.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
sut="$here/code-changed.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
fail=0

verdict() { GITHUB_OUTPUT= "$sut" --names-file "$1"; }
expect() { # expect <run=true|run=false> <label> <names-file>
  local got
  got="$(verdict "$3")"
  if [ "$got" != "$1" ]; then
    echo "SELFTEST FAIL: $2: want $1, got $got" >&2
    fail=1
  else
    echo "ok: $2 ($got)"
  fi
}

# 1. big list, match first.
{ echo "crates/kb-core/src/lib.rs"; for i in $(seq 1 4000); do printf 'docs/research/padding-%06d-aaaaaaaaaaaaaaaaaaaaaaaaaaaa.md\n' "$i"; done; } > "$work/big-first"
size="$(wc -c < "$work/big-first")"
[ "$size" -gt 200000 ] || { echo "fixture too small ($size)" >&2; exit 1; }
expect run=true "200 KB list, relevant path first" "$work/big-first"
# The old form, with a writer that is still mid-stream when grep -q exits
# (the first line is flushed, the rest follows after a pause): the writer is
# killed by SIGPIPE and pipefail turns that into "no match" -> SKIP.
old=run
if ( set -o pipefail; { head -n 1 "$work/big-first"; sleep 0.3; tail -n +2 "$work/big-first"; } | grep -qE '^crates/kb-core/' ) 2>/dev/null; then :; else old=skip; fi
if [ "$old" != skip ]; then
  echo "SELFTEST FAIL: the reproduction no longer reproduces the SIGPIPE skip; fixture is stale" >&2
  fail=1
else
  echo "ok: old 'writer | grep -q' form wrongly says skip on the same list (the bug this script removes)"
fi

# 2. big list, no relevant path -> skip.
grep -v '^crates/' "$work/big-first" > "$work/big-none"
expect run=false "200 KB list, nothing relevant" "$work/big-none"

# 3. every declared prefix / exact path triggers; a near miss does not.
while IFS= read -r p; do
  printf '%s\n' "${p}x" > "$work/one"
  case "$p" in */) expect run=true "prefix $p" "$work/one" ;; esac
done < <(sed -n '/^PREFIXES=(/,/^)/p' "$sut" | grep -o '"[^"]*"' | tr -d '"')
while IFS= read -r p; do
  printf '%s\n' "$p" > "$work/one"; expect run=true "exact $p" "$work/one"
  printf '%s\n' "${p}.bak" > "$work/one"; expect run=false "near miss ${p}.bak" "$work/one"
done < <(sed -n '/^EXACT=(/,/^)/p' "$sut" | grep -o '"[^"]*"' | tr -d '"')
# the CI machinery the code lanes depend on (F1 carry): named, so dropping one fails here
for p in ci/test-floors.toml scripts/ci/witness.py .config/nextest.toml scripts/ci/selfcheck.py; do
  printf '%s\n' "$p" > "$work/one"; expect run=true "ci machinery $p" "$work/one"
done
printf 'README.md\ndocs/foo.md\n' > "$work/one"; expect run=false "docs-only change" "$work/one"

# 4. no usable base -> run=true (never silently skip).
got="$(cd "$work" && git init -q . && GITHUB_OUTPUT= BASE_SHA=deadbeefdeadbeefdeadbeefdeadbeefdeadbeef HEAD_SHA=HEAD "$sut")"
[ "$got" = run=true ] && echo "ok: unresolvable base runs ($got)" || { echo "SELFTEST FAIL: unresolvable base gave $got" >&2; fail=1; }

# 5. the skip message names every prefix and exact path.
msg="$("$sut" --skip-message)"
while IFS= read -r p; do
  case "$msg" in *"$p"*) ;; *) echo "SELFTEST FAIL: skip message omits $p" >&2; fail=1 ;; esac
done < <(sed -n '/^\(PREFIXES\|EXACT\)=(/,/^)/p' "$sut" | grep -o '"[^"]*"' | tr -d '"')

exit "$fail"
