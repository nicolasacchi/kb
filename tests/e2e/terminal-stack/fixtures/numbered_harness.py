#!/usr/bin/env python3
"""Synthetic numbered-line harness for the T25 end-to-end lane (stdlib only, content-free).

Prints `L000001`.. `L<seed+live>`, one per line:
  seed phase  : lines 1..SEED as fast as the pty takes them;
  gate        : with --gate FILE, waits until FILE exists (the test creates it once a
                viewer is live), so the live phase and every rebuild happen AFTER the join;
  live phase  : lines SEED+1..SEED+LIVE at --rate lines/s.
Every --rebuild-every live lines it does an omp-style rebuild: a plain `REBUILD-SOON`
marker line (outside the numbering) plus an `<out>/soon-<n>` file, a lead-in sleep, then ONE
synchronized frame `ESC[?2026h ESC[H ESC[2J ESC[3J` + a full reprint of ALL lines so far +
`ESC[?2026l`. --slow-rebuild-ms stretches the frame by sleeping between 200-line chunks.
With --queries it injects terminal queries (DA1, DA2, XTVERSION, CSI 5n/6n, DECRQM 2026,
CSI ? u, CSI 16t, OSC 10/11 ?, a DCS-tmux-wrapped DA1, and one split across writes) every 40
lines in both phases.
stdin is drained into <out>/stdin.log (echo and canonical mode off), so typed bytes and the
pane's own query replies are observable by the test.
When finished it writes <out>/expected.json {total, seed, live, queries_sent} atomically,
prints `DONE`, and sleeps so the pane stays alive.
With --flood-gate FILE it instead waits (after DONE) for FILE, writes --flood-bytes of filler
lines (`F` + x*78, never matching the numbering), clears screen and scrollback, prints a fresh
numbered block L000001..L<--flood-tail>, then `TAIL-DONE` (the aged-out-offset scenario: the
tap's ring wraps past a viewer that was away, and the new numbered block is the oracle's tail).

Usage: numbered_harness.py --seed N --live M --rebuild-every K --cols C --rows R --rate HZ
                           [--queries] [--gate FILE] [--lead-ms MS] [--slow-rebuild-ms MS]
                           [--flood-gate FILE --flood-bytes N --flood-tail M] --out DIR
"""
import argparse
import json
import os
import sys
import threading
import time

ESC = "\x1b"


def query_shapes():
    """(label, writes): each element of `writes` is one separate write()."""
    return [
        ("da1", [ESC + "[c"]),
        ("da2", [ESC + "[>c"]),
        ("xtversion", [ESC + "[>0q"]),
        ("dsr5", [ESC + "[5n"]),
        ("dsr6", [ESC + "[6n"]),
        ("decrqm2026", [ESC + "[?2026$p"]),
        ("kbdq", [ESC + "[?u"]),
        ("winops16", [ESC + "[16t"]),
        ("osc10", [ESC + "]10;?" + ESC + "\\"]),
        ("osc11", [ESC + "]11;?" + ESC + "\\"]),
        ("dcs_tmux_da1", [ESC + "Ptmux;" + ESC + ESC + "[c" + ESC + "\\"]),
        ("split_da1", [ESC + "[", "c"]),
    ]


class Out:
    def __init__(self):
        self.o = sys.stdout
        self.queries_sent = 0
        self._q = 0

    def write(self, s):
        self.o.write(s)

    def flush(self):
        self.o.flush()

    def query(self):
        shapes = query_shapes()
        _label, writes = shapes[self._q % len(shapes)]
        self._q += 1
        for w in writes:
            self.o.write(w)
            self.o.flush()
            if len(writes) > 1:
                time.sleep(0.005)
        self.queries_sent += 1


