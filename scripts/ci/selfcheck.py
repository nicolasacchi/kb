#!/usr/bin/env python3
"""`just ci-selfcheck`: compile-free assertions that the CI itself cannot pass
without running. Every check reads committed text; nothing builds.

  1. every workflow declares `defaults.run.shell: bash` (pipefail by default)
     and a workflow-level `permissions:`; every job in it declares its own
     `permissions:` block;
  2. no workflow `run:` line, composite action (.github/actions/**) or justfile line pipes into a quiet `grep -q`/`grep -qE` (SIGPIPE
     under pipefail makes "found" read as "not found");
  3. the five path-filtered `code-*` jobs of ci.yml each call
     scripts/ci/code-changed.sh (and its --skip-message) and carry no inline
     copy of the filter;
  4. every `witness.py ... --lane X` in ci.yml names a lane that has a floor in
     ci/test-floors.toml, and every floor lane is witnessed;
  5. every `actions/checkout` pin is the same commit SHA with the same version
     label, and no workflow uses a floating checkout tag;
  6. deny.toml `review by <date>` exceptions have not expired.

Usage: selfcheck.py [--today YYYY-MM-DD] [--root DIR]
"""
import argparse
import datetime
import glob
import os
import re
import sys

try:
    import tomllib
except ModuleNotFoundError:
    sys.exit("selfcheck.py needs python >= 3.11")

CODE_JOBS = ["code-drift", "code-lint", "code-test", "code-spa", "code-e2e"]


def jobs_of(text):
    """{job_id: block_text} for the top-level `jobs:` mapping."""
    lines = text.split("\n")
    try:
        start = lines.index("jobs:")
    except ValueError:
        return {}
    heads = [i for i in range(start + 1, len(lines)) if re.match(r"^  [A-Za-z0-9_-]+:\s*$", lines[i])]
    heads.append(len(lines))
    return {lines[a].strip().rstrip(":"): "\n".join(lines[a:b]) for a, b in zip(heads, heads[1:])}


QUIET_GREP = re.compile(r"\|\s*grep\s+(-[A-Za-z]*q[A-Za-z]*)\b")


def check_quiet_grep(root, errs):
    """`| grep -q` in a pipefail shell, in the files that run shell outside the
    workflow yml proper: composite actions and the justfile (F1 carry)."""
    paths = sorted(glob.glob(os.path.join(root, ".github/actions/**/*.yml"), recursive=True))
    paths.append(os.path.join(root, "justfile"))
    for path in paths:
        if not os.path.exists(path):
            continue
        name = os.path.relpath(path, root)
        for i, line in enumerate(open(path, encoding="utf-8").read().split("\n"), 1):
            if re.match(r"^\s*#", line):
                continue
            if QUIET_GREP.search(line):
                errs.append(f"{name}:{i}: `| grep -q` in a pipefail shell (SIGPIPE turns a match into a miss): {line.strip()[:90]}")


def check_workflows(root, errs):
    for path in sorted(glob.glob(os.path.join(root, ".github/workflows/*.yml"))):
        name = os.path.relpath(path, root)
        text = open(path, encoding="utf-8").read()
        head = text.split("\njobs:", 1)[0]
        if not re.search(r"^defaults:\n  run:\n    shell: bash\s*$", head, re.M):
            errs.append(f"{name}: no `defaults: run: shell: bash` (Actions' implicit shell has no pipefail)")
        if not re.search(r"^permissions:", head, re.M):
            errs.append(f"{name}: no workflow-level `permissions:` (declare `permissions: {{}}` and grant per job)")
        for job, block in jobs_of(text).items():
            if not re.search(r"^    permissions:", block, re.M):
                errs.append(f"{name}: job `{job}` declares no `permissions:`")
        for i, line in enumerate(text.split("\n"), 1):
            code = line.split("#", 1)[0] if re.match(r"^\s*#", line) else line
            if re.match(r"^\s*#", line):
                continue
            if re.search(r"\|\s*grep\s+(-[A-Za-z]*q[A-Za-z]*)\b", code):
                errs.append(f"{name}:{i}: `| grep -q` in a pipefail shell (SIGPIPE turns a match into a miss): {line.strip()[:90]}")


