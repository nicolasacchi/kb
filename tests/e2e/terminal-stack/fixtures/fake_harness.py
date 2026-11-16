#!/usr/bin/env python3
"""Deterministic, seeded, content-free generator of a harness-like terminal byte stream.

Covers: ED2+ED3 inside ?2026 synchronized frames, DECAWM (?7) toggles, alt screen (1049),
DECSTBM scroll regions, SGR (16/256/truecolor, bold/underline styles), wide CJK, emoji
(VS16, ZWJ, skin tones, post-Unicode-11), pending-wrap edge cases, OSC 8 hyperlinks,
cursor save/restore (ESC 7/8 and CSI s/u), DCS-tmux-wrapped kitty APC transmit + virtual
placement + U+10EEEE placeholder rows (encoding reused from synth_omp_kitty_stream.py),
and long plain line runs.

Usage: fake_harness.py [--cols 100] [--rows 30] [--seed 1] [--length N] [-o FILE]
  --length = approximate number of "scenes" to emit (default 60). Same args => identical bytes.
Writes to stdout (binary) unless -o is given. Stdlib only.
"""
import sys, os, random, argparse
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import synth_omp_kitty_stream as K

ESC = "\x1b"
WORDS = "alpha beta gamma delta kappa sigma omega lorem ipsum dolor amet node pane frame cell".split()
CJK = "漢字仮名交じり文表示幅確認用語彙試験列"
EMOJI = [
    "❤️",                    # VS16
    "☺️",
    "\U0001F468‍\U0001F469‍\U0001F467",   # ZWJ family
    "\U0001F469‍\U0001F4BB",      # ZWJ technologist
    "\U0001F44D\U0001F3FD",            # skin tone
    "\U0001F44B\U0001F3FF",
    "\U0001F600", "\U0001F680",
    "\U0001FAE0", "\U0001FAF6", "\U0001F6DD", "\U0001FA77",   # Unicode 14/15 era
    "\U0001F1EE\U0001F1F9",            # regional-indicator flag
]

def words(r, n):
    return " ".join(r.choice(WORDS) for _ in range(n))