def drain_stdin(path):
    """Echo/canonical off, then append every received byte to `path`."""
    try:
        import termios

        attrs = termios.tcgetattr(0)
        attrs[3] &= ~(termios.ECHO | termios.ICANON)
        termios.tcsetattr(0, termios.TCSANOW, attrs)
    except Exception:
        pass

    def loop():
        with open(path, "ab", buffering=0) as f:
            while True:
                try:
                    data = os.read(0, 65536)
                except OSError:
                    return
                if not data:
                    return
                f.write(data)

    t = threading.Thread(target=loop, daemon=True)
    t.start()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=3000)
    ap.add_argument("--live", type=int, default=300)
    ap.add_argument("--rebuild-every", type=int, default=100)
    ap.add_argument("--cols", type=int, default=120)
    ap.add_argument("--rows", type=int, default=40)
    ap.add_argument("--rate", type=float, default=200.0)
    ap.add_argument("--queries", action="store_true")
    ap.add_argument("--gate", default=None)
    ap.add_argument("--lead-ms", type=int, default=1500)
    ap.add_argument("--slow-rebuild-ms", type=int, default=0)
    ap.add_argument("--flood-gate", default=None)
    ap.add_argument("--flood-bytes", type=int, default=0)
    ap.add_argument("--flood-tail", type=int, default=200)
    ap.add_argument("--out", required=True)
    a = ap.parse_args()

    os.makedirs(a.out, exist_ok=True)
    drain_stdin(os.path.join(a.out, "stdin.log"))
    out = Out()
    total = a.seed + a.live
    lines = []

    def emit(n):
        text = "L%06d" % n
        lines.append(text)
        out.write(text + "\n")

    def rebuild(count):
        out.write("REBUILD-SOON\n")
        out.flush()
        with open(os.path.join(a.out, "soon-%d" % count), "w") as f:
            f.write("1")
        time.sleep(a.lead_ms / 1000.0)
        out.write(ESC + "[?2026h" + ESC + "[H" + ESC + "[2J" + ESC + "[3J")
        chunk = 200
        for i in range(0, len(lines), chunk):
            out.write("\n".join(lines[i : i + chunk]) + "\n")
            out.flush()
            if a.slow_rebuild_ms:
                time.sleep(a.slow_rebuild_ms / 1000.0)
        out.write(ESC + "[?2026l")
        out.flush()

    # seed phase
    for n in range(1, a.seed + 1):
        emit(n)
        if a.queries and n % 40 == 0:
            out.query()
        if n % 50 == 0:
            out.flush()
    out.flush()

    if a.gate:
        deadline = time.time() + 180
        while not os.path.exists(a.gate) and time.time() < deadline:
            time.sleep(0.05)

    # live phase
    rebuilds = 0
    delay = 1.0 / a.rate if a.rate > 0 else 0.0
    for k in range(1, a.live + 1):
        emit(a.seed + k)
        if a.queries and k % 40 == 0:
            out.query()
        out.flush()
        if a.rebuild_every > 0 and k % a.rebuild_every == 0 and k < a.live:
            rebuilds += 1
            rebuild(rebuilds)
        if delay:
            time.sleep(delay)
    out.flush()

    tmp = os.path.join(a.out, "expected.json.tmp")
    with open(tmp, "w") as f:
        json.dump(
            {
                "total": total,
                "seed": a.seed,
                "live": a.live,
                "queries_sent": out.queries_sent,
                "rebuilds": rebuilds,
            },
            f,
        )
    os.replace(tmp, os.path.join(a.out, "expected.json"))
    out.write("DONE\n")
    out.flush()
    if a.flood_gate:
        deadline = time.time() + 900
        while not os.path.exists(a.flood_gate) and time.time() < deadline:
            time.sleep(0.05)
        filler = ("F" + "x" * 78 + "\n") * 800  # 64,000 bytes
        sent = 0
        while sent < a.flood_bytes:
            out.write(filler)
            sent += len(filler)
        out.flush()
        out.write(ESC + "[H" + ESC + "[2J" + ESC + "[3J")
        for n in range(1, a.flood_tail + 1):
            out.write("L%06d\n" % n)
        out.write("TAIL-DONE\n")
        out.flush()
        with open(os.path.join(a.out, "tail-done"), "w") as f:
            f.write("1")
    time.sleep(3600)


if __name__ == "__main__":
    main()
