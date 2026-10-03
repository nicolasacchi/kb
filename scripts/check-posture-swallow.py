#!/usr/bin/env python3
"""Ratchet on swallowed errors in security-posture modules (v0.44 F1).

`#![deny(clippy::let_underscore_must_use)]` stops `let _ = fallible();`, but a
posture bug hides just as well behind `.ok()` (Result -> Option, error gone)
or `unwrap_or(<permissive default>)` (error -> the PERMISSIVE value). Neither
is banned outright -- many are right -- so this counts them per file in the
non-test part of each posture module and holds the count at a committed
baseline (ci/posture-swallow-baseline.toml) that may only go DOWN. A new one
must be justified by raising the baseline in a PR that says why.

Usage: check-posture-swallow.py [--self-test]
"""
import os
import re
import sys
import tomllib

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
BASELINE = os.path.join(ROOT, "ci", "posture-swallow-baseline.toml")
FILES = [
    "crates/kb-server/src/middleware.rs",
    "crates/kb-code-server/src/security/mod.rs",
    "crates/kb-code-server/src/security/audit.rs",
    "crates/kb-code-server/src/security/origin.rs",
    "crates/kb-code-server/src/security/paths.rs",
    "crates/kb-code-server/src/security/secrets.rs",
    "crates/kb-code-server/src/github.rs",
    "crates/kb-code-server/src/review_gate.rs",
    "crates/kb-code-server/src/review_store/cred.rs",
    "crates/kb-code-server/src/review_store/registry.rs",
]
PATTERNS = [r"\.ok\(\)", r"\bunwrap_or\(", r"\bunwrap_or_default\(\)"]


def non_test(text):
    """The file up to its first `#[cfg(test)]` (test modules sit at the end)."""
    m = re.search(r"^#\[cfg\(test\)\]", text, re.M)
    return text[: m.start()] if m else text


def count(text):
    body = non_test(text)
    n = 0
    for line in body.split("\n"):
        code = re.sub(r"//.*$", "", line)  # comments never count
        n += sum(len(re.findall(p, code)) for p in PATTERNS)
    return n


def measure(root=ROOT, files=FILES):
    out = {}
    for f in files:
        p = os.path.join(root, f)
        if not os.path.exists(p):
            raise SystemExit(f"posture file {f} no longer exists -- update FILES (the ratchet would be vacuous)")
        out[f] = count(open(p, encoding="utf-8").read())
    return out


def main():
    if "--self-test" in sys.argv:
        assert count("fn a(){ x.ok(); y.unwrap_or(1); }\n#[cfg(test)]\nmod t{ z.ok(); }") == 2
        assert count("// x.ok()\nfn a(){}") == 0
        assert count("fn a(){ q.unwrap_or_default(); }") == 1
        print("check-posture-swallow self-test ok")
        return 0
    with open(BASELINE, "rb") as fh:
        base = tomllib.load(fh).get("files", {})
    now = measure()
    bad = 0
    for f, n in now.items():
        b = base.get(f)
        if b is None:
            print(f"POSTURE RATCHET: {f} has no baseline entry (measured {n}); add it to ci/posture-swallow-baseline.toml", file=sys.stderr)
            bad += 1
        elif n > b:
            print(f"POSTURE RATCHET: {f} now has {n} `.ok()`/`unwrap_or(` forms in non-test code, baseline {b}. "
                  "Each can turn a failed security check into a permissive default; handle the error by name, or raise the baseline in a PR that says why.", file=sys.stderr)
            bad += 1
        elif n < b:
            print(f"note: {f} is down to {n} (baseline {b}); lower the baseline to keep the ratchet tight")
    for f in set(base) - set(now):
        print(f"POSTURE RATCHET: baseline lists {f}, which is not in FILES", file=sys.stderr)
        bad += 1
    print(f"posture-swallow ratchet: {sum(now.values())} form(s) across {len(now)} module(s)")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
