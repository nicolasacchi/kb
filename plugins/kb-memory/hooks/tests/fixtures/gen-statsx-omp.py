#!/usr/bin/env python3
"""Generate statsx-omp-session.jsonl: SYNTHETIC omp v3 transcript for the
STATSX3 harness_usage tests. Invented ids/models only.
Run: python3 -I gen-statsx-omp.py > statsx-omp-session.jsonl
(Named without the omp- prefix on purpose: omp-*.jsonl fixtures are globbed by
the Rust chain tests and need an omp-chain-goldens.json entry.)"""
import json

def e(**k):
    print(json.dumps(k, separators=(",", ":")))

def asst(id_, parent, ts, usage, content="ok", **extra):
    m = {"role": "assistant", "model": "synthetic/model-x",
         "content": [{"type": "text", "text": content}], "usage": usage}
    m.update(extra)
    e(type="message", id=id_, parentId=parent,
      timestamp="2026-11-01T10:%02d:00.000Z" % ts, message=m)

e(type="session", version=3, id="statsx01", timestamp="2026-11-01T10:00:00.000Z",
  cwd="/tmp/statsx-fixture", title="statsx fixture")
e(type="message", id="u1", parentId=None, timestamp="2026-11-01T10:01:00.000Z",
  message={"role": "user", "content": [{"type": "text", "text": "do the thing"}]})
# R1: full new data
asst("r1", "u1", 2,
     {"input": 100, "output": 50, "cacheRead": 900, "cacheWrite": 40,
      "cost": {"input": 0.001, "output": 0.01, "cacheRead": 0.0009,
               "cacheWrite": 0.0004, "total": 0.0123}},
     responseId="R1", duration=4200, ttft=310, completedAt=1793527320000,
     stopReason="toolUse",
     contextSnapshot={"promptTokens": 1040, "nonMessageTokens": 200,
                      "toolSchemaTokens": 150, "compactionEpoch": 2})
# R2: duplicate responseId R1 later on the live chain
asst("r2", "r1", 3,
     {"input": 100, "output": 50, "cacheRead": 900, "cacheWrite": 40,
      "cost": {"total": 0.0123}},
     content="dup", responseId="R1", duration=4200)
# R3: no responseId, no new fields
asst("r3", "r2", 4, {"input": 10, "output": 5, "cacheRead": 0, "cacheWrite": 0},
     content="plain")
# abandoned branch carrying responseId R6 (not on the leaf chain)
asst("b1", "r3", 5, {"input": 1, "output": 1, "cacheRead": 7, "cacheWrite": 0},
     content="abandoned", responseId="R6")
# R4: junk values (kept legacy-translatable: the pre-existing usage fold has no coercion) on the live chain (parent r3, so b1 is abandoned)
asst("r4", "r3", 6,
     {"input": 3, "output": None, "cacheRead": 0, "cacheWrite": -5,
      "cost": "free"},
     content="junk", responseId="R4", duration="slow", ttft=-1,
     contextSnapshot="none")
# live entry sharing the abandoned branch's responseId R6
asst("r5", "r4", 7, {"input": 20, "output": 7, "cacheRead": 5, "cacheWrite": 0,
                      "cost": {"total": 0.002}},
     content="live R6", responseId="R6", duration=900)
