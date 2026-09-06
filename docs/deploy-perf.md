# Deploying the PERF milestone (shipped within v0.41's range)

The PF-wave performance commits landed on `main` just before the slate
milestone tagged v0.41, so v0.41+ binaries carry everything below — there
is no separate PERF tag.

Notes for rolling the 2026-09 performance milestone (the PF waves) onto a
production host. Deploys stay operator-owned — this doc recommends, it
does not mutate.

## The one hard rule: no binary rollback after V0040

`V0040__sessions_is_newest.sql` bumps the schema epoch. The kb-sibling/1
boot guard means a **pre-V0040 binary refuses to boot** against a volume
this release has migrated (by design — the 13.5h kbc outage lesson).
Deploy order per host:

1. Snapshot/backup the state dir (as usual).
2. Deploy the new binary; migrations run at boot, per kb, automatically.
3. If anything goes wrong, roll **forward** (fix + redeploy), not back.
   A true rollback requires restoring the pre-deploy state snapshot
   alongside the old binary.

The migration itself is cheap (one `ALTER TABLE` + backfill `UPDATE` +
partial index per kb) — sessions-corpus sized tables migrate in well
under a second.

## Config: new knobs, recommended values

| key | default | recommendation |
|---|---|---|
| `[server] fanout_cap` | 8 | **Leave at 8** until measured on the target host. It caps per-request corpus fan-out concurrency for every `scope=all` read. Raising it lowers federated latency on many-kb daemons at the cost of parallel storage-actor + `spawn_blocking` pressure; on a large multi-corpus deployment (~24 kbs, in the observed case below) a bump to 12–16 is the plausible sweet spot — change it only with a before/after `kb bench`/curl-timing check. Applies via config edit (in-process restart). |
| `[kb.<name>] reconcile_secs` | inherit | New per-kb override. Set a **large value (e.g. 3600) on big, rarely-changing corpora** (research archives, frozen showcases) to stop paying a full walk every 60s; leave hot corpora (sessions, memory) on the daemon default. `KB_RECONCILE_SECS` env still trumps everything. The per-kb auto-compact ticker follows the same value. |

No other config changes are required; absent keys are byte-identical to
pre-milestone behavior.

## What you get (evidence highlights)

Two-daemon A/B lab: identical corpora (1.2k HTML docs + 200 sessions × 4
captures each), pre = the pre-milestone binary, post = the milestone
binary, BM25-only, warm p50 over 20 samples:

| endpoint | pre p50 ms | post p50 ms | Δ |
|---|---|---|---|
| `/kb/{kb}/daycard` | 30.7 | 12.8 | **−58%** |
| `/kb/{kb}/resurface` | 21.6 | 11.0 | **−49%** |
| `/desk` | 26.9 | 15.5 | **−42%** |
| `/sessions/funnel` | 3.2 | 2.4 | −25% |
| `/sessions` (list) | 4.6 | 4.3 | −7% |
| `/docs?projection=atlas` | 27.4 | 26.3 | flat (deferred item) |

The daycard/desk/resurface wins are the gallery-memo rewires; they grow
with corpus size (the pre binary re-pulls the full corpus per request).
The funnel/list deltas grow with capture density — the lab used 4
captures per session; a busy sessions corpus can carry 20+ on busy days,
and the correlated re-sort the flag replaced was per-outer-row.

- **Sessions storage**: the newest-capture pick is precomputed
  (`is_newest`, V0040) instead of a correlated re-sort per read at ~30
  call sites; the funnel is 3 statements instead of 9; ~74 statements
  now `prepare_cached` (cache sized 128). Multi-capture-heavy corpora
  (the sessions kb) benefit most.
- **Route layer**: daycard/desk/resurface/graph/touches ride the shared
  gallery memo instead of per-request full-corpus pulls.
- **Session view wire**: a no-projection `GET …/view` now returns the
  last 50 turns (`?turns=all` = the old full body; additive
  `turns_total`/`turns_returned` fields). Agent/CLI consumers of full
  transcripts must ask explicitly. The SPA is unaffected (its reader
  uses the HTML presenter; its one JSON consumer pins `turns=all`).
