#!/usr/bin/env python3
"""Record per-glyph cursor advance under tmux (private socket), synthetic glyphs only.
Usage: record_tmux_widths.py OUT.json   (needs tmux on PATH; kills its private server afterwards)"""
import json, os, subprocess, sys, time
HERE = os.path.dirname(os.path.abspath(__file__))
SOCK = "kbt-twin"
G = json.load(open(os.path.join(HERE, "..", "glyphs.json"), encoding="utf-8"))["glyphs"]
ENV = dict(os.environ, LANG="C.UTF-8", LC_ALL="C.UTF-8", TERM="xterm-256color")

def tm(*a, check=True):
    return subprocess.run(["tmux", "-u", "-L", SOCK, *a], env=ENV, capture_output=True, text=True, check=check)

def measure(fmt, text):
    r = tm("new-window", "-d", "-P", "-F", "#{window_id}", "-e", "G=" + text,
           f"printf '{fmt}' \"$G\"; sleep 60")
    wid = r.stdout.strip()
    x = None
    for _ in range(50):
        time.sleep(0.04)
        x = tm("display", "-p", "-t", wid, "#{cursor_x}").stdout.strip()
        if x not in ("", "0") or fmt == "%s":
            if x != "":
                # allow a little settle time, then re-read
                time.sleep(0.03)
                x = tm("display", "-p", "-t", wid, "#{cursor_x}").stdout.strip()
                break
    tm("kill-window", "-t", wid, check=False)
    return int(x) if x else None

def main(out):
    version = subprocess.run(["tmux", "-V"], capture_output=True, text=True).stdout.strip()
    tm("kill-server", check=False)
    tm("new-session", "-d", "-x", "80", "-y", "24", "sleep 3600")
    try:
        res = {}
        for g in G:
            alone = measure("%s", g["text"])
            ctx = measure("a%sb", g["text"])
            res[g["id"]] = {"alone": alone, "ctx": None if ctx is None else ctx - 2}
    finally:
        tm("kill-server", check=False)
    json.dump({"schema": "kbt-widths/1", "tool": version, "method": "printf glyph in a pane; display -p #{cursor_x}; ctx = x after a+glyph+b minus 2",
               "widths": res}, open(out, "w"), indent=1)
    print(version, len(res), "glyphs", file=sys.stderr)

main(sys.argv[1])
