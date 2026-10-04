#!/usr/bin/env bash
# Self-test for scripts/ci/no-cloud-stack.sh (run by `just ci-selfcheck`):
# a clean lock passes; each banned crate, an un-allowlisted quick-xml and a
# split lance/arrow version fail; the allowlist admits quick-xml.
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"; trap 'rm -rf "$work"' EXIT
fail=0
mk() { # mk <file> <name@version>...
  local f="$1"; shift; : > "$f"
  for nv in "$@"; do printf '[[package]]\nname = "%s"\nversion = "%s"\n\n' "${nv%@*}" "${nv#*@}" >> "$f"; done
}
base=(arrow@58.4.0 arrow-array@58.4.0 arrow-schema@58.4.0 lance@11.0.0 lance-index@11.0.0 lance-io@11.0.0 lancedb@0.38.0)
: > "$work/empty.allow"
printf 'quick-xml\n' > "$work/qx.allow"
check() { # check <label> <want-rc> <lock> <allow>
  bash "$here/no-cloud-stack.sh" "$3" "$4" >"$work/out" 2>&1; local rc=$?
  if [ "$rc" -ne "$2" ]; then echo "SELFTEST FAIL: $1: want $2 got $rc" >&2; cat "$work/out" >&2; fail=1; else echo "ok: $1"; fi
}
mk "$work/clean.lock" "${base[@]}" serde@1.0.0
check "clean lock passes" 0 "$work/clean.lock" "$work/empty.allow"
for b in opendal reqsign-google reqsign-azure-storage aws-config aws-sdk-s3 rsa goosefs-sdk; do
  mk "$work/b.lock" "${base[@]}" "$b@1.0.0"
  check "banned $b fails" 1 "$work/b.lock" "$work/empty.allow"
done
mk "$work/qx.lock" "${base[@]}" quick-xml@0.39.4
check "quick-xml without allowlist fails" 1 "$work/qx.lock" "$work/empty.allow"
check "quick-xml with allowlist passes" 0 "$work/qx.lock" "$work/qx.allow"
mk "$work/qxnew.lock" "${base[@]}" quick-xml@0.41.0
check "quick-xml 0.41 passes" 0 "$work/qxnew.lock" "$work/empty.allow"
mk "$work/split.lock" "${base[@]}" arrow@57.0.0
# invariant:1 two arrow versions in the lock fail the supply-chain gate
check "two arrow versions fail" 1 "$work/split.lock" "$work/empty.allow"
mk "$work/nolance.lock" arrow@58.4.0 arrow-array@58.4.0 arrow-schema@58.4.0
check "missing lance fails" 1 "$work/nolance.lock" "$work/empty.allow"
exit "$fail"
