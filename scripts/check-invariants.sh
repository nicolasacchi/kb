#!/usr/bin/env bash
# check-invariants.sh — GC-C2: invariant-to-test traceability coverage table.
#
# The 35 numbered CLAUDE.md invariant slots (see ../CLAUDE.md and
# docs/architecture-invariants.md) are pinned by `// invariant:N <hint>`
# comments dropped directly above the test fn (Rust `#[test]`/`#[tokio::test]`)
# or test title (Playwright `test(...)` / vitest `it(...)`) that would fail if
# invariant N were violated. This script greps the whole tree for
# `invariant:N` markers (N in 1..35), prints a coverage table, and calls out
# any N with zero hits as UNPINNED. RETIRED slots (kept numbered so `#N`
# cross-references never shift, but carrying no active invariant) are listed
# in RETIRED below and excluded from the coverage denominator.
#
# Non-blocking by design: this is a signal for humans/agents deciding which
# invariant to re-check before touching a subsystem, not a merge gate — a
# genuinely unpinnable invariant (e.g. one only enforceable by Cargo's own
# dependency resolution) is a legitimate, permanent UNPINNED entry, not a bug.
# Always exits 0.
#
# Usage: scripts/check-invariants.sh [--quiet]
#   --quiet   suppress the per-invariant table, print only the summary +
#             UNPINNED list (handy for a CI step's log noise budget)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

QUIET=0
for arg in "$@"; do
  case "$arg" in
    --quiet) QUIET=1 ;;
    *) echo "unknown arg: $arg" >&2 ;;
  esac
done

TOTAL=35   # numbered slots — the CLAUDE.md cap

# Retired slots: number preserved, no active invariant, open for the next
# newcomer per the 35-cap rule. Currently empty — no slot is retired; this
# populates only if a future invariant genuinely retires one.
RETIRED=()

is_retired() {
  local n="$1" r
  for r in "${RETIRED[@]}"; do
    if [ "$r" -eq "$n" ]; then return 0; fi
  done
  return 1
}

# Grep every source tree that can carry a pin: Rust crates + web/ + tests/e2e.
# Exclude build output / deps so a stray match in a vendored file or the
# target dir never inflates the count.
grep_invariant() {
  local n="$1"
  grep -rn "invariant:${n}\b" \
    --include='*.rs' --include='*.ts' --include='*.tsx' \
    crates web tests/e2e 2>/dev/null
}

declare -a UNPINNED=()
PINNED_COUNT=0

if [ "$QUIET" -eq 0 ]; then
  printf '%-4s %-8s %s\n' "N" "hits" "first pin"
  printf '%-4s %-8s %s\n' "----" "----" "---------"
fi

for n in $(seq 1 "$TOTAL"); do
  if is_retired "$n"; then
    if [ "$QUIET" -eq 0 ]; then
      printf '%-4s %-8s %s\n' "$n" "-" "RETIRED (slot open)"
    fi
    continue
  fi
  hits="$(grep_invariant "$n")"
  count=0
  first=""
  if [ -n "$hits" ]; then
    count=$(printf '%s\n' "$hits" | wc -l | tr -d ' ')
    first=$(printf '%s\n' "$hits" | head -1 | cut -d: -f1-2)
  fi

  if [ "$count" -eq 0 ]; then
    UNPINNED+=("$n")
    if [ "$QUIET" -eq 0 ]; then
      printf '%-4s %-8s %s\n' "$n" "0" "UNPINNED"
    fi
  else
    PINNED_COUNT=$((PINNED_COUNT + 1))
    if [ "$QUIET" -eq 0 ]; then
      printf '%-4s %-8s %s\n' "$n" "$count" "$first"
    fi
  fi
done

ACTIVE=$((TOTAL - ${#RETIRED[@]}))

echo
echo "invariant-test coverage: ${PINNED_COUNT}/${ACTIVE} pinned (${#RETIRED[@]} retired slot(s): ${RETIRED[*]})"

if [ "${#UNPINNED[@]}" -gt 0 ]; then
  echo
  echo "UNPINNED (${#UNPINNED[@]}): ${UNPINNED[*]}"
  echo "  — see docs/invariant-test-map.md for what a pin would look like for each."
else
  echo "all active invariants have at least one pin."
fi

# Always non-blocking — this is a coverage signal, not a merge gate.
exit 0
