#!/usr/bin/env bash
# wire-gates-selftest.sh — proves scripts/check-ts-shadows.sh and
# scripts/check-wire-ratchet.sh actually FAIL on the violations they claim to
# catch (a gate that cannot fail is the failure mode this milestone is about).
# Runs both against a throw-away mini repo; compile-free.
set -euo pipefail
here=$(cd "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

mk() {
  rm -rf "$tmp/r"; mkdir -p "$tmp/r/scripts/ci" "$tmp/r/web-code/src/api/generated" \
    "$tmp/r/web/src/api/generated" "$tmp/r/crates/kb-code-server/src"
  cp "$here/scripts/check-ts-shadows.sh" "$here/scripts/check-wire-ratchet.sh" "$tmp/r/scripts/"
  echo 'export type Foo = { a: string };' > "$tmp/r/web-code/src/api/generated/Foo.ts"
  printf 'export type { Foo } from "./generated/Foo";\nexport interface Bar { x: number }\n' \
    > "$tmp/r/web-code/src/api/types.ts"
  : > "$tmp/r/web-code/src/api/drift.ts"
  printf '# none\n' > "$tmp/r/scripts/ci/ts-shadow-allowlist.txt"
  printf 'hand_exports = 1\njson_sites = 1\n' > "$tmp/r/scripts/ci/wire-ratchet.toml"
  printf 'fn f(){ Json(json!({"a":1})); }\n' > "$tmp/r/crates/kb-code-server/src/a.rs"
}
expect() { # expect <ok|fail> <script> <why>
  local want=$1 s=$2 why=$3 rc=0
  (cd "$tmp/r" && bash "scripts/$s" >/dev/null 2>&1) || rc=$?
  if [ "$want" = ok ] && [ "$rc" -ne 0 ]; then echo "FAIL: $s should pass: $why"; exit 1; fi
  if [ "$want" = fail ] && [ "$rc" -eq 0 ]; then echo "FAIL: $s should fail: $why"; exit 1; fi
}

mk
expect ok check-ts-shadows.sh "re-export is not a shadow"
expect ok check-wire-ratchet.sh "at ceiling"

mk; echo 'export interface Foo { a: string }' >> "$tmp/r/web-code/src/api/types.ts"
expect fail check-ts-shadows.sh "hand interface shadows generated Foo"

mk; echo 'export interface Foo { a: string }' >> "$tmp/r/web-code/src/api/types.ts"
echo 'export type _Foo = Assert<Satisfies<FooWire, Foo>>;' > "$tmp/r/web-code/src/api/drift.ts"
expect ok check-ts-shadows.sh "shadow named in drift.ts code is accepted"

mk; echo 'export interface Foo { a: string }' >> "$tmp/r/web-code/src/api/types.ts"
printf '// Foo asserted here\n/* and Foo\n   again Foo */\n' > "$tmp/r/web-code/src/api/drift.ts"
expect fail check-ts-shadows.sh "a name that appears only in drift.ts comments is not an assertion"

mk; echo 'web-code:Gone:web-code/src/api/types.ts' >> "$tmp/r/scripts/ci/ts-shadow-allowlist.txt"
expect fail check-ts-shadows.sh "stale allowlist entry"

mk; echo 'export interface Baz { y: 1 }' >> "$tmp/r/web-code/src/api/types.ts"
expect fail check-wire-ratchet.sh "hand_exports above ceiling"

mk; printf 'fn g(){ Json(json!({"b":2})); }\n' >> "$tmp/r/crates/kb-code-server/src/a.rs"
expect fail check-wire-ratchet.sh "json_sites above ceiling"

mk; printf 'hand_exports = 5\njson_sites = 1\n' > "$tmp/r/scripts/ci/wire-ratchet.toml"
expect fail check-wire-ratchet.sh "ceiling not lowered after reduction"

echo "wire gates self-test: ok"
