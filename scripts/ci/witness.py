#!/usr/bin/env python3
"""Decide a test lane from its machine-readable REPORT, not its exit code.

A process exit code is exactly what `| tee`, an empty filter, a harness that
never started, or a skipped suite can fake (the e2e job once exited 0 with
23-24 failing specs). This script reads the report the test tool itself wrote
and requires, for the lane to count as green:

  * failed == 0 (nothing unexpected, no suite-level error);
  * executed >= the committed floor in ci/test-floors.toml, so a lane that
    "passes" because half its tests silently stopped running goes red.

Flaky (passed only after a retry) is reported separately and is NOT a failure
-- unless `--prev PREV_REPORT --fail-repeat-flaky` finds the SAME test flaky
in the previous report too (hosted-runner load loses a different spec each
run; a repeat name is a real bug).

Usage:
  witness.py <kind> <report> --lane NAME [--floors ci/test-floors.toml]
             [--only-if true|false|...] [--prev REPORT --fail-repeat-flaky]
kind: playwright | vitest | nextest-junit

`--only-if V`: the lane's own path-filter output (steps.changes.outputs.run).
Anything but "true" means the lane legitimately did not run -> exit 0 and say
so (a skipped lane is not "0 of N tests").

Floors only move DOWN with a PR that says why (deleting tests is legitimate
but must be visible). When executed is well above the floor the script prints
the suggested new floor, so raising it is a one-line PR.
Stdlib only (python >= 3.11 for tomllib).
"""
import argparse
import json
import os
import sys
import xml.etree.ElementTree as ET

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover
    sys.exit("witness.py needs python >= 3.11 (tomllib)")


def die(msg):
    print(f"WITNESS FAIL: {msg}", file=sys.stderr)
    sys.exit(1)


def load_json(path):
    try:
        with open(path, encoding="utf-8") as fh:
            return json.load(fh)
    except FileNotFoundError:
        die(f"report {path} does not exist -- the lane did not write one, so nothing is proven")
    except json.JSONDecodeError as exc:
        die(f"report {path} is not valid JSON ({exc})")


def playwright(path):
    rep = load_json(path)
    stats = rep.get("stats")
    if not isinstance(stats, dict):
        die(f"{path}: no `stats` object -- not a Playwright JSON report")
    flaky_ids, bad = [], []

    def walk(suite, trail):
        trail = trail + ([suite["title"]] if suite.get("title") else [])
        for spec in suite.get("specs", []):
            for test in spec.get("tests", []):
                ident = " > ".join(trail + [spec.get("title", "?")]) + f" [{test.get('projectName', '')}]"
                st = test.get("status")
                if st == "flaky":
                    flaky_ids.append(ident)
                elif st not in ("expected", "skipped"):
                    bad.append(f"{ident}: {st}")
        for sub in suite.get("suites", []):
            walk(sub, trail)

    for top in rep.get("suites", []):
        walk(top, [])
    errors = rep.get("errors") or []
    failed = int(stats.get("unexpected", 0)) + len(errors)
    for b in bad:
        print(f"  not-green test: {b}", file=sys.stderr)
    for e in errors:
        print(f"  suite-level error: {str(e.get('message', e))[:300]}", file=sys.stderr)
    # a test whose status is neither expected/skipped/flaky is a failure even if
    # `stats` somehow undercounts it
    failed = max(failed, len(bad) + len(errors))
    executed = int(stats.get("expected", 0)) + int(stats.get("flaky", 0))
    return dict(executed=executed, failed=failed, flaky=sorted(set(flaky_ids)),
                skipped=int(stats.get("skipped", 0)))


def vitest(path):
    rep = load_json(path)
    for k in ("numPassedTests", "numFailedTests", "numTotalTests"):
        if k not in rep:
            die(f"{path}: missing `{k}` -- not a vitest JSON report")
    failed = int(rep["numFailedTests"]) + int(rep.get("numFailedTestSuites", 0))
    if rep.get("success") is False and failed == 0:
        failed = 1
    return dict(executed=int(rep["numPassedTests"]), failed=failed, flaky=[],
                skipped=int(rep.get("numPendingTests", 0)) + int(rep.get("numTodoTests", 0)))


def nextest_junit(path):
    try:
        root = ET.parse(path).getroot()
    except FileNotFoundError:
        die(f"report {path} does not exist -- the lane did not write one, so nothing is proven")
    except ET.ParseError as exc:
        die(f"report {path} is not valid XML ({exc})")
    executed = failed = skipped = 0
    flaky = []
    for case in root.iter("testcase"):
        name = f"{case.get('classname', '')}::{case.get('name', '')}"
        if case.find("skipped") is not None:
            skipped += 1
            continue
        if case.find("failure") is not None or case.find("error") is not None:
            failed += 1
            continue
        if case.find("flakyFailure") is not None or case.find("flakyError") is not None:
            flaky.append(name)
        executed += 1
    return dict(executed=executed, failed=failed, flaky=sorted(set(flaky)), skipped=skipped)


KINDS = {"playwright": playwright, "vitest": vitest, "nextest-junit": nextest_junit}


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("kind", choices=sorted(KINDS))
    ap.add_argument("report")
    ap.add_argument("--lane", required=True)
    ap.add_argument("--floors", default="ci/test-floors.toml")
    ap.add_argument("--only-if", default=None)
    ap.add_argument("--prev", default=None)
    ap.add_argument("--fail-repeat-flaky", action="store_true")
    args = ap.parse_args(argv)

    summary = os.environ.get("GITHUB_STEP_SUMMARY")

    def say(line):
        print(line)
        if summary:
            with open(summary, "a", encoding="utf-8") as fh:
                fh.write(line + "\n")

    if args.only_if is not None and args.only_if != "true":
        say(f"### witness `{args.lane}`: lane skipped by its path filter (only-if={args.only_if!r}); nothing to prove")
        return 0

    try:
        with open(args.floors, "rb") as fh:
            floors = tomllib.load(fh)
    except FileNotFoundError:
        die(f"{args.floors} not found")
    entry = floors.get(args.lane)
    if not isinstance(entry, dict) or not isinstance(entry.get("executed"), int) or entry["executed"] < 1:
        die(f"{args.floors} has no positive integer `executed` floor for lane `{args.lane}`")
    floor = entry["executed"]

    res = KINDS[args.kind](args.report)
    problems = []
    if res["failed"]:
        problems.append(f"{res['failed']} failed/unexpected")
    if res["executed"] < floor:
        problems.append(f"only {res['executed']} executed, floor is {floor} (lower the floor in {args.floors} in a PR that says why tests were removed)")

    if args.prev and args.fail_repeat_flaky and os.path.exists(args.prev):
        prev = KINDS[args.kind](args.prev)
        again = sorted(set(res["flaky"]) & set(prev["flaky"]))
        if again:
            problems.append("flaky in two consecutive runs (a repeat is a defect, not load): " + "; ".join(again))

    say(f"### witness `{args.lane}`: {res['executed']} executed, {res['failed']} failed, "
        f"{len(res['flaky'])} flaky, {res['skipped']} skipped (floor {floor})")
    for f in res["flaky"]:
        say(f"- flaky: {f}")
    if res["executed"] > floor + max(20, floor // 20):
        say(f"- suggested floor: {res['executed'] - res['executed'] % 5 - 5} (raise `[{args.lane}] executed` in {args.floors})")
    if problems:
        die(f"lane `{args.lane}`: " + "; ".join(problems))
    return 0


if __name__ == "__main__":
    sys.exit(main())
