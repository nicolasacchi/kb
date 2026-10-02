#!/usr/bin/env bash
# check-wire-ratchet.sh — the "typed by default" ratchet (I3).
#
# Two counts that may only go DOWN, committed in scripts/ci/wire-ratchet.toml:
#   hand_exports  `export interface|type` declarations in web-code/src/api/types.ts
#                 (each one a hand mirror of a daemon wire shape that tsc cannot
#                 compare against the Rust struct);
#   json_sites    `Json(json!(…))` / `Json(serde_json::json!(…))` response bodies in
#                 crates/kb-code-server/src (an untyped body no generator can see).
# A count ABOVE its ceiling fails: ship the new route/type with a `#[ts(export)]`
# struct instead. A count below the ceiling fails too, with the new number to
# commit, so the ceiling follows every reduction and cannot be spent later.
# Compile-free (grep only).
set -euo pipefail
cd "$(dirname "$0")/.."
FILE=scripts/ci/wire-ratchet.toml

hand=$(grep -cE '^export (interface|type) [A-Za-z_]' web-code/src/api/types.ts || true)
json=$(grep -rhoE 'Json\((serde_json::)?json!\(' crates/kb-code-server/src --include='*.rs' | wc -l | tr -d ' ')

want() { sed -nE "s/^$1[[:space:]]*=[[:space:]]*([0-9]+).*/\1/p" "$FILE" | head -1; }
status=0
check() {
  local key=$1 have=$2 ceil
  ceil=$(want "$key")
  if [ -z "$ceil" ]; then echo "missing $key in $FILE"; status=1; return; fi
  if [ "$have" -gt "$ceil" ]; then
    echo "RATCHET: $key is $have, ceiling $ceil — type the new wire shape (ts-export) instead of adding to it"
    status=1
  elif [ "$have" -lt "$ceil" ]; then
    echo "RATCHET: $key dropped to $have (ceiling $ceil) — lower the ceiling in $FILE to $have in this commit"
    status=1
  fi
}
check hand_exports "$hand"
check json_sites "$json"
[ "$status" -eq 0 ] && echo "wire ratchet: hand_exports=$hand json_sites=$json (at ceiling)"
exit "$status"
