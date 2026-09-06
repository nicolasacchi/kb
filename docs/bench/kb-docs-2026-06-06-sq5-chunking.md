# SQ5 passage/chunk embeddings A/B — kb-docs (2026-06-06)

Same corpus + embedder (bge-large), with vs. without passage chunking
(`chunked_embeddings`). The chunked kb indexes each doc's passages into
`artifact_chunks`; semantic/hybrid search vector-queries the chunks and
max-pools to a per-doc score. 20 labelled queries.

| kb | mode | Recall@1 | Recall@5 | Recall@10 | MRR | nDCG@10 |
|---|---|---:|---:|---:|---:|---:|
| kb-docs-large | hybrid | 0.800 | 0.950 | 1.000 | 0.868 | 0.899 |
| kb-docs-large-chunked | hybrid | 0.750 | 0.950 | 0.950 | 0.833 | 0.863 |
| kb-docs-large | semantic | 0.750 | 0.950 | 0.950 | 0.827 | 0.853 |
| kb-docs-large-chunked | semantic | 0.700 | 0.950 | 0.950 | 0.804 | 0.837 |

## Verdict — keep chunking OFF for kb-docs; it's a long-doc lever

Chunking was **slightly negative** here (hybrid/semantic R@1 −0.05). Why:
chunking fixes the 512-token whole-body embedding truncation, but
**kb-docs is mostly short-to-medium technical docs** (`longread` =
`word_count > 1500` is the minority), so most bodies already embed in
full — the truncation it fixes rarely applies. Splitting a short doc into
passages and max-pooling then adds a little noise versus the single clean
whole-body vector (a passage can match a query on a tangential span).

This is **not** a regression in shipped behaviour — `chunked_embeddings`
is opt-in per kb and defaults OFF; the daemon also falls back to the
doc-level vector arm when the chunk table is empty. The implementation is
correct (16 unit/storage tests + a clean code deep-review; the chunk arm
verified live: a "lance storage schema" semantic query returns the
storage doc as #1 via `chunk_vector_query`).

**Where to enable it:** corpora of genuinely long documents — multi-page
research write-ups, incident timelines, long markdown (e.g. the
`research`/`platform` corpora) — where the doc body exceeds ~512 tokens
and the answer lives past the first ~400 words. Enable per kb, run
`kb reindex` to populate the chunk table, and **bench that corpus** before
trusting it. Do not enable blindly on a short-doc corpus.
