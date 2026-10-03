#!/usr/bin/env bash
# check-ts-shadows.sh — the "no shadow of a generated wire type" gate (I3).
#
# A hand-written `export interface X` / `export type X` in an SPA's src/ may
# not share a name with `src/api/generated/X.ts` (the ts-rs output for the
# daemon's own wire type) unless
#   - it is a RE-EXPORT (`export type { X } from "./generated/X"` — not a
#     declaration, so it never matches), or
#   - the SPA's `src/api/drift.ts` mentions X IN CODE (an explicit
#     Mutual/Satisfies assertion saying why it differs; a name that appears
#     only inside a comment does not count), or
#   - it is on the GRANDFATHER list scripts/ci/ts-shadow-allowlist.txt.
# The list can only SHRINK: an entry that no longer names a live shadow is
# itself a failure, so a fixed shadow must be struck off in the same commit.
#
# Why: kb-code's `PrReviewsOut` existed twice with different shapes and hid a
# real `string | null` vs `?: string` error; the hand `PrCommentOut` still
# lacks five fields the wire sends. A shadow is a second source of truth that
# `tsc` cannot compare.
#
# Compile-free (grep/sed only): safe for a lane that builds nothing.
set -euo pipefail
cd "$(dirname "$0")/.."

ALLOW=scripts/ci/ts-shadow-allowlist.txt
status=0
found=$(mktemp)
drift_code=$(mktemp)
trap 'rm -f "$found" "$drift_code"' EXIT

# Print a TS file with /* block */ and // line comments removed. A tiny
# tokenizer, not a regex: comment markers inside '...', "..." and `...`
# string literals (a "//" URL, a glob) are text, not comments, so code that
# follows such a string on the same line is kept. Backtick strings may span
# lines; single/double quotes end at the line end.
strip_comments() {
  awk '
    {
      line = $0; out = ""; n = length(line); i = 1
      while (i <= n) {
        c = substr(line, i, 1); d = substr(line, i, 2)
        if (inblock) {
          if (d == "*/") { inblock = 0; i += 2 } else { i++ }
        } else if (q != "") {
          out = out c
          if (c == "\\" && i < n) { out = out substr(line, i + 1, 1); i += 2; continue }
          if (c == q) q = ""
          i++
        } else if (d == "/*") { inblock = 1; i += 2 }
        else if (d == "//") { break }
        else {
          if (c == "\"" || c == "\047" || c == "`") q = c
          out = out c; i++
        }
      }
      if (q != "`") q = ""
      print out
    }' "$1"
}

for app in web web-code; do
  gen="$app/src/api/generated"
  [ -d "$gen" ] || continue
  drift="$app/src/api/drift.ts"
  : > "$drift_code"
  if [ -f "$drift" ]; then strip_comments "$drift" > "$drift_code"; fi
  for f in "$gen"/*.ts; do
    name=$(basename "$f" .ts)
    # Hand-written declarations of that name anywhere in the app's src/,
    # outside the generated dir itself.
    grep -rnE "^[[:space:]]*export[[:space:]]+(declare[[:space:]]+)?(interface|type)[[:space:]]+${name}([^A-Za-z0-9_]|$)" \
      "$app/src" --include='*.ts' --include='*.tsx' 2>/dev/null \
      | grep -v "^$gen/" | while IFS=: read -r path line _; do
        if grep -qE "\b${name}\b" "$drift_code"; then
          continue
        fi
        echo "$app:$name:$path" >> "$found"
      done || true
  done
done
sort -u -o "$found" "$found"

while IFS= read -r entry; do
  [ -z "$entry" ] && continue
  if ! grep -qxF "$entry" "$ALLOW"; then
    echo "SHADOW: $entry  (hand-written declaration shares a name with a generated wire type)"
    status=1
  fi
done < "$found"

while IFS= read -r entry; do
  case "$entry" in ''|'#'*) continue ;; esac
  if ! grep -qxF "$entry" "$found"; then
    echo "STALE allowlist entry (no longer a shadow — delete it): $entry"
    status=1
  fi
done < "$ALLOW"

if [ "$status" -ne 0 ]; then
  cat <<'MSG'

Replace the hand type with `export type { X } from "./generated/X"` (preferred),
or assert how it differs in the app's src/api/drift.ts (Mutual/Satisfies).
Never add to scripts/ci/ts-shadow-allowlist.txt: it is a ratchet.
MSG
  exit 1
fi
echo "ts-shadow gate: $(grep -cvE '^(#|$)' "$ALLOW") grandfathered shadow(s), none new"
