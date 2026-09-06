# Embedding bake-off bench

This directory holds the assets for the retrieval-quality bake-off
that compares the three BGE models registered in
`kb_core::embed::SUPPORTED_MODELS`:

| model              | dim  | size    |
|--------------------|------|---------|
| bge-small-en-v1.5  | 384  |  127 MB |
| bge-base-en-v1.5   | 768  |  440 MB |
| bge-large-en-v1.5  | 1024 | 1340 MB |

The bench answers: **does a larger model improve retrieval quality on
your own corpora?** Same source dir, three parallel kbs,
one daemon, hand-curated query sets per corpus, Recall@k / MRR /
nDCG@10 side-by-side.

## How the pieces fit

- `bench/kb.toml` — throwaway daemon config wiring three kbs per
  corpus, one per model. Do not point production at it; this is for a
  separate daemon process started just for the bench. Your production
  daemon's own state is untouched.
- `bench/queries/<corpus>.jsonl` — hand-curated query sets. One file
  per corpus; 30–50 queries each. Each line:
  ```json
  {"query": "rust async storage actor",
   "relevant": ["abc123def456", "..."],
   "notes": "optional why-relevant note"}
  ```
- `kb bench {init, discover, run}` — the CLI scaffolds, helps you
  label, and runs the bake-off. See `kb bench --help`.

## Workflow

1. **Pick a bench state root.** The bench daemon writes its lance +
   sqlite under `$XDG_STATE_HOME/kb/<daemon-name>/`. Pick a daemon
   name that doesn't clash with production, e.g. `bench`:

   ```toml
   [daemon]
   name = "bench"
   ```

2. **Download the models** (one-time, ~2 GB total):

   ```bash
   kb model download bge-small-en-v1.5   # already present if you've used kb
   kb model download bge-base-en-v1.5
   kb model download bge-large-en-v1.5
   ```

3. **Boot the bench daemon** with `bench/kb.toml`. The indexer
   embeds every artifact under each kb; this is the long-running
   step (~5.6 emb/s on i7-7700, so 10k docs × 3 models ≈ 90 min).
   Watch `kb push --filter index.complete` until all kbs settle.

   ```bash
   kb daemon --config bench/kb.toml
   ```

4. **Scaffold the query sets** with `kb bench init`. Run once per
   corpus:

   ```bash
   kb bench init \
     --corpus /srv/example-docs \
     --output bench/queries/example-docs.jsonl \
     --n 40 --seed 0xb33f
   ```

   Each scaffold line has an empty `query` field and a `relevant`
   array pre-seeded with the sampled file's artifact id. Open the
   file in your editor and fill in real queries (one per line, the
   `relevant` array can hold multiple ids).

5. **Discover candidate-relevant ids** as you label. Drive the
   daemon, eyeball the top hits, pick which ones you'd consider
   relevant for a given query, and paste their 12-hex ids into the
   `relevant` array of the matching jsonl row:

   ```bash
   kb bench discover \
     --kb example-docs-small \
     --query "atlas determinism" \
     --limit 20
   ```

6. **Run the bake-off** once your jsonl is labelled:

   ```bash
   kb bench run \
     --queries bench/queries/example-docs.jsonl \
     --kbs example-docs-small,example-docs-base,example-docs-large \
     --modes hybrid,semantic \
     --k 1 --k 5 --k 10 \
     --output docs/bench/example-docs-2026-05-19.md \
     --json docs/bench/example-docs-2026-05-19.json
   ```

   The report has one table per mode, kb rows × metric columns.
   Compare the rows: if `example-docs-base` beats `example-docs-small` by
   ≥5pp Recall@5 across modes, the bigger model is worth the cost.

## Interpreting modes

- **semantic** — vector-only retrieval. Isolates the embedding
  model's contribution.
- **hybrid** — BM25 + vector via lance's RRF (k=60). The production
  default. If hybrid scores are flat while semantic improves with a
  bigger model, BM25 is doing most of the work and a model swap
  isn't worth the cost.

## Cost reality

Embed time scales as `N_docs × N_models / 5.6 emb/s` on an i7-7700
Kaby Lake. A 10k-doc kb × 3 models = ~90 minutes. Multiple corpora
add up — plan to run overnight the first time.

Disk: bge-large embeddings are 4 KB/doc (1024 × 4 bytes). For 10k
docs × 3 models per corpus, the bench state dir holds ~120 MB of
vector data. Plus ~2 GB of ONNX weights in `$XDG_CACHE_HOME/kb/`.

## baseline.json provenance

The committed `bench/baseline.json` (2026-07-11) is the report from the
**first successful CI-runner run** of the `bench-nightly` workflow
(run `29159168953`, `workflow_dispatch` on `main` @ `222b99ef`,
ubuntu-latest, release build, keyword/BM25-only, the 20-query
`bench/queries/kb-docs.jsonl` set). It replaces the 2026-07-10
GC-A8 / `56f24cf5` baseline, which came from a **debug** `kb` binary on
the dev laptop — its latency columns (p50 20.5 ms) were never
representative; the CI runner's release numbers (p50 8.6 ms) are the
comparable reference for nightly latency deltas from now on.

Known caveat: the CI run's *quality* numbers landed below the laptop
baseline on the same query set (Recall@1 0.20 vs 0.45, MRR 0.40 vs
0.59). Ranking-relevant commits landed between the two reports —
notably GC-B1 (`eedd1504`, tie-order determinism pinning) sits inside
`56f24cf5..222b99ef` — and the runner indexes the corpus from scratch,
so BM25 tie behaviour differs from the laptop's warm index. Watch the
next few nightlies: quality deltas are now runner-vs-runner and should
be stable; a further drop is a real regression, not provenance noise.

## The nightly regression bench (CI, self-contained)

Separate from the embedding bake-off above, [`scripts/bench-nightly.sh`](../scripts/bench-nightly.sh)
(driven by [`.github/workflows/bench-nightly.yml`](../.github/workflows/bench-nightly.yml))
is a small, fully self-contained search-quality regression check: every
input lives in this repository. It boots a throwaway daemon over the
repo's own `docs/research` corpus, runs `kb bench run` in **keyword
(BM25-only)** mode against the committed 20-query gold set
[`bench/queries/kb-docs.jsonl`](queries/kb-docs.jsonl) (queries about kb's
own design docs, each with a hand-picked `relevant` artifact id), and
diffs the result against the committed [`bench/baseline.json`](baseline.json).
BM25-only means the job needs no `kb-embedder` binary, no ONNX Runtime,
and no model download.

Run it locally the same way CI does:

```bash
scripts/bench-nightly.sh --baseline bench/baseline.json
```

It's non-blocking by design (scheduled + manual dispatch only, never on a
PR) — a quality or latency regression shows up as a signed delta in the
job summary, not a failed check. To establish a new baseline after an
intentional ranking change, copy a fresh `report.json` over
`bench/baseline.json` and commit it.

## Caveats

- Hand-curated query sets bias the measurement toward what the
  labeller knows. The report is a comparison, not an absolute truth.
- The bench measures the daemon's `/api/search` end-to-end (BM25 +
  RRF + vector). A retrieval-quality win at this layer doesn't
  necessarily translate to a UX win — the SPA still ranks within the
  top-k window the operator already sees today.
- The bench daemon must not share state with the production daemon
  (different `[daemon] name`). Crossing the streams would let the
  bench's indexer overwrite production embeddings.
