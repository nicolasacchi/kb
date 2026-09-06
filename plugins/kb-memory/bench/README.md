# `recall-order-probe` — MR2

Does the **position** of a memory inside `kb-recall.sh`'s injected block change
whether a model can use it?

The kb-slate design of record
([`docs/research/kb-slate-design-2026-09.html`](../../../docs/research/kb-slate-design-2026-09.html)
§11) calls order "the one free lever nobody has measured here" and makes the
measurement a precondition: `KB_RECALL_LAYOUT`'s default flips from `v2` to
`v2-last` **only** on a ≥ 5-point win on every model that ran. This directory is
that instrument. The verdict lives in
[`docs/research/kb-recall-order-probe-2026-09.html`](../../../docs/research/kb-recall-order-probe-2026-09.html).

`kb bench` is deliberately *not* extended for this: it measures Recall@k, MRR
and nDCG over ids and cannot see model behaviour.

## Files

| file | what |
| --- | --- |
| `recall-order-probe.sh` | the harness — resumable, throttled, one `results.jsonl` line per call |
| `probe-queries.jsonl` | **private, not in this repo** — 30 rows: `{query_id, query, pack}`. The pack is **frozen** — the five hits `kb recall --limit 5 --json` returned for that query on 2026-09-05, carrying only the fields `kb-recall.sh`'s jq filter reads |
| `probe-questions.jsonl` | **private, not in this repo** — 90 rows: `{query_id, k, question, answer, source_id}`, three per pack at `k ∈ {1,3,5}` |
| `probe-filler.txt` | 80,000 characters (≈20K tokens) of one captured session transcript's first `<pre>`, unedited — the block sits behind it so it lands where a hook injection lands |
| `run-reduced/`, `run/` | `results.jsonl` + `report.md` per run |

## Why the packs are frozen

The spec's shape is "pull `kb recall --limit 5 --json` per pack". Pulling live
would make the run irreproducible and, worse, silently invalidate the checked-in
questions the moment a memory is written, forgotten or re-ranked — the answers
are keyed to specific hits at specific ranks. The packs are therefore snapshotted
into `probe-queries.jsonl` and the harness renders from them. Re-freeze by
re-running the authoring pass; never hand-edit a pack without re-running
`./recall-order-probe.sh --verify`.

## What a question is, and the honest limits of it

Each question is a **cloze over hit k's rendered title**: the title with one
word blanked, and "reply with only that word". Scoring is a normalized exact
match (case-folded, punctuation-stripped, whitespace-collapsed).

Two deliberate departures from the design's sketch, both forced by what is
actually on the wire:

1. **The answer comes from the title, not the summary.** A `summary` on a recall
   hit is the `kb-summary` meta, and most live memories have none — whole packs
   would have had no askable content. More decisively: layout `v2` renders **no
   summary at all** at ranks 4–5, so a summary-sourced `k=5` question would
   measure v2's depth cut rather than order, and would be unanswerable under two
   of the three layouts. A title is rendered byte-identically at every rank in
   all three layouts, which makes **position the only variable** — the one thing
   the probe exists to isolate.
2. **A cloze, not a use-the-fact task.** This measures whether a line at
   position *k* is *attended to*, which is the standard needle-in-haystack
   instrument and is what "does order change what reaches the model" asks. It
   does **not** measure whether the model would act on that memory. Read the
   numbers as attention, not utility.

Every answer is checked, on every run, to occur **exactly once** across its whole
pack and **nowhere** in the filler — otherwise the model could answer without
reading the block. `--verify` runs those checks alone.

## Hermeticity

The probe's own model calls run with `KB_DAEMON_URL` pointed at a dead port,
`KB_SESSIONS_DIR` cleared and `KB_BEAT=0`, so this repo's kb hooks fail silently
instead of injecting a **real** recall block into the probe's context (that
contamination would invalidate every number), no probe turn is captured into the
sessions corpus, and the live registry never sees one.

The block itself is rendered by **`kb-recall.sh` itself**, driven with a fake
`kb` on `PATH` that serves the frozen pack. There is no second implementation of
the layouts here: the probe must measure the shipping bytes or it measures
nothing.

## Running

```bash
./recall-order-probe.sh --verify                 # input invariants only
./recall-order-probe.sh --reduced                # 5 packs × 3 layouts × 3 k × 1 model = 45 calls
./recall-order-probe.sh                          # 30 × 3 × 3 × 2 = 540 calls
./recall-order-probe.sh --report-only            # rebuild report.md from results.jsonl
```

Resumable by construction: a `<query_id>|<layout>|<k>|<model>` key already in
`results.jsonl` is skipped, so a killed run resumes where it stopped. It waits
(never more than 10 minutes) whenever `/proc/pressure/io` `full avg10` is at or
above `PROBE_IO_CEILING` (default 40), so the probe backs off instead of
competing for disk I/O with other work on a busy machine.

`PROBE_CLAUDE_MODEL` (default `haiku`) and `PROBE_CODEX_MODEL` pick the models.
The `codex` lane is enabled only when both `codex` and `codexclaude` are on
`PATH`; when a model is unavailable the run says so and the report names which
models actually ran.

## Not committed (regenerate locally)

`probe-filler.txt` and the `run/` / `run-reduced/` directories are
gitignored: the filler is the first `<pre>` of a real captured session
(private content from whatever project it came from) and the run directories
hold model answers over real memory packs. Rebuild the filler from any capture
in the sessions corpus (`kb cat --kb <sessions-kb> <id> | sed -n '/<pre>/,/<\/pre>/p' | head -c 80000 > probe-filler.txt`)
and re-run the probe to repopulate `run*/`; the verdict artifact
(`docs/research/kb-recall-order-probe-2026-09.html`) records the numbers the
2026-09-05 run produced.

`probe-queries.jsonl` and `probe-questions.jsonl` are **not shipped in the
public repo** either, for the same reason as the filler: the frozen packs
are real titles and ids pulled from the operator's own memory corpus on
2026-09-05, and the questions are cloze answers keyed to those real titles —
publishing them would leak memory content this project otherwise keeps
private. Neither file is referenced by CI or by any other script; only
`recall-order-probe.sh` (this directory) and the two research artifacts
above cite them, so their absence breaks nothing but the probe itself.
To regenerate a private pair of your own: run `kb recall --limit 5 --json`
against your own memory corpus for ~30 varied queries to build
`probe-queries.jsonl` (`{query_id, query, pack}` per row, pack = that
query's five hits verbatim), then author three cloze questions per pack
(one per `k ∈ {1,3,5}`, blanking one word of that rank's rendered title) into
`probe-questions.jsonl` (`{query_id, k, question, answer, source_id}`), and
run `./recall-order-probe.sh --verify` to check the invariants above before
a real run.
