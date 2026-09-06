# SQ4 reranker A/B — kb-docs (2026-06-06)

Cross-encoder reranker (`bge-reranker-base`, opt-in) over the fused
top-50, scoring on `title + body_text_excerpt`. Same corpus + embedder
(bge-large) with vs. without the reranker; 20 labelled queries.

## Mode: `hybrid`

| kb | Recall@1 | Recall@5 | Recall@10 | MRR | nDCG@10 |
|---|---:|---:|---:|---:|---:|
| kb-docs-large (no rerank) | 0.800 | 0.950 | 1.000 | 0.868 | 0.899 |
| kb-docs-large-rerank | 0.600 | 0.700 | 0.750 | 0.658 | 0.677 |

## Verdict — keep the reranker OFF here (for now)

The reranker **regressed** every metric (R@1 −0.20, R@10 −0.25). Root
cause: the document text fed to the cross-encoder is `title +
body_text_excerpt` — a single short excerpt that under-represents the
artifact, so the reranker mis-scores against an already-strong
hybrid+title-boost baseline. The deep-review predicted exactly this
("re-bench SQ4 with chunk texts"): a cross-encoder needs a representative
passage, not a one-line excerpt.

This is **not** a regression in shipped behavior — reranking is opt-in
per kb (`reranker_model`) and defaults OFF; the daemon also degrades to
the fusion order if the model is absent. **Do not enable `reranker_model`
on a corpus without per-corpus bench evidence.** Revisit after SQ5
(passage/chunk embeddings): rerank the winning chunk's text rather than
the excerpt, then re-run this A/B.
