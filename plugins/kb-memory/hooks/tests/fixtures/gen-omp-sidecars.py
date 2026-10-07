#!/usr/bin/env python3
"""Subagent sidecars in omp's REAL on-disk layout, for the segmented-capture tests.

  gen-omp-sidecars.py DIR COUNT [--start SEC] [--step SEC] [--records N]
                                [--no-timestamps] [--prefix P] [--blob BYTES]
                                [--base ISO-8601-UTC]

Writes DIR/<P><k>-Agent.jsonl. Like a real omp file each one starts with the
fixed-width TITLE SLOT ({"type":"title", ... "updatedAt": ..., no "timestamp"}),
then the session header (line 2, carrying the ISO "timestamp"), then a linked
chain of messages. Sidecar k starts SEC + k*STEP seconds after the generator
epoch of gen-omp-turns.py (2026-08-24T10:00:00Z). With --no-timestamps nothing in
the file carries a time (the header has none, the title slot has no updatedAt,
messages have none): the mtime is then the only clock - it is set to the same
instant. --blob pads every record's text (a realistic sidecar is hundreds of KB);
--base moves the epoch (the scale test's chain starts at 2025-08-24T08:00:00Z).
"""
import json, sys, os, datetime

def opt(a, name, default):
    return a[a.index(name) + 1] if name in a else default

BASE = datetime.datetime(2026, 8, 24, 10, 0, 0, tzinfo=datetime.timezone.utc)
if "--base" in sys.argv:
    BASE = datetime.datetime.strptime(sys.argv[sys.argv.index("--base") + 1], "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=datetime.timezone.utc)

def iso(sec):
    return (BASE + datetime.timedelta(seconds=sec)).strftime("%Y-%m-%dT%H:%M:%S.000Z")

def main():
    a = sys.argv[1:]
    d, count = a[0], int(a[1])
    start, step = int(opt(a, "--start", "5")), int(opt(a, "--step", "25"))
    nrec = int(opt(a, "--records", "3"))
    notime = "--no-timestamps" in a
    prefix = opt(a, "--prefix", "")
    blob = int(opt(a, "--blob", "0"))
    os.makedirs(d, exist_ok=True)
    for k in range(count):
        t = start + k * step
        slot = {"type": "title", "v": 1, "title": "sub %d" % k, "source": "auto"}
        if not notime:
            slot["updatedAt"] = iso(t)
        s = json.dumps(slot)
        slot_line = s + " " * max(0, 255 - len(s))
        hdr = {"type": "session", "version": 3, "id": "sub%05d-0000-4000-8000-000000000001" % k, "cwd": "/tmp/segtest"}
        if not notime:
            hdr["timestamp"] = iso(t)
        lines = [slot_line, json.dumps(hdr)]
        par = None
        for r in range(nrec):
            m = {"type": "message", "id": "s%dm%d" % (k, r), "parentId": par,
                 "message": {"role": "user" if r % 2 == 0 else "assistant", "content": [{"type": "text", "text": ("SUB-%d-%d " % (k, r)) + "x" * blob}]}}
            if r % 2 == 1:
                m["message"]["model"] = "prov/model-x"
            if not notime:
                m["timestamp"] = iso(t + 1 + r)
            par = m["id"]
            lines.append(json.dumps(m))
        p = os.path.join(d, "%s%d-Agent.jsonl" % (prefix, k))
        with open(p, "w") as f:
            f.write("\n".join(lines) + "\n")
        epoch = BASE.timestamp() + t
        os.utime(p, (epoch, epoch))

main()
