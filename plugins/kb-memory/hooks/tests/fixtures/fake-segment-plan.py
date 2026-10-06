#!/usr/bin/env python3
"""Offline stand-in for `kb sessions segment-plan` (v0.46 SEG-B), used ONLY by
test-capture-omp-segments.sh when the kb on PATH has no such verb (a kb built
before v0.46). CI runs the same tests against the real planner.

It is deliberately simpler than the real one - same leaf-chain rules as
TRANSLATE, a cut at the first legal user boundary once a part holds >= target
raw bytes (or at any entry once it holds >= 2*target), frozen = every part but
the last - but it emits the same JSON fields and the same `--emit` line order
(header, title slot, then the part's chain lines in file order).
"""
import json, sys

def parse_args(argv):
    o = {"emit": None, "target": 16 * 1024 * 1024, "no_write": False}
    i = 0
    while i < len(argv):
        x = argv[i]
        if x in ("--source", "--state", "--adapter-ver"):
            o[x[2:].replace("-", "_")] = argv[i + 1]; i += 2
        elif x == "--target-bytes":
            o["target"] = int(argv[i + 1]); i += 2
        elif x == "--emit":
            o["emit"] = int(argv[i + 1]); i += 2
        elif x == "--no-write":
            o["no_write"] = True; i += 1
        else:
            i += 1
    return o

def is_user(e):
    if e.get("type") != "message":
        return False
    m = e.get("message") or {}
    if m.get("role") != "user":
        return False
    parts = [p.get("text") for p in (m.get("content") or []) if isinstance(p, dict) and p.get("type") == "text"]
    return "\n".join(x if isinstance(x, str) else json.dumps(x) for x in parts) != ""

def read_obj(src, e):
    with open(src, "rb") as f:
        f.seek(e["off"])
        try:
            return json.loads(f.read(e["len"]))
        except Exception:
            return {}

def load_index(src, state, write):
    """The per-line index, incremental through `state` (pickle) like the real
    planner's checkpoint: only bytes after `indexed` are scanned."""
    import os, pickle
    size = os.path.getsize(src)
    ents, indexed = [], 0
    if state and os.path.exists(state):
        try:
            st = pickle.load(open(state, "rb"))
            if st["size"] <= size and st["indexed"] <= size:
                ents, indexed = st["ents"], st["indexed"]
        except Exception:
            ents, indexed = [], 0
    off = indexed
    with open(src, "rb") as f:
        f.seek(indexed)
        for raw in f:
            if not raw.endswith(b"\n"):
                break  # a torn tail is planned over by the real one; here: skipped
            try:
                obj = json.loads(raw)
            except Exception:
                obj = None
            if isinstance(obj, dict):
                ents.append({"off": off, "len": len(raw), "type": obj.get("type"), "id": obj.get("id"),
                             "parent": obj.get("parentId"), "user": is_user(obj),
                             "model": obj.get("model") if obj.get("type") == "model_change" else None})
            off += len(raw)
    if write and state:
        tmp = state + ".tmp"
        pickle.dump({"size": size, "indexed": off, "ents": ents}, open(tmp, "wb"))
        os.replace(tmp, state)
    return ents

def main():
    if "--help" in sys.argv[1:]:
        print("fake segment-plan"); return
    o = parse_args(sys.argv[1:])
    src = o["source"]
    ents = load_index(src, o.get("state"), not o["no_write"])
    hdr = next((e for e in ents if e["type"] == "session"), None)
    tslot = next((e for e in ents if e["type"] == "title"), None)
    es = [e for e in ents if e["type"] != "title"]
    chain = []
    cur = es[-1]["id"] if es else None
    for e in reversed(es):
        if cur is not None and e["id"] == cur:
            chain.append(e)
            cur = e["parent"]
    chain.reverse()
    rb = max([i for i, e in enumerate(chain) if e["type"] == "reset_boundary"], default=-1)
    live = chain[rb + 1:]
    cuts, acc, start = [], 0, 0
    for i, e in enumerate(live):
        if i > start and ((e["user"] and acc >= o["target"]) or acc >= 2 * o["target"]):
            cuts.append((start, i)); start, acc = i, 0
        acc += e["len"]
    if live:
        cuts.append((start, len(live)))
    parts = []
    for k, (lo, hi) in enumerate(cuts, 1):
        seg = live[lo:hi]
        dm = "omp"
        for e in live[:lo + 1]:
            if e["type"] == "model_change":
                dm = e["model"] or "omp"
        parts.append({"idx": k, "state": "live" if k == len(cuts) else "frozen", "reused": False,
                      "first_id": seg[0]["id"], "last_id": seg[-1]["id"],
                      "n_entries": len(seg), "start_offset": seg[0]["off"],
                      "end_offset": seg[-1]["off"] + seg[-1]["len"], "bytes": sum(e["len"] for e in seg),
                      "cut": "tail" if k == len(cuts) else "user", "dmodel": dm, "_seg": seg})
    if o["emit"] is not None:
        k = o["emit"]
        if not 1 <= k <= len(parts):
            sys.exit("no part %d" % k)
        seg = parts[k - 1]["_seg"]
        ids = {e["off"] for e in seg}
        out = sys.stdout.buffer
        with open(src, "rb") as f:
            def raw(e):
                f.seek(e["off"]); b = f.read(e["len"])
                return b if b.endswith(b"\n") else b + b"\n"
            for e in (hdr, tslot):
                if e is not None and e["off"] not in ids:
                    out.write(raw(e))
            for e in seg:
                out.write(raw(e))
        return
    h = read_obj(src, hdr) if hdr else {}
    ts = read_obj(src, tslot) if tslot else {}
    doc = {"schema": "segment-plan/1", "session_id": h.get("id") or (es[0]["id"] if es else "unknown"),
           "cwd": h.get("cwd") or "unknown",
           "title": ((ts.get("title") or "").strip() if tslot else ""),
           "dmodel": parts[-1]["dmodel"] if parts else "omp", "target_bytes": o["target"],
           "parts": [{k: v for k, v in p.items() if k != "_seg"} for p in parts],
           "divergence": None, "orphans": []}
    print(json.dumps(doc))

main()
