#!/usr/bin/env bash
# Public-repo leak gate.
#
# Greps the WHOLE tracked tree (root files and scripts/ included) for a private
# extended-regex that names things which must never reach this public repo.
#
# The pattern list is itself private (it names private projects), so it is NOT
# in the repository. It arrives in the environment:
#
#   PUBLIC_GATE_PATTERNS   an extended regex (grep -E syntax)
#   PUBLIC_GATE_REQUIRED   "1" => an empty/unset pattern is a FAILURE (push to
#                          main); anything else => skip with a notice (fork PRs
#                          and Dependabot PRs cannot read repository secrets).
#
# Output is file:line ONLY. Neither the pattern nor the matched text is ever
# printed, so the log cannot become the leak it exists to prevent. The shell
# never runs with xtrace, and git's own stderr is discarded (an invalid regex
# would otherwise be echoed back).
#
# Paths in scripts/ci/public-gate.allow (one pathspec per line, `#` comments)
# are excluded: generated or vendored blobs whose bytes match by accident
# (base64 images, lockfile integrity hashes). Keep it to blobs, never to prose.
#
# Usage (local): PUBLIC_GATE_PATTERNS='<regex>' scripts/ci/public-gate.sh
set -uo pipefail
set +x

pattern="${PUBLIC_GATE_PATTERNS-}"
required="${PUBLIC_GATE_REQUIRED-0}"

if [ -z "$pattern" ]; then
  if [ "$required" = "1" ]; then
    echo "::error::public gate: PUBLIC_GATE_PATTERNS is empty, and this run requires it (push to main). Set the repository secret."
    exit 1
  fi
  echo "::notice::public gate skipped: PUBLIC_GATE_PATTERNS is not available to this run (fork or Dependabot PR). It is enforced on push to main."
  exit 0
fi

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$here/../.."

excludes=()
if [ -f "$here/public-gate.allow" ]; then
  while IFS= read -r line; do
    line="${line%%#*}"
    line="$(echo "$line" | tr -d '[:space:]')"
    [ -n "$line" ] && excludes+=(":(exclude)$line")
  done < "$here/public-gate.allow"
fi

hits_file="$(mktemp)"
trap 'rm -f "$hits_file"' EXIT

git grep -nIE -e "$pattern" -- . "${excludes[@]}" 2>/dev/null | cut -d: -f1,2 > "$hits_file"
rc=${PIPESTATUS[0]}

if [ "$rc" -gt 1 ]; then
  echo "::error::public gate: git grep failed (exit $rc) - most likely the pattern is not a valid extended regex. Nothing was checked."
  exit 2
fi

if [ -s "$hits_file" ]; then
  n="$(wc -l < "$hits_file" | tr -d ' ')"
  echo "::error::public gate: $n line(s) match the private pattern list. Locations (file:line):"
  while IFS= read -r loc; do
    echo "  $loc"
  done < "$hits_file"
  exit 1
fi

echo "public gate: clean (whole tracked tree, ${#excludes[@]} allow-listed path(s))."
