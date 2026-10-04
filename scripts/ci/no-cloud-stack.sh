#!/usr/bin/env bash
# v0.45 N7 -- the cloud object-store stack must not be in Cargo.lock, and the
# lance/arrow lockstep (invariant #1) must resolve to ONE version of each.
#
# kb only ever opens local filesystem paths, so `lance` is declared with
# `default-features = false` (root Cargo.toml). A feature-unification slip (a
# crate re-enabling lance's `aws`/`gcp`/`azure`/`oss`/`goosefs`/`geo`
# features) would silently put opendal/reqsign/aws-sdk/rsa back into the
# binaries and revive the RUSTSEC-2023-0071 / quick-xml exposure. This gate
# reads the COMMITTED Cargo.lock (no compile, no network):
#
#   1. none of BANNED may appear in the lock;
#   2. no quick-xml older than 0.41.0 (the line RUSTSEC-2026-0194/0195 are
#      fixed in) unless "quick-xml" is listed in scripts/ci/no-cloud-stack.allow
#      (one crate name per line, '#' comments) -- the explicit allowlist for a
#      legitimate retention such as lance-namespace-impls' REST client. A
#      quick-xml >= 0.41 (e.g. via plist) is fine;
#   3. arrow, arrow-array, arrow-schema, lance, lance-index, lance-io and
#      lancedb each resolve to exactly one version.
#
# Usage: no-cloud-stack.sh [path/to/Cargo.lock] [path/to/allow-file]
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
lock="${1:-$here/../../Cargo.lock}"
allow="${2:-$here/no-cloud-stack.allow}"
python3 - "$lock" "$allow" <<'PY'
import collections, os, sys, tomllib

lock_path, allow_path = sys.argv[1], sys.argv[2]
BANNED = ["opendal", "reqsign-google", "reqsign-azure-storage", "aws-config",
          "aws-sdk-s3", "rsa", "goosefs-sdk"]
GUARDED = ["quick-xml"]
SINGLE = ["arrow", "arrow-array", "arrow-schema", "lance", "lance-index",
          "lance-io", "lancedb"]

with open(lock_path, "rb") as f:
    pkgs = tomllib.load(f).get("package", [])
vers = collections.defaultdict(list)
for p in pkgs:
    vers[p["name"]].append(p["version"])

allowed = set()
if os.path.exists(allow_path):
    for line in open(allow_path):
        line = line.split("#", 1)[0].strip()
        if line:
            allowed.add(line)

bad = []
for name in BANNED:
    if name in vers:
        bad.append(f"{name} {vers[name]} is in Cargo.lock (cloud stack re-enabled)")
def key(v):
    return tuple(int(x) for x in v.split("-")[0].split("+")[0].split("."))
for name in GUARDED:
    old = [v for v in vers.get(name, []) if key(v) < (0, 41, 0)]
    if old and name not in allowed:
        bad.append(f"{name} {old} (< 0.41.0, advisory-affected) is in Cargo.lock and not in the allowlist")
for name in SINGLE:
    if len(vers.get(name, [])) != 1:
        bad.append(f"{name} must resolve to exactly one version, found {vers.get(name, [])}")

if bad:
    print("no-cloud-stack: FAIL", file=sys.stderr)
    for b in bad:
        print("  - " + b, file=sys.stderr)
    print("find the re-enabler with: cargo tree -e features -i lance-io", file=sys.stderr)
    sys.exit(1)
print(f"no-cloud-stack: ok ({len(pkgs)} packages; lance/arrow single-version)")
PY
