"""Unit tests for mirror_standin.py: the dump checker and the stream itself."""

import fcntl
import importlib.util
import json
import os
import pty
import re
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unittest

SCRIPT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "mirror_standin.py")


def _load_module():
    spec = importlib.util.spec_from_file_location("mirror_standin", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


ms = _load_module()
ANSI_RE = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]")


def _run_cli(*args):
    return subprocess.run(
        [sys.executable, "-I", SCRIPT, *args],
        capture_output=True,
        text=True,
        timeout=60,
    )


def _write(path, text):
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)
    return path


def _numbered_text(numbers):
    return "".join("L%06d line %d\n" % (n, n) for n in numbers)


class TestMirrorStandin(unittest.TestCase):
    def test_truncated_dump_fails_the_incident(self):
        lines = ["L%06d line %d\n" % (n, n) for n in range(1, 2715)]
        result = ms.analyze_dump(lines, 3300)
        self.assertFalse(result["ok"])
        self.assertEqual(len(result["tail_missing"]), 586)
        self.assertTrue(
            any("truncated" in problem for problem in result["problems"]),
            result["problems"],
        )

    def test_cli_truncated_dump_exits_1_with_truncated_in_result(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = _write(
                os.path.join(tmp, "dump.txt"),
                _numbered_text(range(1, 2715)),
            )
            proc = _run_cli("--check-dump", path, "--expect-total", "3300")
            self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
            result_lines = [
                line for line in proc.stdout.splitlines() if "RESULT" in line
            ]
            self.assertTrue(result_lines, proc.stdout)
            self.assertIn("FAIL", result_lines[-1])
            self.assertIn("truncated", result_lines[-1])

    def test_complete_dump_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = _write(
                os.path.join(tmp, "dump.txt"),
                _numbered_text(range(1, 3301)),
            )
            proc = _run_cli("--check-dump", path, "--expect-total", "3300")
            self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
            self.assertIn("RESULT                 : PASS", proc.stdout)

    def test_expect_total_is_required(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = _write(
                os.path.join(tmp, "dump.txt"),
                _numbered_text(range(1, 101)),
            )
            missing_total = _run_cli("--check-dump", path)
            self.assertEqual(missing_total.returncode, 2)
            missing_dump = _run_cli("--expect-total", "3300")
            self.assertEqual(missing_dump.returncode, 2)
            unreadable = _run_cli(
                "--check-dump",
                os.path.join(tmp, "no-such-file.txt"),
                "--expect-total",
                "3300",
            )
            self.assertEqual(unreadable.returncode, 2)

    def test_overrun_fails(self):
        lines = ["L%06d line %d\n" % (n, n) for n in range(1, 3301)]
        result = ms.analyze_dump(lines, 3000)
        self.assertFalse(result["ok"])
        self.assertTrue(
            any("overrun" in problem for problem in result["problems"]),
            result["problems"],
        )

    def test_duplicates_fail(self):
        lines = [
            "L%06d line %d\n" % (n, n) for n in range(1, 101)
        ] + ["L%06d line 50\n" % 50]
        result = ms.analyze_dump(lines, 100)
        self.assertFalse(result["ok"])
        self.assertIn(50, result["duplicates"])

    def test_gap_in_middle_fails(self):
        lines = [
            "L%06d line %d\n" % (n, n)
            for n in range(1, 2001)
            if n != 1500
        ]
        result = ms.analyze_dump(lines, 2000)
        self.assertFalse(result["ok"])
        self.assertEqual(result["missing"], [1500])

    def test_out_of_order_fails(self):
        lines = ["L%06d line %d\n" % (n, n) for n in range(1, 101)]
        lines[9], lines[10] = lines[10], lines[9]
        result = ms.analyze_dump(lines, 100)
        self.assertFalse(result["ok"])
        self.assertFalse(result["in_order"])

    def test_empty_and_no_numbers_fail(self):
        for text in ("", "hello world\nno numbers here\n"):
            result = ms.analyze_dump(text.splitlines(True), 100)
            self.assertFalse(result["ok"])
            self.assertIn("no numbered lines", result["problems"])
            self.assertFalse(
                any("truncated" in problem for problem in result["problems"]),
                result["problems"],
            )
            with tempfile.TemporaryDirectory() as tmp:
                path = _write(os.path.join(tmp, "dump.txt"), text)
                proc = _run_cli("--check-dump", path, "--expect-total", "100")
                self.assertEqual(
                    proc.returncode, 1, proc.stdout + proc.stderr
                )

    def test_parsing_tolerates_ansi_and_editor_gutter_and_ignores_the_summary_block(self):
        lines = ["L%06d line %d\n" % (n, n) for n in range(1, 7)]
        lines.append("\x1b[2mL000007 [image 640x400, on web]\x1b[0m\r\n")
        lines.append(" L000008 line 8\r\n")
        lines.append("total numbered lines    : 3300  (L000001 .. L003300)\n")
        lines.append("seed lines              : 3000\n")
        lines.append("last line written     : L003300\n")
        result = ms.analyze_dump(lines, 8)
        self.assertEqual(result["numbers"], list(range(1, 9)))
        self.assertEqual(result["highest"], 8)
        self.assertTrue(result["ok"], result)

    def test_smoke_mode_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            out_path = os.path.join(tmp, "out.stream")
            report_path = os.path.join(tmp, "report.json")
            proc = _run_cli(
                "--smoke", "--out", out_path, "--report", report_path
            )
            self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
            with open(report_path, encoding="utf-8") as f:
                report = json.load(f)
            self.assertTrue(report["pass"], report)
            self.assertGreater(report["image_labels_in_file"], 0)
            self.assertEqual(report["placeholder_cells_in_file"], 0)

    def test_labels_and_lines_never_exceed_cols_minus_one(self):
        failures = []
        for cols in range(2, 121):
            for n in range(1, 201):
                raw = ms.line(n, cols, 50)
                stripped = ANSI_RE.sub("", raw)
                width = ms.cell_width(stripped)
                if width > cols - 1:
                    failures.append((cols, n, width, cols - 1))
        if failures:
            cols, n, width, limit = failures[0]
            self.fail(
                "%d failure(s); first at cols=%d n=%d width=%d limit=%d"
                % (len(failures), cols, n, width, limit)
            )


class TestPtyRun(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.ms = ms
        pid, fd = pty.fork()
        if pid == 0:
            try:
                os.execvp(
                    "python3",
                    [
                        "python3",
                        "-I",
                        SCRIPT,
                        "--rate",
                        "0",
                        "--live",
                        "400",
                        "--rebuild-every",
                        "100",
                    ],
                )
            finally:
                os._exit(127)
        try:
            fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 100, 0, 0))
        except OSError:
            pass
        buf = b""
        deadline = time.time() + 60
        try:
            while True:
                remaining = deadline - time.time()
                if remaining <= 0:
                    try:
                        os.kill(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    try:
                        os.waitpid(pid, 0)
                    except ChildProcessError:
                        pass
                    raise AssertionError("pty run timed out after 60s")
                ready, _, _ = select.select(
                    [fd], [], [], min(remaining, 2.0)
                )
                if not ready:
                    continue
                try:
                    data = os.read(fd, 65536)
                except OSError:
                    break
                if not data:
                    break
                buf += data
        finally:
            try:
                os.waitpid(pid, 0)
            except ChildProcessError:
                pass
            try:
                os.close(fd)
            except OSError:
                pass
        cls.text = buf.decode("utf-8", "replace")
        cls.body = cls.text[cls.text.index(cls.ms.ED3):]

    def test_ed3_ignoring_stream_still_fails_with_duplicates(self):
        plain = (
            ANSI_RE.sub("", self.body)
            .split("=== kb-term T4")[0]
            .replace("\r\n", "\n")
        )
        result = self.ms.analyze_dump(plain.splitlines(True), 3400)
        self.assertFalse(result["ok"], "duplicates=%d" % len(result["duplicates"]))
        self.assertGreater(len(result["duplicates"]), 0)

    def test_perfect_dump_passes_and_its_truncation_fails(self):
        last = self.body.split(self.ms.ED3)[-1]
        plain = (
            ANSI_RE.sub("", last)
            .split("=== kb-term T4")[0]
            .replace("\r\n", "\n")
        )
        with tempfile.TemporaryDirectory() as tmp:
            perfect_path = _write(
                os.path.join(tmp, "perfect.txt"), plain
            )
            proc = _run_cli(
                "--check-dump", perfect_path, "--expect-total", "3400"
            )
            self.assertEqual(
                proc.returncode, 0, proc.stdout + proc.stderr
            )
            self.assertIn("RESULT                 : PASS", proc.stdout)
            lines = plain.splitlines()
            self.assertGreater(len(lines), 586)
            truncated_text = "\n".join(lines[:-586]) + "\n"
            truncated_path = _write(
                os.path.join(tmp, "truncated.txt"), truncated_text
            )
            proc2 = _run_cli(
                "--check-dump", truncated_path, "--expect-total", "3400"
            )
            self.assertEqual(proc2.returncode, 1, proc2.stdout + proc2.stderr)
            self.assertIn("truncated", proc2.stdout)

    def test_pty_run_matches_the_design_counts(self):
        self.assertEqual(self.body.count(self.ms.ED3), 5)
        self.assertEqual(self.body.count(self.ms.SYNC_ON), 4)
        self.assertEqual(
            self.ms.verify_stream(self.body, 3400, 100)["problems"], []
        )
        self.assertIn("kb-term T4 stand-in finished", self.text)
        self.assertIn(
            "--check-dump /tmp/sb.txt --expect-total 3400", self.text
        )
        self.assertIn("last line written     : L003400", self.text)


if __name__ == "__main__":
    unittest.main()
