#!/usr/bin/env python3
"""kb-term T4 stand-in: imitates the local mirror client's byte stream in a pane.

Zero dependencies (python3 stdlib). Synthetic content only.

Modes
  (default)        interactive: write to the terminal, print a checklist at exit
  --smoke          non-interactive: write the stream to --out FILE, verify it, exit 0/1
  --check-dump F   read a scrollback dump saved from the multiplexer (plain text)
                   and report duplicates / missing numbered lines; requires --expect-total N
"""
import argparse, json, os, re, shutil, sys, time, unicodedata

ESC = "\x1b"
ED2 = ESC + "[2J"
ED3 = ESC + "[3J"
HOME = ESC + "[H"
SYNC_ON = ESC + "[?2026h"
SYNC_OFF = ESC + "[?2026l"
DIM = ESC + "[2m"
RST = ESC + "[0m"
NL = "\r\n"
PLACEHOLDER = "\U0010EEEE"  # what omp's kitty placeholder cells look like
NUM_RE = re.compile(r"^(?:\x1b\[[0-9;?]*[A-Za-z])*L(\d{6})\b")

COLORS = [31, 32, 33, 34, 35, 36, 91, 92, 93, 94]
WIDE = ["日本語", "中文", "한글", "\U0001f600"]  # CJK, hangul, emoji


def cell_width(s):
    w = 0
    for ch in s:
        if unicodedata.combining(ch):
            continue
        w += 2 if unicodedata.east_asian_width(ch) in ("W", "F") else 1
    return w


def fit(s, cols):
    """Truncate s to at most cols-1 display cells (never touches the last column,
    so no pending-wrap ambiguity)."""
    limit = max(1, cols - 1)
    out, w = [], 0
    for ch in s:
        cw = cell_width(ch)
        if w + cw > limit:
            break
        out.append(ch)
        w += cw
    return "".join(out)


def label_for(n, cols, kind):
    """A placeholder image row rewritten as a width-safe dim label."""
    dims = [(640, 400), (1024, 768), (320, 200)][n % 3]
    text = "L%06d [image %dx%d, on web]" % (n, dims[0], dims[1])
    return DIM + fit(text, cols) + RST


