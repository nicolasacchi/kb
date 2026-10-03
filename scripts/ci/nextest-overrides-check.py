#!/usr/bin/env python3
"""Every `[[profile.*.overrides]]` filter in .config/nextest.toml must match at
least one test.

PF-C1 renamed three test binaries into one and the lance-heavy override
(`test-group = 'lance-heavy'`, max-threads = 1) silently stopped applying
because its filter matched NOTHING: no failure, just a serialization the CI
timeout depended on, gone. This asks nextest itself (`cargo nextest list -E`)
and fails on a filter with zero matches.

Needs the test binaries built, so it runs in workspace-test right after the
nextest run (the build is cached there), not in the compile-free supply-chain
job. Extra args after `--` are passed to `cargo nextest list` (the workspace
exclusions).
"""
import json
import subprocess
import sys

try:
    import tomllib
except ModuleNotFoundError:
    sys.exit("needs python >= 3.11")


def filters(path=".config/nextest.toml"):
    with open(path, "rb") as fh:
        cfg = tomllib.load(fh)
    out = []
    for pname, prof in (cfg.get("profile") or {}).items():
        for ov in prof.get("overrides", []) or []:
            if "filter" in ov:
                out.append((pname, ov["filter"]))
    return out


def count_matches(expr, extra):
    cmd = ["cargo", "nextest", "list", "--locked", "--message-format", "json", "-E", expr, *extra]
    res = subprocess.run(cmd, capture_output=True, text=True)
    if res.returncode != 0:
        sys.exit(f"`{' '.join(cmd)}` failed ({res.returncode}):\n{res.stderr[-2000:]}")
    # the JSON document is the last non-empty stdout line group; parse the whole stdout
    data = json.loads(res.stdout)
    n = 0
    for suite in (data.get("rust-suites") or {}).values():
        for tc in (suite.get("testcases") or {}).values():
            fm = (tc.get("filter-match") or {}).get("status")
            if fm == "matches":
                n += 1
    return n


def main():
    extra = sys.argv[sys.argv.index("--") + 1:] if "--" in sys.argv else []
    fs = filters()
    if not fs:
        sys.exit("no [[profile.*.overrides]] filters found -- this check would be vacuous")
    bad = 0
    for prof, expr in fs:
        n = count_matches(expr, extra)
        print(f"profile.{prof}: filter {expr!r} matches {n} test(s)")
        if n == 0:
            print(f"NEXTEST OVERRIDE MATCHES NOTHING: profile.{prof} filter {expr!r} (a renamed binary or test silently dropped the override)", file=sys.stderr)
            bad += 1
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
