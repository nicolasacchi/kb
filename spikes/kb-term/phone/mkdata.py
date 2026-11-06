#!/usr/bin/env python3
"""Build the synthetic data files for the phone spike (stdlib only, deterministic)."""
import os, re, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
GEN = os.path.join(HERE, "..", "gen", "fake_harness.py")
WORDS = "alpha beta gamma delta kappa sigma omega lorem ipsum dolor amet node pane frame cell".split()


def harness(cols, rows, seed, length):
    return subprocess.run(
        [sys.executable, "-I", GEN, "--cols", str(cols), "--rows", str(rows),
         "--seed", str(seed), "--length", str(length)],
        capture_output=True, check=True, timeout=300).stdout


def main(outdir):
    os.makedirs(outdir, exist_ok=True)
    # seed: what a twin would serialize for a late joiner = >=20,000 lines of scrollback.
    # ED3 and alt-screen switches are stripped so the history survives; a plain tail guarantees the count.
    h = harness(120, 30, 11, 600)
    h = re.sub(rb"\x1b\[3J", b"", h)
    h = re.sub(rb"\x1b\[\?1049[hl]", b"", h)
    tail = bytearray()
    for i in range(20100):
        w = " ".join(WORDS[(i * 7 + k * 3) % len(WORDS)] for k in range(6))
        tail += f"\x1b[3{1 + i % 6}m{i:05d}\x1b[0m {w}\r\n".encode()
    seed = h + b"\r\n" + bytes(tail)
    open(os.path.join(outdir, "seed.bin"), "wb").write(seed)
    # resume stream: 100x30 harness stream, seed 7
    stream = harness(100, 30, 7, 300)
    open(os.path.join(outdir, "stream.bin"), "wb").write(stream)
    # same stream without ED3 / alt-screen so the comparison covers thousands of scrollback lines, not 41
    stream_strip = re.sub(rb"\x1b\[\?1049[hl]", b"", re.sub(rb"\x1b\[3J", b"", stream))
    open(os.path.join(outdir, "stream-strip.bin"), "wb").write(stream_strip)
    # readability: 120 col screen
    wide = harness(120, 30, 5, 40)
    open(os.path.join(outdir, "wide.bin"), "wb").write(wide)
    for n, b in (("seed", seed), ("stream", stream), ("stream-strip", stream_strip), ("wide", wide)):
        nl = b.count(b"\n")
        print(f"{n}.bin bytes={len(b)} newlines={nl}")


if __name__ == "__main__":
    main(sys.argv[1])