- **kb-code-server**: review list / inbox / recurrence composed via
  batched queries; the per-call-site git blob read in hierarchy walks is
  memoized per request. `tests/measure/latency.rs` now gates PR Room
  read budgets.
- **SPA images**: prod bundles ship no sourcemaps — `dist/` −69% (web)
  / −70% (web-code); JS bytes unchanged.
- **CI** (not prod-facing, recorded for completeness): the workspace
  suite runs under nextest — test phase ~2–3 min for ~3,590 tests vs
  40–60+ min serial; drift checks moved off the critical path.

## Lab findings feeding PF-I1

- **Atlas cliff confirmed**: `?projection=atlas` (uncached per-request
  full lance scan, the deliberately deferred item) measured 28ms p50 at
  1.2k docs → 173–191ms at ~5.9k docs (~30µs/doc, linear) — extrapolates
  to ~600ms at 20k. Prod-sized corpora pay 150–200ms per atlas load
  today; a generation-keyed memo (the gallery pattern) is now
  evidence-justified. Caveat: measured during concurrent ingest on the
  same daemon; the p50s were stable, one p95 was churn-inflated.
- **Ingest slowdown: resolved to contention + fragmentation, compaction
  is healthy** (PF-F1 probe). An initial run degraded to ~21 docs/min/kb
  — but that run had TWO kbs bulk-ingesting concurrently plus a second
  daemon on the same HDD. A controlled re-run (one ingesting kb, probe
  logs) sustained ~800 docs/min to 20k rows, with the periodic
  auto-compact firing every cycle exactly as designed (71–106 fragments
  → 1, 166–321ms per pass, versions pruned). Post-compact the atlas
  full scan at 20k measured 231ms p50 — well under the fragmented-state
  extrapolation (~600ms): fragmentation multiplies scan cost; compacted
  row-count scaling is mild (~11µs/doc). Operational advice: avoid
  parallel bulk imports on HDD hosts; compaction needs no tuning.
- **Prod-shaped RSS fully attributed** (read-only in-container probe):
  2.80GB cgroup = kb daemon 780MB + embed-model
  kb-embedder 1.67GB + reranker kb-embedder 242MB. The 1.67GB for a
  ~130MB-weight bge-small model points at ORT arena growth (the old
  ">32 batch thrashes RSS" spike note — arenas don't shrink). So
  ~2.8GB IS steady state with a resident embedder: the memory limit can
  come down to ~6–8g, never to the old 300–500MB bar.
- **Arena-RSS verdict (closes the follow-up)**: driving the binary
  under sustained mixed-shape load (12 rounds, long near-cap texts),
  RSS plateaus at ~1.07GB **capped-4 AND uncapped alike**, flat from
  round 3 — the arena high-water mark is input-shape-driven, not
  thread-count-driven, and it is bounded, not a leak. Prod's 1.67GB is
  the same behavior at real input variety. fastembed exposes no ORT
  session options (no arena-shrink/memory-limit without patching), so
  the recorded close is: document it as expected steady state (the
  6–8g limit stands) and note periodic subprocess recycling (the
  respawn machinery already exists) as the future lever if a host ever
  needs the memory back. No code warranted now.

## Known-latent items (not regressions, tracked)

- e2e specs `spa-canvas` (timeline-refetch race) and `spa-siblings`
  (hotkey URL) flake under runner load; rerun-failed is the mitigation
  until the specs are hardened.
- Three flat-800ms SSE test drains remain in `end_to_end.rs`
  (~6219/~10063/~18161) — same class as the one the nextest flip
  surfaced and we fixed; convert on next fire.
- A long-running kb container idles at multi-GiB RSS (2.79 GiB observed
  in one case, cap 12 GiB) — cause unattributed (lance caches / mmap /
  embedder RSS), on the PF-I1 follow-up list. Hold the current memory
  limit until attributed.
