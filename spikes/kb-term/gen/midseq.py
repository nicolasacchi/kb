#!/usr/bin/env python3
"""Mid-sequence cut generator (standalone, stdlib only).

Writes bytes to stdout in three timed pieces so that a viewer joining at a
chosen moment lands INSIDE a CSI sequence (t=0..4s) or inside an OSC title
(t=4..8s). Piece boundaries are the interesting part; the text is filler.
Usage: midseq.py [--gap SECONDS]   (default 4)
"""
import os, sys, time, argparse
ap = argparse.ArgumentParser(); ap.add_argument("--gap", type=float, default=4.0)
a = ap.parse_args()
w = lambda b: os.write(1, b)
w(b"AAAA\r\nBBBB \x1b[1;3")                       # cut inside an SGR (CSI params pending)
time.sleep(a.gap)
w(b"1mRED-BOLD\x1b[0m tail\r\n\x1b]0;tit")        # now cut inside an OSC
time.sleep(a.gap)
w(b"le\x07after-osc\r\nMIDSEQ-END\r\n")
