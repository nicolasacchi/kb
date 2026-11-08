#!/usr/bin/env python3
"""pane_probe.py -- synthetic pane fixture for the term-int lane (stdlib only).

argv: mode, log path. Each mode runs as the pane (a raw tty): it logs
received bytes hex-encoded, one flushed line per read, to the log path.

Modes:
  query       send DA1, XTVERSION, OSC 11 ?, CSI 14t/16t/18t; log raw replies
  raw         send CSI > 4;2 m first, then log input
  altmouse    enter alt screen, send ?1000h ?1006h, log input
  paste-on    send ?2004h, log input
  paste-off   (no bracketed paste), log input
  tick        print a line every 100ms for 1.5s (timestamped), then log input
  flood       write 20MB then exit

Run with `python3 -I`. Synthetic content only; no real session data.
"""
import os
import select
import sys
import time
import tty

ESC = b"\x1b"
LOG = None


def log(line):
    LOG.write(line + "\n")
    LOG.flush()


def send(data):
    os.write(1, data)


def read_loop(duration):
    end = time.time() + duration
    while time.time() < end:
        ready, _, _ = select.select([0], [], [], 0.05)
        if not ready:
            continue
        try:
            data = os.read(0, 65536)
        except OSError:
            return
        if not data:
            return
        log(data.hex())


def main():
    global LOG
    mode, path = sys.argv[1], sys.argv[2]
    tty.setraw(0)
    LOG = open(path, "w")
    if mode == "query":
        # Repeat the query burst: tmux adopts the anchor's colour memory
        # and client pixel size asynchronously after attach/resize, so an
        # early burst can still be answered with tmux's synthetic
        # fallbacks. Later bursts (after the grid is taken) carry the
        # responder's answers. Read continuously between bursts so every
        # reply lands in the log.
        query = (
            ESC + b"[c"
            + ESC + b"[>q"
            + ESC + b"]11;?" + b"\x07"
            + ESC + b"[14t"
            + ESC + b"[16t"
            + ESC + b"[18t"
        )
        for _ in range(12):
            send(query)
            read_loop(0.5)
        send(ESC + b"[>4;2m")
        read_loop(3.0)
    elif mode == "raw":
        # Extended-key announcement first, then log input verbatim.
        send(ESC + b"[>4;2m")
        read_loop(3.0)
    elif mode == "altmouse":
        send(ESC + b"[?1049h" + ESC + b"[?1000h" + ESC + b"[?1006h")
        read_loop(3.0)
    elif mode == "paste-on":
        send(ESC + b"[?2004h")
        read_loop(3.0)
    elif mode == "paste-off":
        read_loop(3.0)
    elif mode == "tick":
        end = time.time() + 1.5
        while time.time() < end:
            line = "tick %.6f\n" % time.time()
            send(line.encode())
            time.sleep(0.1)
        # Stop timestamp (wall clock, the same source the Enter test uses).
        log("stop %.6f" % time.time())
        read_loop(8.0)
    elif mode == "flood":
        chunk = b"x" * 65536
        for _ in range(320):  # 320 * 64KiB = 20MB
            try:
                os.write(1, chunk)
            except OSError:
                return
    else:
        sys.exit("unknown mode %r" % mode)


if __name__ == "__main__":
    main()
