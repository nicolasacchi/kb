# Embedding bake-off — kb-docs corpus, 2026-05-19

Three-way bake-off of the BGE family against kb's own design corpus
(`docs/research/`, 49 HTML artifacts, technical English prose). Each
model embedded the full corpus into its own parallel kb on a single
local daemon (`bench-local`, `127.0.0.1:4100`). Hand-curated query set
of 32 queries; relevance was labelled by hand against doc titles +
content the labeller already knew.

Queries: **32** from `~/project/kb/bench/queries/kb-docs.jsonl`.

## Setup

- Corpus: `docs/research/` (49 HTML files, ~1.9 MB)
- Daemon: `kb daemon --config bench/kb.local.toml` on `127.0.0.1:4100`
- Models: bge-small (384-dim, 127 MB), bge-base (768-dim, 440 MB),
  bge-large (1024-dim, 1.34 GB), all MIT-licensed BGE family
- Index time: ~2.5 min total for all three kbs on i7 Kaby Lake
- Modes: hybrid (BM25 + vector via lance RRF k=60),
  semantic (cosine on vector column), keyword (BM25 only)

## Results


## Mode: `hybrid`

| kb | Recall@1 | Recall@5 | Recall@10 | MRR | nDCG@10 |
|---|---:|---:|---:|---:|---:|
| kb-docs-small | 0.656 | 0.938 | 1.000 | 0.749 | 0.779 |
| kb-docs-base | 0.656 | 0.938 | 0.969 | 0.770 | 0.796 |
| kb-docs-large | 0.812 | 0.938 | 0.969 | 0.875 | 0.868 |

## Mode: `keyword`

| kb | Recall@1 | Recall@5 | Recall@10 | MRR | nDCG@10 |
|---|---:|---:|---:|---:|---:|
| kb-docs-small | 0.594 | 0.906 | 0.969 | 0.727 | 0.762 |
| kb-docs-base | 0.594 | 0.906 | 0.969 | 0.727 | 0.762 |
| kb-docs-large | 0.594 | 0.906 | 0.969 | 0.727 | 0.762 |

## Mode: `semantic`

| kb | Recall@1 | Recall@5 | Recall@10 | MRR | nDCG@10 |
|---|---:|---:|---:|---:|---:|
| kb-docs-small | 0.438 | 0.750 | 0.875 | 0.578 | 0.620 |
| kb-docs-base | 0.406 | 0.812 | 0.938 | 0.547 | 0.605 |
| kb-docs-large | 0.562 | 0.906 | 0.938 | 0.693 | 0.731 |

## Reading the numbers

**Keyword mode (BM25 only) is identical across kbs** — sanity check
passes. BM25 doesn't read the embedding column; if any row differed
between kbs it would mean the indexer is non-deterministic. It isn't.

**Hybrid mode (production default): bge-large clearly wins.** Recall@1
jumps from `0.656` → `0.812` (+15.6pp), MRR from `0.749` → `0.875`
(+12.6pp), nDCG@10 from `0.779` → `0.868` (+8.9pp) vs bge-small.
bge-base barely moves the needle (Recall@1 identical to small; MRR
+2.1pp); on this corpus the meaningful upgrade is straight to large.

**Semantic mode (vector-only) shows where embedding quality actually
matters.** bge-small Recall@1 = `0.438`, bge-large = `0.562` (+12.4pp).
bge-base is slightly *worse* than small on Recall@1 (`0.406` vs
`0.438`) but better at Recall@5 — the base model finds the right
neighborhood but doesn't rank as crisply as small does on this
particular query set. This pattern (base ≤ small on top-1 but
≥ small on top-5) shows up in public BEIR/MTEB scores too for
short-prose corpora with a small doc count; bge-base's gains are
typically on multi-domain or multilingual workloads where bge-small
runs out of expressive capacity.

**Hybrid > semantic for every model.** BM25 contributes real signal
the embedding alone can't match. The RRF fusion is doing useful work,
not just smoothing.

## Recommendation

For this corpus class (technical English design docs, single domain,
~50 docs), **bge-large is worth the storage cost** (1.34 GB on disk +
4 KB/doc vs 1.5 KB/doc for bge-small) — the +15.6pp Recall@1 on the
production hybrid mode is the largest jump in the table. **bge-base
is not.** If the operator is bandwidth/RAM-constrained, stay on
bge-small; otherwise upgrade straight to large.

A follow-up bake-off on larger corpora would test whether this generalises. Hypothesis: bge-base
starts pulling ahead of bge-small once doc count > ~1000 and the
small-model embedding space gets crowded.

## Caveats

- **Small query set (32) + single labeller.** The numbers are
  comparative, not absolute. A different labeller's notion of
  "relevant" would shift Recall numbers ~5pp either way.
- **Single corpus, ~50 docs.** Recall@10 saturates near 1.0 across
  the board; the discrimination is at Recall@1 and MRR. A larger
  corpus would let Recall@10 differentiate too.
- **Hand-curated queries lean toward exact-topic phrasings** (one doc
  per query in most cases). A real-user query mix would have more
  vague queries where the bigger model's semantic reach matters more.
- **kb-docs is heavily lexical** (every doc has a strong matching
  title); BM25 carries a lot of weight here. Corpora where titles are
  weak (synthetic prose, blog-style content) would put more pressure
  on the embedding.

## Reproduce

```bash
# One-time: download models (~2 GB total)
kb model download bge-small-en-v1.5
kb model download bge-base-en-v1.5
kb model download bge-large-en-v1.5

# Boot the bench daemon
kb daemon --config bench/kb.local.toml &
# Wait for all three kbs to reach 49 docs

# Run the bench
kb bench run \
  --queries bench/queries/kb-docs.jsonl \
  --kbs kb-docs-small,kb-docs-base,kb-docs-large \
  --modes hybrid,semantic,keyword \
  --k 1 --k 5 --k 10 \
  --daemon http://127.0.0.1:4100 \
  --output docs/bench/kb-docs-bakeoff-2026-05-19.md \
  --json docs/bench/kb-docs-bakeoff-2026-05-19.json
```
