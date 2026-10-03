#!/usr/bin/env bash
# claims.sh -- list the NORMATIVE claims a diff ADDS (comments / docs / test
# names), each with file:line and whether a resolvable pin sits on the same
# line or the next three. Deterministic grep only: the judgement (is the pin
# actually sufficient?) is the agent's, in the SKILL.md workflow.
#
# Usage: claims.sh [<base-ref> [<head-ref>]]     (default: origin/main..HEAD)
#        claims.sh --self-test
#
# Output, one row per claim:  STATUS  file:line  token  | the added line
#   pinned    a `pinned by `x`` (any capitalisation), `invariant:N`, a backticked
#             `file.rs::fn` (the ::fn is REQUIRED -- a bare file mention such as
#             `foo.rs` names where the code lives, not what proves the claim),
#             or a backticked snake_case identifier that is a real `fn` in the tree
#   UNPINNED  none of the above within the line + next 3 added lines
set -uo pipefail

# Does this text carry a pin reference? (file-only mentions deliberately do not.)
PIN_RE='[Pp][Ii][Nn][Nn][Ee][Dd][[:space:]]+[Bb][Yy] `|invariant:[0-9]+|`[A-Za-z0-9_./-]+\.(rs|ts|tsx)::[A-Za-z0-9_:]+`'
has_pin_ref() { printf '%s' "$1" | grep -E "$PIN_RE" >/dev/null; }

if [ "${1:-}" = "--self-test" ]; then
  fail=0
  t() { has_pin_ref "$2"; local got=$?; [ "$got" -eq "$1" ] || { echo "SELFTEST FAIL: has_pin_ref($2) = $got, want $1" >&2; fail=1; }; }
  t 1 'This NEVER happens; see `crates/kb-core/src/lib.rs` for the code.'
  t 1 'This NEVER happens (`lib.rs`).'
  t 0 'This NEVER happens (`lib.rs::the_test`).'
  t 0 'This NEVER happens (Pinned by `x`).'
  t 0 'This NEVER happens, invariant:7.'
  [ "$fail" -eq 0 ] && echo "claims self-test ok"
  exit "$fail"
fi

base="${1:-origin/main}"
head="${2:-HEAD}"
cd "$(git rev-parse --show-toplevel)"

TOKENS='NEVER|ALWAYS|REFUSES|REFUSED|fail(s)? closed|fails CLOSED|pinned by|structurally|byte-identical|cannot race|lossless|exactly once|at most once|atomic(ally)?|guaranteed?'

tracked="$(mktemp)"; trap 'rm -f "$tracked"' EXIT
git ls-files > "$tracked"

is_fn() { git grep -qE "fn[[:space:]]+$1([^A-Za-z0-9_]|\$)" -- '*.rs' 2>/dev/null; }

git diff -U0 --no-color "$base...$head" -- ':!docs/research' ':!*.lock' ':!*package-lock.json' \
| awk -v toks="$TOKENS" '
    /^\+\+\+ b\// { file = substr($0, 7); next }
    /^@@/ { match($0, /\+[0-9]+/); line = substr($0, RSTART + 1, RLENGTH - 1) + 0; next }
    /^\+/ && !/^\+\+\+/ {
      text = substr($0, 2)
      print file "\t" line "\t" text
      line++
    }' > "$tracked.added"

total=0; unpinned=0
mapfile -t rows < "$tracked.added"
for i in "${!rows[@]}"; do
  IFS=$'\t' read -r file line text <<<"${rows[$i]}"
  # comment / doc lines only (code is judged by its tests, not its prose)
  case "$file" in
    *.md|*.html) ;;
    *) printf '%s' "$text" | grep -E '^[[:space:]]*(//|///|//!|\*|#|<!--)' >/dev/null || continue ;;
  esac
  tok="$(printf '%s' "$text" | grep -oE "$TOKENS" | head -1)"
  [ -z "$tok" ] && continue
  total=$((total + 1))
  window="$text"
  for j in 1 2 3; do
    n=$((i + j))
    [ "$n" -lt "${#rows[@]}" ] || break
    IFS=$'\t' read -r f2 l2 t2 <<<"${rows[$n]}"
    [ "$f2" = "$file" ] || break
    window="$window $t2"
  done
  status=UNPINNED
  if has_pin_ref "$window"; then
    status=pinned
  else
    while IFS= read -r id; do
      id="${id//\`/}"
      if [[ "$id" =~ ^[a-z][a-z0-9]*(_[a-z0-9]+)+$ ]] && is_fn "$id"; then status=pinned; break; fi
    done < <(printf '%s' "$window" | grep -oE '`[a-z][a-z0-9_]*`')
  fi
  [ "$status" = UNPINNED ] && unpinned=$((unpinned + 1))
  printf '%-8s %s:%s  [%s]  | %s\n' "$status" "$file" "$line" "$tok" "$(printf '%s' "$text" | sed -E 's/^[[:space:]]+//' | cut -c1-110)"
done
rm -f "$tracked.added"
echo "claims: $total normative claim(s) added, $unpinned unpinned (base $base)"
