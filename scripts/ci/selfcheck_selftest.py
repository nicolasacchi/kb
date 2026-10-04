#!/usr/bin/env python3
"""Proves scripts/ci/selfcheck.py FAILS on each defect it claims to catch."""
import datetime
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import selfcheck  # noqa: E402

GOOD_WF = """name: x
on: push
permissions: {}
defaults:
  run:
    shell: bash
jobs:
  a:
    permissions:
      contents: read
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
      - run: echo hi
"""


class SelfCheck(unittest.TestCase):
    def tree(self, wf=GOOD_WF, deny="# review by 2026-12-31\n"):
        d = tempfile.mkdtemp()
        os.makedirs(os.path.join(d, ".github/workflows"))
        with open(os.path.join(d, ".github/workflows/x.yml"), "w") as fh:
            fh.write(wf)
        with open(os.path.join(d, "deny.toml"), "w") as fh:
            fh.write(deny)
        return d

    def errs(self, fn, d, *a):
        e = []
        fn(d, *a, e) if a else fn(d, e)
        return e

    def test_good_workflow_is_clean(self):
        d = self.tree()
        self.assertEqual(self.errs(selfcheck.check_workflows, d), [])
        self.assertEqual(self.errs(selfcheck.check_checkout_pins, d), [])

    def test_missing_default_shell(self):
        d = self.tree(GOOD_WF.replace("defaults:\n  run:\n    shell: bash\n", ""))
        self.assertTrue(any("shell: bash" in e for e in self.errs(selfcheck.check_workflows, d)))

    def test_missing_workflow_and_job_permissions(self):
        d = self.tree(GOOD_WF.replace("permissions: {}\n", "").replace("    permissions:\n      contents: read\n", ""))
        e = self.errs(selfcheck.check_workflows, d)
        self.assertTrue(any("workflow-level" in x for x in e))
        self.assertTrue(any("job `a`" in x for x in e))

    def test_quiet_grep_in_a_pipe(self):
        d = self.tree(GOOD_WF.replace("echo hi", "git diff --name-only | grep -qE '^x'"))
        self.assertTrue(any("grep -q" in x for x in self.errs(selfcheck.check_workflows, d)))
        # a comment that merely mentions it is fine
        d = self.tree(GOOD_WF.replace("      - run: echo hi", "      # was: git diff | grep -qE x\n      - run: echo hi"))
        self.assertEqual(self.errs(selfcheck.check_workflows, d), [])

    def test_quiet_grep_in_composite_action_and_justfile(self):
        d = self.tree()
        os.makedirs(os.path.join(d, ".github/actions/a"))
        with open(os.path.join(d, ".github/actions/a/action.yml"), "w") as fh:
            fh.write("runs:\n  steps:\n    - run: |\n        git log | grep -Eq '^x'\n")
        e = self.errs(selfcheck.check_quiet_grep, d)
        self.assertTrue(any("action.yml:4" in x for x in e), e)
        os.remove(os.path.join(d, ".github/actions/a/action.yml"))
        with open(os.path.join(d, "justfile"), "w") as fh:
            fh.write("t:\n    # git log | grep -q x\n    git log | grep -q x\n")
        e = self.errs(selfcheck.check_quiet_grep, d)
        self.assertEqual(len(e), 1, e)
        self.assertIn("justfile:3", e[0])

    def test_checkout_pin_label_and_floating_tag(self):
        d = self.tree(GOOD_WF.replace("# v7.0.1", "# v4"))
        self.assertTrue(any("version label" in x for x in self.errs(selfcheck.check_checkout_pins, d)))
        d = self.tree(GOOD_WF.replace("3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1", "v7"))
        self.assertTrue(any("not a commit SHA" in x for x in self.errs(selfcheck.check_checkout_pins, d)))

    def test_deny_review_by_expiry(self):
        d = self.tree()
        self.assertEqual(self.errs(selfcheck.check_deny_review_by, d, datetime.date(2026, 12, 31)), [])
        self.assertTrue(any("EXPIRED" in x for x in self.errs(selfcheck.check_deny_review_by, d, datetime.date(2027, 1, 1))))
        d = self.tree(deny="nothing here\n")
        self.assertTrue(any("vacuous" in x for x in self.errs(selfcheck.check_deny_review_by, d, datetime.date(2026, 1, 1))))
        d = self.tree(deny='[advisories]\nignore = [{ id = "RUSTSEC-0000-0000", reason = "x" }]\n')
        self.assertTrue(any("vacuous" in x for x in self.errs(selfcheck.check_deny_review_by, d, datetime.date(2026, 1, 1))))
        d = self.tree(deny="[advisories]\nignore = []\n")
        self.assertEqual(self.errs(selfcheck.check_deny_review_by, d, datetime.date(2026, 1, 1)), [])

    def test_ci_code_recipe_must_run_nextest_and_doctests(self):
        good = "ci-code:\n    cargo clippy -p a\n    cargo nextest run --locked --profile ci-code -p a\n    cargo test --locked --doc -p a\n\nnext:\n    echo\n"
        d = self.tree()
        with open(os.path.join(d, "justfile"), "w") as fh:
            fh.write(good)
        self.assertEqual(self.errs(selfcheck.check_ci_code_recipe, d), [])
        for bad, needle in [
            (good.replace("cargo nextest run --locked --profile ci-code -p a", "cargo test -p a --no-fail-fast"), "nextest"),
            (good.replace("    cargo test --locked --doc -p a\n", ""), "--doc"),
            (good.replace("--profile ci-code", "--profile ci"), "nextest"),
        ]:
            with open(os.path.join(d, "justfile"), "w") as fh:
                fh.write(bad)
            e = self.errs(selfcheck.check_ci_code_recipe, d)
            self.assertTrue(any(needle in x for x in e), (needle, e))

    def test_unquoted_name_with_colon_space_is_flagged(self):
        d = self.tree(GOOD_WF.replace("name: x", "name: cargo-mutants (label: mutants)"))
        self.assertTrue(any("invalid YAML" in x for x in self.errs(selfcheck.check_yaml_names, d)))
        d = self.tree(GOOD_WF.replace("name: x", 'name: "cargo-mutants (label: mutants)"'))
        self.assertEqual(self.errs(selfcheck.check_yaml_names, d), [])
        d = self.tree()
        self.assertEqual(self.errs(selfcheck.check_yaml_names, d), [])

    def test_the_real_tree_passes(self):
        root = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
        self.assertEqual(selfcheck.run(root, datetime.date.today()), [])


if __name__ == "__main__":
    unittest.main(verbosity=1)