def check_code_jobs(root, errs):
    path = os.path.join(root, ".github/workflows/ci.yml")
    text = open(path, encoding="utf-8").read()
    jobs = jobs_of(text)
    for j in CODE_JOBS:
        block = jobs.get(j)
        if block is None:
            errs.append(f"ci.yml: job `{j}` not found")
            continue
        if "scripts/ci/code-changed.sh\n" not in block:
            errs.append(f"ci.yml: `{j}` does not call scripts/ci/code-changed.sh")
        if "scripts/ci/code-changed.sh --skip-message" not in block:
            errs.append(f"ci.yml: `{j}` skip step does not use code-changed.sh --skip-message")
        if re.search(r"crates/kb-\(code-\(server", block):
            errs.append(f"ci.yml: `{j}` still carries an inline copy of the path filter")
    # the script's regex must still cover the workflow files themselves
    script = open(os.path.join(root, "scripts/ci/code-changed.sh"), encoding="utf-8").read()
    for must in ('".github/workflows/ci.yml"', '"scripts/ci/code-changed.sh"', '"rust-toolchain.toml"', '"scripts/review-store/"',
                 '"ci/test-floors.toml"', '"scripts/ci/witness.py"', '".config/nextest.toml"', '"scripts/ci/selfcheck.py"'):
        if must not in script:
            errs.append(f"code-changed.sh no longer lists {must} (the filter must see its own definition)")


def check_witness(root, errs):
    text = open(os.path.join(root, ".github/workflows/ci.yml"), encoding="utf-8").read()
    used = set(re.findall(r"witness\.py\s+\S+\s+\S+\s+--lane\s+([A-Za-z0-9_-]+)", text))
    with open(os.path.join(root, "ci/test-floors.toml"), "rb") as fh:
        floors = tomllib.load(fh)
    for lane in sorted(used - set(floors)):
        errs.append(f"ci.yml witnesses lane `{lane}` but ci/test-floors.toml has no floor for it")
    for lane in sorted(set(floors) - used):
        errs.append(f"ci/test-floors.toml has a floor for `{lane}` that no ci.yml step witnesses")
    for lane, v in floors.items():
        if not isinstance(v.get("executed"), int) or v["executed"] < 1:
            errs.append(f"ci/test-floors.toml: `{lane}` has no positive integer `executed`")


def check_checkout_pins(root, errs):
    pins = {}
    for path in sorted(glob.glob(os.path.join(root, ".github/**/*.yml"), recursive=True)):
        for i, line in enumerate(open(path, encoding="utf-8"), 1):
            m = re.search(r"uses:\s*actions/checkout@(\S+)(?:\s+#\s*(\S+))?", line)
            if m:
                pins.setdefault((m.group(1), m.group(2)), []).append(f"{os.path.relpath(path, root)}:{i}")
    if len(pins) > 1:
        errs.append("actions/checkout is pinned inconsistently: " + "; ".join(f"{k} x{len(v)}" for k, v in pins.items()))
    for (ref, label), where in pins.items():
        if not re.fullmatch(r"[0-9a-f]{40}", ref):
            errs.append(f"actions/checkout@{ref} is not a commit SHA ({where[0]})")
        if not label or not re.fullmatch(r"v\d+\.\d+\.\d+", label):
            errs.append(f"actions/checkout@{ref[:8]} has no exact version label (got {label!r}) ({where[0]})")


def check_deny_review_by(root, today, errs):
    text = open(os.path.join(root, "deny.toml"), encoding="utf-8").read()
    seen = 0
    for i, line in enumerate(text.split("\n"), 1):
        for m in re.finditer(r"review[ -]by\s+(\d{4}-\d{2}-\d{2})", line, re.I):
            seen += 1
            d = datetime.date.fromisoformat(m.group(1))
            if today > d:
                errs.append(f"deny.toml:{i}: exception review-by {d} has EXPIRED (today {today}); re-review it and move the date, or drop the ignore")
    if seen == 0:
        errs.append("deny.toml: no `review by <date>` marker found -- the expiry check would be vacuous")


def run(root, today):
    errs = []
    check_workflows(root, errs)
    check_quiet_grep(root, errs)
    check_code_jobs(root, errs)
    check_witness(root, errs)
    check_checkout_pins(root, errs)
    check_deny_review_by(root, today, errs)
    return errs


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--today", default=None)
    ap.add_argument("--root", default=os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".."))
    a = ap.parse_args(argv)
    today = datetime.date.fromisoformat(a.today) if a.today else datetime.date.today()
    errs = run(os.path.abspath(a.root), today)
    for e in errs:
        print(f"SELFCHECK FAIL: {e}", file=sys.stderr)
    if errs:
        return 1
    print("ci-selfcheck: workflows, code-* filter, witness floors, checkout pins, deny review-by: ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
