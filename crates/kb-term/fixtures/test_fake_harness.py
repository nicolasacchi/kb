#!/usr/bin/env python3
"""Unit tests for the synthetic fixture generators (stdlib only).

The generators are exercised through `sys.executable -I` exactly as the
term-tap lane and T3+ run them. Exit code is the gate: unittest writes no
JUnit, so this file is intentionally unwitnessed.
"""
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
HARNESS = os.path.join(HERE, "fake_harness.py")
SYNTH = os.path.join(HERE, "synth_omp_kitty_stream.py")


def run(script, *args):
    return subprocess.run([sys.executable, "-I", script, *args],
                          stdout=subprocess.PIPE, check=True,
                          timeout=60).stdout


class FakeHarness(unittest.TestCase):
    def test_same_args_identical_bytes(self):
        a = run(HARNESS, "--seed", "3", "--length", "20")
        b = run(HARNESS, "--seed", "3", "--length", "20")
        self.assertEqual(a, b)
        self.assertTrue(a)

    def test_different_seed_differs(self):
        a = run(HARNESS, "--seed", "3", "--length", "20")
        b = run(HARNESS, "--seed", "4", "--length", "20")
        self.assertNotEqual(a, b)

    def test_output_file_equals_stdout(self):
        with tempfile.TemporaryDirectory() as d:
            out_path = os.path.join(d, "stream.bin")
            subprocess.run([sys.executable, "-I", HARNESS, "--seed", "3",
                            "--length", "20", "-o", out_path], check=True,
                           timeout=60)
            with open(out_path, "rb") as fh:
                written = fh.read()
        self.assertEqual(written, run(HARNESS, "--seed", "3", "--length", "20"))

    def test_cols_rows_change_output(self):
        base = run(HARNESS, "--seed", "3", "--length", "20")
        wide = run(HARNESS, "--seed", "3", "--length", "20", "--cols", "140")
        tall = run(HARNESS, "--seed", "3", "--length", "20", "--rows", "50")
        self.assertNotEqual(base, wide)
        self.assertNotEqual(base, tall)

    def test_shaped_stream_reaches_the_end_marker(self):
        data = run(HARNESS, "--seed", "3", "--length", "20")
        self.assertIn(b"FAKE-HARNESS-END", data)

    def test_synth_stream_wrapped_and_plain(self):
        wrapped = run(SYNTH, "--cols", "90", "--rows", "19")
        plain = run(SYNTH, "--cols", "90", "--rows", "19", "--no-wrap")
        self.assertIn(b"\x1bPtmux;", wrapped)
        self.assertNotIn(b"\x1bPtmux;", plain)
        self.assertNotEqual(wrapped, plain)


if __name__ == "__main__":
    unittest.main(verbosity=1)