def sgr_line(r, cols):
    kind = r.choice(["16", "256", "true", "style"])
    if kind == "16":
        p = f"{ESC}[{r.choice([30,31,32,33,34,35,36,37,90,91,92,97])};{r.choice([40,41,42,44,49,100,101])}m"
    elif kind == "256":
        p = f"{ESC}[38;5;{r.randrange(256)};48;5;{r.randrange(256)}m"
    elif kind == "true":
        p = f"{ESC}[38;2;{r.randrange(256)};{r.randrange(256)};{r.randrange(256)}m{ESC}[48;2;{r.randrange(64)};{r.randrange(64)};{r.randrange(64)}m"
    else:
        st = r.choice(["1", "2", "3", "4", "4:2", "4:3", "4:4", "4:5", "7", "9", "53", "1;4", "1;3;4:3"])
        p = f"{ESC}[{st}m" + (f"{ESC}[58:2::{r.randrange(256)}:{r.randrange(256)}:{r.randrange(256)}m" if r.random() < .4 else "")
    return p + words(r, r.randint(2, max(2, cols // 9)))[: cols - 2] + f"{ESC}[0m\r\n"

def scene_plain(r, c, rows):
    return "".join(f"line {r.randrange(10**6):06d} {words(r, r.randint(1, c // 7))}"[: c - 1] + "\r\n" for _ in range(r.randint(5, 40)))

def scene_sgr(r, c, rows):
    return "".join(sgr_line(r, c) for _ in range(r.randint(3, 12)))

def scene_wide(r, c, rows):
    out = []
    for _ in range(r.randint(2, 8)):
        s = "".join(r.choice(CJK) for _ in range(r.randint(2, c // 4)))
        out.append(f"{s} {r.choice(EMOJI)} {r.choice(EMOJI)} x{r.choice(EMOJI)}y\r\n")
    # CJK exactly at the right margin (wide char cannot fit in last column)
    out.append("a" * (c - 1) + r.choice(CJK) + "\r\n")
    return "".join(out)

def scene_pending_wrap(r, c, rows):
    # fill exactly c columns (cursor in pending-wrap state) then do various things
    o = "x" * c
    kind = r.randrange(4)
    if kind == 0: return o + "\r\n" + "next\r\n"                       # CR LF after exact fill
    if kind == 1: return o + f"{ESC}[1D" + "!" + "\r\n"                  # CUB clears pending wrap
    if kind == 2: return o + "y" + "\r\n"                              # wraps
    return f"{ESC}[?7l" + o + "OVERWRITE-LAST" + f"{ESC}[?7h" + "\r\n"  # DECAWM off: last cell overwritten

def scene_decawm(r, c, rows):
    return f"{ESC}[?7l" + "z" * (c + r.randint(1, 20)) + f"{ESC}[?7h\r\n" + "w" * (c + 5) + "\r\n"

def scene_region(r, c, rows):
    top = r.randint(2, rows // 3); bot = r.randint(rows // 2, rows - 2)
    o = f"{ESC}[{top};{bot}r{ESC}[{top};1H"
    o += "".join(f"region row {i} {words(r, 3)}\r\n" for i in range(bot - top + r.randint(3, 15)))
    o += f"{ESC}[r{ESC}[{rows};1H\r\n"     # reset margins, go to bottom
    return o

def scene_alt(r, c, rows):
    o = f"{ESC}[?1049h{ESC}[2J{ESC}[H"
    for i in range(r.randint(3, rows - 2)):
        o += f"{ESC}[{i + 1};1H{ESC}[7m ALT {i} {ESC}[0m {words(r, 4)}"
    return o + f"{ESC}[?1049l"

def scene_osc8(r, c, rows):
    i = r.randrange(1000)
    return (f"{ESC}]8;id=l{i};https://example.invalid/{i}{ESC}\\link-{i}{ESC}]8;;{ESC}\\ and "
            f"{ESC}]8;;https://example.invalid/b{i}\x07bel-link{ESC}]8;;\x07 end\r\n")

def scene_cursor(r, c, rows):
    return (f"{ESC}7{ESC}[5;10H{ESC}[1mSAVED-ESC7{ESC}[0m{ESC}8restored\r\n"
            f"{ESC}[s{ESC}[3;3Hcsi-s{ESC}[urest\r\n")

def scene_frame(r, c, rows):
    # synchronized update: full clear of screen (ED2) and scrollback (ED3), redraw
    o = f"{ESC}[?2026h{ESC}[H{ESC}[2J{ESC}[3J"
    o += f"{ESC}[1;1H{ESC}[1m== frame {r.randrange(10**4)} =={ESC}[0m"
    for i in range(2, r.randint(5, rows)):
        o += f"{ESC}[{i};1H{words(r, r.randint(2, c // 8))}"
    return o + f"{ESC}[?2026l\r\n"

def scene_frame_noclear(r, c, rows):
    o = f"{ESC}[?2026h{ESC}[H"
    for i in range(1, r.randint(4, rows)):
        o += f"{ESC}[{i};1H{ESC}[2K{words(r, r.randint(2, c // 8))}"
    return o + f"{ESC}[?2026l"

def scene_kitty(r, c, rows):
    i = r.randrange(1, 1 << 24)
    cols = r.randint(8, min(60, c - 2)); nrows = r.randint(3, 12)
    px = (r.randint(40, 160), r.randint(30, 100))
    wrap = r.random() < .85
    b = bytearray()
    b += K.wrap(K.apc(b"a=d,d=A,q=2"), wrap)
    b += K.transmit(i, K.png(*px), wrap)
    b += K.wrap(K.apc(b"a=p,U=1,q=2,i=%d,p=%d,c=%d,r=%d" % (i, i, cols, nrows)), wrap)
    b += K.rows(i, cols, nrows)
    return bytes(b)

SCENES = [(scene_plain, 8), (scene_sgr, 5), (scene_wide, 4), (scene_pending_wrap, 3), (scene_decawm, 2),
          (scene_region, 2), (scene_alt, 2), (scene_osc8, 2), (scene_cursor, 2), (scene_frame, 3),
          (scene_frame_noclear, 3), (scene_kitty, 2)]

def generate(cols, rows, seed, length):
    r = random.Random(seed)
    pool = [f for f, w in SCENES for _ in range(w)]
    out = bytearray(f"{ESC}[0m{ESC}[2J{ESC}[H".encode())
    # guarantee every scene kind appears at least once, in seeded order, then random
    first = [f for f, _ in SCENES]; r.shuffle(first)
    seq = (first + [r.choice(pool) for _ in range(max(0, length - len(first)))])[: max(length, len(first))]
    for f in seq:
        s = f(r, cols, rows)
        out += s if isinstance(s, bytes) else s.encode("utf-8")
        if r.random() < .15: out += scene_plain(r, cols, rows).encode()   # long plain run between scenes
    out += f"{ESC}[0m{ESC}[r{ESC}[?7h\r\nFAKE-HARNESS-END\r\n".encode()
    return bytes(out)

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cols", type=int, default=100); ap.add_argument("--rows", type=int, default=30)
    ap.add_argument("--seed", type=int, default=1); ap.add_argument("--length", type=int, default=60)
    ap.add_argument("-o", "--output")
    a = ap.parse_args()
    data = generate(a.cols, a.rows, a.seed, a.length)
    if a.output:
        open(a.output, "wb").write(data)
    else:
        sys.stdout.buffer.write(data)

if __name__ == "__main__":
    main()
