# KBT-STRIP: tap query stripper v2 with a semantic fuzz oracle

Throwaway spike (kb-term v2 "Mirror"). Zero dependencies: `bun test` here, `bash ci.sh` in CI.

* `strip.ts`: pipeline `TmuxUnwrap x4 -> C1Filter -> Stripper` (`QueryStripper` composes them).
  Stages pass `number[]` with `BOUNDARY (-1)` meaning "abort whatever is in progress", which is how a
  deleted C1 or an envelope edge can never splice bytes together.
* `oracle.ts`: an independent whole-buffer VT action parser (shares no code with `strip.ts`) with its own
  query-shape table. Policy: a sequence cut by ESC/CAN/SUB/C1 yields no action; DCS-tmux is unwrapped recursively.
* `strip.test.ts`: goldens (every strip-list shape, splice repros, tmux, C1, caps, no-timer), each run whole,
  at every 2-way split, every 3-way split (short ones) and byte by byte.
* `fuzz.test.ts`: `actions(output) == actions(input) - queries` and "no query shape left" over fragment+query+tail
  streams, token soup, exhaustive 2-cut splits, and `gen/fake_harness.py` output with queries injected at random offsets.
* `ci.sh` -> `out/summary.txt`, `out/bun-test.txt`, `out/metrics-*.json`.

Comparison normalisation: CAN (0x18) actions are ignored on both sides. The stripper appends a CAN when it aborts a
string it has already forwarded, so the receiver leaves the string state too.
