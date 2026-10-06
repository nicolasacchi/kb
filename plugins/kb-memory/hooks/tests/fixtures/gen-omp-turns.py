#!/usr/bin/env python3
"""Tiny deterministic omp session generator for the segmented-capture tests.

  gen-omp-turns.py create FILE SID TURNS [--blob N] [--model M] [--writes K]
  gen-omp-turns.py append FILE TURNS [--parent ID] [--tag T] [--blob N]
  gen-omp-turns.py reset  FILE            (append a reset_boundary on the leaf)
  gen-omp-turns.py exit   FILE KIND [--parent ID]
                                          (append a session_exit custom entry on the leaf, or
                                           on ID: a dead-end branch when later appends
                                           continue from the real leaf via --parent;
                                           the next plain append continues after it)

A turn is: user message -> assistant (text + toolCall) -> tool_execution_start
-> toolResult, one genuine id/parentId chain. Every `--writes`-th turn calls a
`write` tool (so the translation carries an edited-set snapshot). Output is a
pure function of the arguments: ids t00001u/a/s/r, timestamps 10 s apart.
"""
import json, sys, datetime

def iso(sec):
    base = datetime.datetime(2026, 8, 24, 10, 0, 0, tzinfo=datetime.timezone.utc)
    return (base + datetime.timedelta(seconds=sec)).strftime("%Y-%m-%dT%H:%M:%S.000Z")

def opt(args, name, default):
    if name in args:
        return args[args.index(name) + 1]
    return default

def turn(n, parent, tag, blob, writes):
    out = []
    t = n * 10
    uid, aid, sid_, rid = ("%s%05d%s" % (tag, n, c) for c in "uasr")
    tc = "call_%s%05d" % (tag, n)
    name = "write" if writes and n % writes == 0 else "bash"
    args = {"path": "/tmp/f%d.txt" % n} if name == "write" else {"command": "echo %d" % n}
    out.append({"type": "message", "id": uid, "parentId": parent, "timestamp": iso(t),
                "message": {"role": "user", "content": [{"type": "text", "text": "TURN-%s%05d please do the thing" % (tag, n)}]}})
    out.append({"type": "message", "id": aid, "parentId": uid, "timestamp": iso(t + 1),
                "message": {"role": "assistant", "model": "prov/model-x",
                            "content": [{"type": "text", "text": "ASSISTANT-%s%05d" % (tag, n)},
                                        {"type": "toolCall", "id": tc, "name": name, "arguments": args}],
                            "usage": {"input": 100, "output": 10, "cacheRead": 0, "cacheWrite": 0}}})
    out.append({"type": "custom", "customType": "tool_execution_start", "id": sid_, "parentId": aid,
                "timestamp": iso(t + 2), "data": {"toolCallId": tc, "intent": "intent %d" % n}})
    out.append({"type": "message", "id": rid, "parentId": sid_, "timestamp": iso(t + 3),
                "message": {"role": "toolResult", "toolCallId": tc, "toolName": name,
                            "content": [{"type": "text", "text": ("RESULT-%s%05d " % (tag, n)) + "x" * blob}]}})
    return out, rid

def title_slot(title):
    s = json.dumps({"type": "title", "v": 1, "title": title})
    return s + " " * (255 - len(s))

def read(path):
    with open(path) as f:
        return [json.loads(l) for l in f if l.strip() and not l.startswith('{"type": "title"') and not l.startswith('{"type":"title"')]

def main():
    a = sys.argv[1:]
    cmd = a[0]
    if cmd == "create":
        path, sid, turns = a[1], a[2], int(a[3])
        blob = int(opt(a, "--blob", "300"))
        model = opt(a, "--model", "prov/model-x")
        writes = int(opt(a, "--writes", "0"))
        recs = [{"type": "session", "version": 3, "id": sid, "timestamp": iso(0), "cwd": "/tmp/segtest", "title": "seg"}]
        lines = [json.dumps(recs[0]), title_slot("Initial title")]
        mc = {"type": "model_change", "id": "m00001", "parentId": None, "timestamp": iso(1), "model": model}
        lines.append(json.dumps(mc))
        parent = "m00001"
        for n in range(1, turns + 1):
            ents, parent = turn(n, parent, "t", blob, writes)
            lines += [json.dumps(e) for e in ents]
        open(path, "w").write("\n".join(lines) + "\n")
    elif cmd == "append":
        path, turns = a[1], int(a[2])
        blob = int(opt(a, "--blob", "300"))
        tag = opt(a, "--tag", "t")
        ents = read(path)
        nums = [int(e["id"][1:6]) for e in ents if e.get("id", "")[:1] == tag and e["id"][1:6].isdigit()]
        n0 = max(nums) if nums else 0
        parent = opt(a, "--parent", None) or [e for e in ents if e.get("id")][-1]["id"]
        with open(path, "a") as f:
            for n in range(n0 + 1, n0 + 1 + turns):
                t, parent = turn(n, parent, tag, blob, 0)
                for e in t:
                    f.write(json.dumps(e) + "\n")
    elif cmd == "reset":
        path = a[1]
        ents = read(path)
        leaf = [e for e in ents if e.get("id")][-1]["id"]
        with open(path, "a") as f:
            f.write(json.dumps({"type": "reset_boundary", "id": "rb%s" % leaf, "parentId": leaf, "timestamp": iso(99999)}) + "\n")
    elif cmd == "exit":
        path, kind = a[1], a[2]
        ents = read(path)
        leaf = opt(a, "--parent", None) or [e for e in ents if e.get("id")][-1]["id"]
        with open(path, "a") as f:
            f.write(json.dumps({"type": "custom", "customType": "session_exit", "id": "xit%s" % leaf,
                                "parentId": leaf, "timestamp": iso(50000 + len(ents)),
                                "data": {"kind": kind, "pendingToolCalls": []}}) + "\n")
    else:
        sys.exit("usage")

main()
