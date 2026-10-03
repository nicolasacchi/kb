#!/usr/bin/env bash
# Proves the ghcr-gc selection rule on a fixture. Pure jq, no network.
# The v0.43 release images were pruned by the old action-based GC although its
# ignore-versions regex was meant to protect them; this pins the rule that
# replaced it.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
gc="$here/../ghcr-gc.sh"

fixture="$(python3 - <<'PY'
import json
v = []
def add(i, tags, day):
    v.append({"id": i, "created_at": f"2026-09-{day:02d}T00:00:00Z",
              "metadata": {"container": {"tags": tags}}})
add(1, ["0.43"], 1)                    # release, oldest of all
add(2, ["latest", "0.43"], 1)          # release + latest
add(3, [], 2)                          # untagged child manifest
add(4, ["main-aaaaaaaaaaaa", "latest"], 3)   # MIXED: main-* next to latest
add(5, ["main-bbbbbbbbbbbb", "v0.44"], 4)    # MIXED: main-* next to semver
for n in range(6, 20):                 # 14 pure main-* versions, ids 6..19
    add(n, [f"main-{n:012d}"], 5 + (n - 6))
print(json.dumps(v))
PY
)"

KEEP=10 bash "$gc" --select-only <<<"$fixture" | cut -f1 | sort -n | tr '\n' ' ' > /tmp/ghcr-gc-sel.$$
got="$(cat /tmp/ghcr-gc-sel.$$)"; rm -f /tmp/ghcr-gc-sel.$$
# 14 pure main-* versions (ids 6..19), newest 10 kept (10..19) -> 6 7 8 9 deleted.
want="6 7 8 9 "
if [ "$got" != "$want" ]; then
  echo "FAIL: selected '$got' want '$want'" >&2; exit 1
fi
for protected in 1 2 3 4 5; do
  case " $got" in *" $protected "*) echo "FAIL: protected version $protected selected" >&2; exit 1;; esac
done

# KEEP larger than the population selects nothing.
none="$(KEEP=50 bash "$gc" --select-only <<<"$fixture")"
[ -z "$none" ] || { echo "FAIL: KEEP=50 selected '$none'" >&2; exit 1; }

# A dry run over the same fixture exits 0 and reports, never deleting
# (gh is not even on the PATH for this call).
printf '%s' "$fixture" > /tmp/ghcr-gc-fixture.$$
out="$(PATH=/usr/bin:/bin PACKAGE=kb VERSIONS_FILE=/tmp/ghcr-gc-fixture.$$ KEEP=10 bash "$gc")"
rm -f /tmp/ghcr-gc-fixture.$$
case "$out" in *"dry run: nothing deleted"*"") ;; *) echo "FAIL: dry run output: $out" >&2; exit 1;; esac
echo "ghcr-gc selftest OK"