def text_line(n, cols):
    c = COLORS[n % len(COLORS)]
    body = "L%06d " % n
    mid = "line %d lorem ipsum dolor sit amet" % n
    if n % 17 == 0:
        mid += " " + WIDE[(n // 17) % len(WIDE)]
    plain = fit(body + mid, cols)
    # keep the number uncoloured-prefixed so greps stay simple; colour the tail
    head, tail = plain[:7], plain[7:]
    return head + ESC + "[%dm" % c + tail + RST


def line(n, cols, image_every):
    if image_every and n % image_every == 0:
        return label_for(n, cols, "image")
    return text_line(n, cols)


class Emitter:
    def __init__(self, sink, cols, image_every):
        self.sink, self.cols, self.image_every = sink, cols, image_every
        self.n = 0
        self.rebuilds = 0
        self.ed3 = 0

    def w(self, s):
        self.sink.write(s)

    def seed(self, count):
        self.w(ED3)
        self.ed3 += 1
        for _ in range(count):
            self.n += 1
            self.w(line(self.n, self.cols, self.image_every) + NL)
        self.sink.flush()

    def live(self, count, rate, rebuild_every):
        for i in range(1, count + 1):
            self.n += 1
            self.w(line(self.n, self.cols, self.image_every) + NL)
            self.sink.flush()
            if rebuild_every and i % rebuild_every == 0:
                self.rebuild()
            if rate > 0:
                time.sleep(1.0 / rate)

    def rebuild(self):
        """omp-like: ED2+ED3 then full reprint inside one ?2026 frame."""
        self.w(SYNC_ON + HOME + ED2 + ED3)
        self.ed3 += 1
        for k in range(1, self.n + 1):
            self.w(line(k, self.cols, self.image_every) + NL)
        self.w(SYNC_OFF)
        self.rebuilds += 1
        self.sink.flush()


def verify_stream(text, expect_total, cols):
    """Parse the produced byte stream the way a dumb terminal model would: every
    ED3 resets 'history'; after the final ED3 the numbered lines must be exactly
    1..expect_total, once each, in order, none wider than cols-1 cells."""
    problems = []
    ed3_count = text.count(ED3)
    if text.count(SYNC_ON) != text.count(SYNC_OFF):
        problems.append("unbalanced ?2026")
    segs = text.split(ED3)
    for si, seg in enumerate(segs[1:], start=1):
        nums = []
        for raw in seg.split(NL):
            m = NUM_RE.match(raw)
            if m:
                nums.append(int(m.group(1)))
                vis = re.sub(r"\x1b\[[0-9;?]*[A-Za-z]", "", raw)
                if cell_width(vis) > cols - 1:
                    problems.append("line %s wider than %d cells" % (m.group(1), cols - 1))
        if nums != list(range(1, len(nums) + 1)):
            problems.append("segment %d not 1..%d consecutive" % (si, len(nums)))
        if si == len(segs) - 1 and len(nums) != expect_total:
            problems.append("last segment has %d lines, expected %d" % (len(nums), expect_total))
    return {"ed3_count": ed3_count, "segments": len(segs) - 1, "problems": problems}


def analyze_dump(lines, expect_total):
    """Pure. lines: iterable of str. Returns a dict with keys:
    numbers (list[int], file order), highest (int, 0 if none), duplicates (sorted list),
    missing (list, 1..highest), in_order (bool), expect_total (int),
    tail_missing (list: highest+1..expect_total when highest < expect_total, else []),
    overrun (sorted unique list of numbers > expect_total), problems (list[str]), ok (bool)."""
    numbers = []
    for raw in lines:
        m = NUM_RE.match(raw.rstrip("\r\n").lstrip())
        if m:
            numbers.append(int(m.group(1)))
    seen = set()
    dup_set = set()
    for n in numbers:
        if n in seen:
            dup_set.add(n)
        seen.add(n)
    duplicates = sorted(dup_set)
    highest = max(seen) if seen else 0
    missing = [n for n in range(1, highest + 1) if n not in seen]
    in_order = numbers == sorted(numbers)
    overrun = sorted(n for n in seen if n > expect_total)
    if highest < expect_total:
        tail_missing = list(range(highest + 1, expect_total + 1))
    else:
        tail_missing = []
    problems = []
    if not numbers:
        problems.append("no numbered lines")
    else:
        if duplicates:
            problems.append("duplicates: %s" % duplicates[:10])
        if missing:
            problems.append("missing in 1..%d: %d" % (highest, len(missing)))
        if not in_order:
            problems.append("out of order")
        if overrun:
            problems.append("overrun: numbers beyond the expected total")
        if highest < expect_total:
            problems.append("truncated: highest L%06d < expected L%06d (%d tail lines absent: L%06d..L%06d)" % (highest, expect_total, len(tail_missing), tail_missing[0], tail_missing[-1]))
    ok = not problems
    return {
        "numbers": numbers,
        "highest": highest,
        "duplicates": duplicates,
        "missing": missing,
        "in_order": in_order,
        "expect_total": expect_total,
        "tail_missing": tail_missing,
        "overrun": overrun,
        "problems": problems,
        "ok": ok,
    }


def check_dump(path, expect_total, out=sys.stdout):
    """Read path (utf-8, errors='replace'), call analyze_dump, print the report, return 0 or 1."""
    with open(path, encoding="utf-8", errors="replace") as f:
        lines = list(f)
    result = analyze_dump(lines, expect_total)
    numbers = result["numbers"]
    highest = result["highest"]
    duplicates = result["duplicates"]
    missing = result["missing"]
    in_order = result["in_order"]
    tail_missing = result["tail_missing"]
    problems = result["problems"]
    ok = result["ok"]
    print("numbered lines in dump : %d" % len(numbers), file=out)
    if numbers:
        print("first / last           : L%06d / L%06d" % (numbers[0], numbers[-1]), file=out)
    else:
        print("first / last           : -", file=out)
    print("highest number         : %d" % highest, file=out)
    print("expected total        : %d" % expect_total, file=out)
    print("duplicates             : %d %s" % (len(duplicates), duplicates[:10]), file=out)
    print("missing (1..highest)   : %d %s" % (len(missing), missing[:10]), file=out)
    print("in order               : %s" % in_order, file=out)
    if numbers and highest < expect_total:
        print("tail                   : truncated: highest L%06d < expected L%06d (%d tail lines absent: L%06d..L%06d)" % (highest, expect_total, len(tail_missing), tail_missing[0], tail_missing[-1]), file=out)
    else:
        print("tail                   : OK", file=out)
    if ok:
        print("RESULT                 : PASS", file=out)
    else:
        print("RESULT                 : FAIL (%s)" % "; ".join(problems), file=out)
    return 0 if ok else 1


def summary(em, args, cols, rows):
    return """
=== kb-term T4 stand-in finished ===
pane size               : %dx%d
seed lines              : %d
live lines              : %d
total numbered lines    : %d  (L000001 .. L%06d)
last line written     : L%06d
rebuilds (ED2+ED3+reprint, ?2026 framed): %d
ED3 sequences sent      : %d
image labels            : every %d-th line (dim "[image WxH, on web]")

CHECK NOW (details in crates/kb-term/tools/README.md):
 1. Scroll up: L000001 is reachable at the very top (seed is in scrollback).
 2. Scroll through all of it: each number appears ONCE (no duplicates after the
    last rebuild), and none is missing.
    Faster: dump the scrollback to a file and run
      python3 mirror_standin.py --check-dump /tmp/sb.txt --expect-total %d
 3. Image labels are dim, on one row each, no wrapped or shifted rows.
 4. Repeat after raising scroll_buffer_size (see README).
""" % (cols, rows, args.seed, em.n - args.seed, em.n, em.n, em.n, em.rebuilds, em.ed3, args.image_every, em.n)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--seed", type=int, default=3000, help="history lines in the SEED (default 3000)")
    ap.add_argument("--live", type=int, default=300, help="live lines streamed after the seed")
    ap.add_argument("--rate", type=float, default=20.0, help="live lines per second (0 = as fast as possible)")
    ap.add_argument("--rebuild-every", type=int, default=100, help="omp-like rebuild after every N live lines (0 = never)")
    ap.add_argument("--image-every", type=int, default=50, help="every Nth line is an image label (0 = none)")
    ap.add_argument("--cols", type=int, default=0, help="override width (default: terminal width)")
    ap.add_argument("--smoke", action="store_true", help="non-interactive self-test, writes to --out")
    ap.add_argument("--out", default="mirror_standin.out", help="--smoke output file")
    ap.add_argument("--report", default="", help="--smoke JSON report path")
    ap.add_argument("--check-dump", metavar="FILE", help="check a saved scrollback dump")
    ap.add_argument("--expect-total", type=int, default=None, help="total numbered lines the run wrote (required with --check-dump)")
    args = ap.parse_args()

    if args.check_dump and args.expect_total is None:
        ap.error("a dump can only be judged complete against the total the run wrote (the summary's 'total numbered lines'); pass --expect-total N with --check-dump")
    if args.expect_total is not None and not args.check_dump:
        ap.error("--expect-total requires --check-dump FILE")
    if args.expect_total is not None and args.expect_total < 1:
        ap.error("--expect-total must be >= 1")

    if args.check_dump:
        try:
            return check_dump(args.check_dump, args.expect_total)
        except OSError as e:
            print("error: cannot read %s: %s" % (args.check_dump, e), file=sys.stderr)
            return 2

    if args.smoke:
        cols, rows = args.cols or 40, 10
        args.seed, args.live, args.rate = 30, 20, 0
        args.rebuild_every, args.image_every = 8, 7
        with open(args.out, "w", encoding="utf-8", newline="") as f:
            em = Emitter(f, cols, args.image_every)
            em.seed(args.seed)
            em.live(args.live, 0, args.rebuild_every)
        text = open(args.out, encoding="utf-8", newline="").read()
        v = verify_stream(text, em.n, cols)
        expect_rebuilds = args.live // args.rebuild_every
        lines = text.count(NL)
        # file line count = seed + live + sum of reprints
        expect_lines = args.seed + args.live + sum(args.seed + i * args.rebuild_every for i in range(1, expect_rebuilds + 1))
        res = {
            "mode": "smoke", "cols": cols, "seed": args.seed, "live": args.live,
            "total_numbered": em.n, "rebuilds": em.rebuilds, "expected_rebuilds": expect_rebuilds,
            "ed3_sent": em.ed3, "ed3_in_file": v["ed3_count"], "file_lines": lines,
            "expected_file_lines": expect_lines, "image_labels_in_file": text.count("on web]"),
            "placeholder_cells_in_file": text.count(PLACEHOLDER), "problems": v["problems"],
        }
        if em.rebuilds != expect_rebuilds: res["problems"].append("rebuild count")
        if lines != expect_lines: res["problems"].append("file line count %d != %d" % (lines, expect_lines))
        if v["ed3_count"] != em.ed3 or em.ed3 != expect_rebuilds + 1: res["problems"].append("ED3 count")
        if res["placeholder_cells_in_file"]: res["problems"].append("raw placeholder cells leaked")
        if not res["image_labels_in_file"]: res["problems"].append("no image labels")
        res["pass"] = not res["problems"]
        out = json.dumps(res, indent=2)
        print(out)
        if args.report:
            os.makedirs(os.path.dirname(args.report) or ".", exist_ok=True)
            open(args.report, "w").write(out + "\n")
        return 0 if res["pass"] else 1

    size = shutil.get_terminal_size((80, 24))
    cols, rows = args.cols or size.columns, size.lines
    out = sys.stdout
    print("kb-term T4 stand-in: %dx%d, seed %d, live %d @ %g/s, rebuild every %d. Starting in 2s..." %
          (cols, rows, args.seed, args.live, args.rate, args.rebuild_every))
    time.sleep(2)
    em = Emitter(out, cols, args.image_every)
    try:
        em.seed(args.seed)
        em.live(args.live, args.rate, args.rebuild_every)
    except KeyboardInterrupt:
        pass
    out.write(summary(em, args, cols, rows).replace("\n", NL))
    out.flush()
    return 0


if __name__ == "__main__":
    sys.exit(main())
