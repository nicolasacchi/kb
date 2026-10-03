#!/usr/bin/env python3
"""Self-test for scripts/ci/witness.py (run by `just ci-selfcheck`).

Every case builds a synthetic report. The point of the file is the cases the
exit-code verdict got wrong: a report with failures, a lane that executed far
fewer tests than its floor, a missing/garbled report, and a skipped lane.
"""
import contextlib
import io
import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import witness  # noqa: E402

FLOORS = '[e2e]\nexecuted = 10\n[nx]\nexecuted = 2\n[vt]\nexecuted = 5\n'


def pw(expected=10, unexpected=0, flaky=0, skipped=0, flaky_titles=(), bad_status=None, errors=()):
    specs = []
    for i in range(expected):
        specs.append({"title": f"t{i}", "tests": [{"projectName": "chromium", "status": "expected"}]})
    for t in flaky_titles:
        specs.append({"title": t, "tests": [{"projectName": "chromium", "status": "flaky"}]})
    if bad_status:
        specs.append({"title": "bad", "tests": [{"projectName": "chromium", "status": bad_status}]})
    return {"stats": {"expected": expected, "unexpected": unexpected, "flaky": flaky, "skipped": skipped},
            "suites": [{"title": "a.spec.ts", "specs": specs}], "errors": list(errors)}


class Witness(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp()
        self.floors = os.path.join(self.d, "floors.toml")
        with open(self.floors, "w") as fh:
            fh.write(FLOORS)

    def write(self, name, obj, raw=None):
        p = os.path.join(self.d, name)
        with open(p, "w") as fh:
            fh.write(raw if raw is not None else json.dumps(obj))
        return p

    def run_w(self, *argv):
        err = io.StringIO()
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(err):
            try:
                rc = witness.main(list(argv))
            except SystemExit as e:
                rc = e.code
        return rc, err.getvalue()

    def base(self, kind, path, lane="e2e", *extra):
        return self.run_w(kind, path, "--lane", lane, "--floors", self.floors, *extra)

    def test_green(self):
        self.assertEqual(self.base("playwright", self.write("r.json", pw()))[0], 0)

    def test_unexpected_fails(self):
        rc, err = self.base("playwright", self.write("r.json", pw(expected=10, unexpected=1)))
        self.assertEqual(rc, 1)
        self.assertIn("failed", err)

    def test_status_not_counted_in_stats_still_fails(self):
        rc, _ = self.base("playwright", self.write("r.json", pw(bad_status="unexpected")))
        self.assertEqual(rc, 1)

    def test_below_floor_fails_even_with_zero_failures(self):
        rc, err = self.base("playwright", self.write("r.json", pw(expected=3)))
        self.assertEqual(rc, 1)
        self.assertIn("floor is 10", err)

    def test_suite_level_error_fails(self):
        rc, _ = self.base("playwright", self.write("r.json", pw(errors=[{"message": "globalSetup blew up"}])))
        self.assertEqual(rc, 1)

    def test_missing_and_garbled_report_fail(self):
        self.assertEqual(self.base("playwright", os.path.join(self.d, "nope.json"))[0], 1)
        self.assertEqual(self.base("playwright", self.write("g.json", None, raw="{not json"))[0], 1)
        self.assertEqual(self.base("playwright", self.write("e.json", {"hello": 1}))[0], 1)

    def test_flaky_is_not_a_failure_but_repeat_flaky_is(self):
        cur = self.write("cur.json", pw(expected=10, flaky=1, flaky_titles=["wobbly"]))
        self.assertEqual(self.base("playwright", cur)[0], 0)
        prev_same = self.write("prev.json", pw(expected=10, flaky=1, flaky_titles=["wobbly"]))
        rc, err = self.base("playwright", cur, "e2e", "--prev", prev_same, "--fail-repeat-flaky")
        self.assertEqual(rc, 1)
        self.assertIn("wobbly", err)
        prev_other = self.write("prev2.json", pw(expected=10, flaky=1, flaky_titles=["different"]))
        self.assertEqual(self.base("playwright", cur, "e2e", "--prev", prev_other, "--fail-repeat-flaky")[0], 0)

    def test_skipped_lane_is_not_zero_of_n(self):
        rc, _ = self.run_w("playwright", os.path.join(self.d, "absent.json"), "--lane", "e2e",
                           "--floors", self.floors, "--only-if", "false")
        self.assertEqual(rc, 0)
        rc, _ = self.run_w("playwright", os.path.join(self.d, "absent.json"), "--lane", "e2e",
                           "--floors", self.floors, "--only-if", "true")
        self.assertEqual(rc, 1)

    def test_unknown_lane_has_no_floor(self):
        rc, err = self.run_w("playwright", self.write("r.json", pw()), "--lane", "ghost", "--floors", self.floors)
        self.assertEqual(rc, 1)
        self.assertIn("no positive integer", err)

    def test_vitest(self):
        ok = {"numPassedTests": 6, "numFailedTests": 0, "numTotalTests": 6, "numFailedTestSuites": 0, "success": True}
        self.assertEqual(self.base("vitest", self.write("v.json", ok), "vt")[0], 0)
        bad = dict(ok, numFailedTests=1, success=False)
        self.assertEqual(self.base("vitest", self.write("v2.json", bad), "vt")[0], 1)
        few = dict(ok, numPassedTests=2, numTotalTests=2)
        self.assertEqual(self.base("vitest", self.write("v3.json", few), "vt")[0], 1)

    def test_nextest_junit(self):
        def xml(*cases):
            return '<testsuites>' + '<testsuite name="x">' + ''.join(cases) + '</testsuite></testsuites>'
        ok = xml('<testcase classname="a" name="t1"/>', '<testcase classname="a" name="t2"/>',
                 '<testcase classname="a" name="t3"><skipped/></testcase>')
        self.assertEqual(self.base("nextest-junit", self.write("j.xml", None, raw=ok), "nx")[0], 0)
        bad = xml('<testcase classname="a" name="t1"/>', '<testcase classname="a" name="t2"><failure/></testcase>')
        self.assertEqual(self.base("nextest-junit", self.write("j2.xml", None, raw=bad), "nx")[0], 1)
        self.assertEqual(self.base("nextest-junit", self.write("j3.xml", None, raw="<testsuites/>"), "nx")[0], 1)

    def test_committed_floors_are_wellformed(self):
        import tomllib
        here = os.path.dirname(os.path.abspath(__file__))
        with open(os.path.join(here, "..", "..", "ci", "test-floors.toml"), "rb") as fh:
            f = tomllib.load(fh)
        for lane in ("e2e", "code-e2e", "nextest", "web-vitest", "web-code-vitest"):
            self.assertGreater(f[lane]["executed"], 0, lane)


if __name__ == "__main__":
    unittest.main(verbosity=1)
