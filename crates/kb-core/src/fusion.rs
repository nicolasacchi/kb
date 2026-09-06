//! SQ2 — Rust-side hybrid fusion.
//!
//! The search path historically delegated hybrid ranking to lance's
//! internal Reciprocal Rank Fusion (`Storage::hybrid_query` chains
//! `.full_text_search().nearest_to()` and lance merges). That couples
//! two limits we want to break apart:
//!
//! 1. lance fetches each arm at only the final `limit`, so a document
//!    ranked just past `limit` in one arm but top in the other can be
//!    dropped before fusion ever sees it.
//! 2. There is no hook for field weighting, filtering, or boosts.
//!
//! Pulling fusion into Rust lets the route over-fetch each arm into a
//! larger candidate pool and fuse here. Crucially this leaves
//! `Storage::hybrid_query` **untouched** — it is still the backbone of
//! agent-memory recall (`routes/memory.rs`), whose ranking must not move
//! (root invariant #10/#11).
//!
//! The algorithm mirrors lance's hybrid reranker so a pool equal to the
//! limit reproduces the previous ranking: score = Σ 1/(K + rank) over
//! every arm a hit appears in (0-based rank, K = 60), then a stable sort
//! by score descending. Ties resolve by first appearance across the arms
//! in the order passed — lance concatenates `[vector, fts]` and keeps the
//! first occurrence before its stable sort, so callers pass the vector
//! arm first to match.

use crate::storage::lance::DocSummary;

/// RRF rank constant. Matches lance's hybrid default and
/// `memory::rerank` (`1/(K + rank)`, 0-based). Keeping the same constant
/// is what makes the Rust path reproduce the lance baseline.
pub const RRF_K: f32 = 60.0;

/// Reciprocal-rank-fuse N ranked arms into a single ranking.
///
/// Each arm is an owned vec of hits in descending relevance (index 0 =
/// best); the arms are consumed so the surviving hit is moved, not cloned.
/// A hit's fused score is `Σ 1/(k + rank)` across every arm it appears
/// in; dedup is by [`DocSummary::id`] (globally unique per artifact).
/// Ties break by first appearance across `arms` in order (so earlier
/// arms win ties — pass the vector arm first to match lance). The
/// returned hits carry their fused score in [`DocSummary::score`], and
/// the list is truncated to `limit`.
pub fn rrf_fuse(arms: Vec<Vec<DocSummary>>, k: f32, limit: usize) -> Vec<DocSummary> {
    use std::collections::HashMap;
    // id -> (accumulated score, first-appearance ordinal, the hit).
    // Consumes the owned arms and MOVES each hit into the accumulator on
    // first appearance (only its score is mutated on repeats) — no
    // per-row DocSummary deep clone. Callers own the arms and drop them
    // right after fusion, so taking ownership here is behaviour-identical.
    let mut acc: HashMap<String, (f32, usize, DocSummary)> = HashMap::new();
    let mut order = 0usize;
    for arm in arms {
        for (rank, doc) in arm.into_iter().enumerate() {
            let contrib = 1.0 / (k + rank as f32);
            if let Some(entry) = acc.get_mut(&doc.id) {
                entry.0 += contrib;
            } else {
                acc.insert(doc.id.clone(), (contrib, order, doc));
                order += 1;
            }
        }
    }
    let mut scored: Vec<(f32, usize, DocSummary)> = acc.into_values().collect();
    // Stable order: score desc, then first-appearance asc. A total
    // comparator (NaN treated equal) keeps it deterministic.
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(score, _, mut doc)| {
            doc.score = Some(score);
            doc
        })
        .collect()
}

/// Reciprocal-rank-fuse N *tagged* arms into a single ranking — the
/// federated (`scope=all`) counterpart of [`rrf_fuse`].
///
/// [`rrf_fuse`] dedups purely on [`DocSummary::id`], which assumes every
/// id is globally unique. That assumption is FALSE across corpora:
/// [`kb_core::ids::ArtifactId::from_path`] hashes only the source-relative
/// path, so two kbs sharing a rel path (a shared root `index.html`
/// template is the common case) hash to the SAME id. Each arm here
/// carries an explicit `key` (the caller's choice — the owning kb name is
/// the federated-search use case); the accumulator dedups on `(key,
/// doc.id)`, so two arms with the SAME key still merge their score
/// contributions exactly like `rrf_fuse` (e.g. a corpus's own bm25 +
/// vector arms, already fused into one arm per corpus by the caller
/// before reaching here), while two arms with DIFFERENT keys never merge
/// even when `doc.id` collides — each keeps its own row in the output.
///
/// Otherwise identical to `rrf_fuse`: `Σ 1/(k + rank)` per arm, stable
/// sort by score desc then first-appearance asc, truncated to `limit`.
/// The returned tuple's key is the arm's tag; ties break by first
/// appearance across `arms` in submission order (same contract as
/// `rrf_fuse` — pass arms in a deterministic order, e.g. `state.kbs`'s
/// `BTreeMap` order, per invariant #28).
pub fn rrf_fuse_keyed<K: Clone + Eq + std::hash::Hash>(
    arms: Vec<(K, Vec<DocSummary>)>,
    k: f32,
    limit: usize,
) -> Vec<(K, DocSummary)> {
    use std::collections::HashMap;
    // (key, id) -> (accumulated score, first-appearance ordinal, key, hit).
    let mut acc: HashMap<(K, String), (f32, usize, K, DocSummary)> = HashMap::new();
    let mut order = 0usize;
    for (key, arm) in arms {
        for (rank, doc) in arm.into_iter().enumerate() {
            let contrib = 1.0 / (k + rank as f32);
            let acc_key = (key.clone(), doc.id.clone());
            if let Some(entry) = acc.get_mut(&acc_key) {
                entry.0 += contrib;
            } else {
                acc.insert(acc_key, (contrib, order, key.clone(), doc));
                order += 1;
            }
        }
    }
    let mut scored: Vec<(f32, usize, K, DocSummary)> = acc.into_values().collect();
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(score, _, key, mut doc)| {
            doc.score = Some(score);
            (key, doc)
        })
        .collect()
}

/// Default title-match boost. A flat BM25 over `[title, body, headings,
/// code, prompt]` weights a title hit no higher than a body hit, but a
/// query term appearing in the *title* is a strong relevance signal.
/// This lifts such hits after fusion. Tunable; `0.0` disables.
pub const TITLE_BOOST: f32 = 0.5;

/// Boost fused hits whose title matches a query term, then re-sort.
///
/// A hit whose lowercased `title` contains any query token of length ≥ 3
/// has its [`DocSummary::score`] multiplied by `1.0 + factor`. Pure and
/// deterministic; the re-sort is stable, so equal-scored hits keep their
/// fused order. Applied to the full candidate pool *before* the route
/// truncates to `limit`, so a title match buried just past `limit` can
/// still be lifted into the returned page.
pub fn apply_title_boost(hits: &mut [DocSummary], query: &str, factor: f32) {
    if factor == 0.0 {
        return;
    }
    let terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 3)
        .map(|t| t.to_ascii_lowercase())
        .collect();
    if terms.is_empty() {
        return;
    }
    for h in hits.iter_mut() {
        // Substring (not word-boundary) match — deliberate: it catches
        // morphological variants (query "embedding" → title "Embeddings")
        // that lift relevance here. A word-boundary variant bench-measured
        // WORSE on kb-docs (base/large −0.10 R@1, 2026-06-06) because it
        // misses those. Revisit only if a larger corpus shows over-boosting
        // from incidental substrings (re-bench before changing).
        let title = h.title.to_ascii_lowercase();
        if terms.iter().any(|t| title.contains(t.as_str())) {
            if let Some(s) = h.score.as_mut() {
                *s *= 1.0 + factor;
            }
        }
    }
    // Stable sort by score desc — preserves the fused tie-break order for
    // hits that end up equal.
    hits.sort_by(|a, b| {
        b.score
            .unwrap_or(0.0)
            .partial_cmp(&a.score.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// GS-track — graph-degree ranking signal (opt-in via `[kb.*] graph_boost`).
///
/// In-degree (backlinks) is the endorsement axis of the edge graph: being
/// *linked to* is a curation signal the corpus already computes
/// (`Storage::edge_counts`, the same map behind the gallery's
/// backlinks/outlinks chips). Out-degree is deliberately ignored — an index
/// note that links everything must not rank itself up by fan-out.
///
/// The boost is **additive and scaled to the RRF regime**: the most-linked
/// artifact in the corpus gains `weight / RRF_K` (i.e. `weight` extra
/// top-rank arm contributions); everything else scales by
/// `sqrt(in) / sqrt(in_max)` for diminishing returns. `sqrt` and not `ln`
/// on purpose: IEEE-754 sqrt is correctly-rounded (bit-identical across
/// platforms, the atlas-determinism op set — kb-core invariant #3), while
/// `ln` is libm-implementation-defined and can flip equal-score ties
/// across glibc/musl.
///
/// Pure and deterministic; the re-sort is stable (equal scores keep their
/// fused order), mirroring [`apply_title_boost`]. Applied to the full
/// candidate pool before truncation, so a well-linked hit just past
/// `limit` can still surface. When the SQ4 reranker is enabled it
/// overwrites scores afterwards — the boost then only shapes which
/// candidates enter the rerank window.
pub fn apply_graph_boost(
    hits: &mut [DocSummary],
    degrees: &std::collections::HashMap<String, (u32, u32)>,
    weight: f32,
) {
    if weight == 0.0 || hits.is_empty() {
        return;
    }
    // Corpus-wide max in-degree (the map covers the whole kb) keeps the
    // normalisation independent of the query's candidate pool.
    let in_max = degrees.values().map(|&(_, inb)| inb).max().unwrap_or(0);
    if in_max == 0 {
        return;
    }
    let denom = (in_max as f32).sqrt();
    for h in hits.iter_mut() {
        let inb = degrees.get(&h.id).map(|&(_, i)| i).unwrap_or(0);
        if inb == 0 {
            continue;
        }
        if let Some(s) = h.score.as_mut() {
            *s += weight * (1.0 / RRF_K) * ((inb as f32).sqrt() / denom);
        }
    }
    // Stable sort by score desc — same discipline as `apply_title_boost`:
    // equal-scored hits keep the deterministic fused order.
    hits.sort_by(|a, b| {
        b.score
            .unwrap_or(0.0)
            .partial_cmp(&a.score.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(id: &str) -> DocSummary {
        DocSummary {
            id: id.to_string(),
            ..Default::default()
        }
    }

    fn doc_titled(id: &str, title: &str) -> DocSummary {
        DocSummary {
            id: id.to_string(),
            title: title.to_string(),
            ..Default::default()
        }
    }

    fn ids(hits: &[DocSummary]) -> Vec<&str> {
        hits.iter().map(|h| h.id.as_str()).collect()
    }

    #[test]
    fn single_arm_preserves_order() {
        let arm = vec![doc("a"), doc("b"), doc("c")];
        let out = rrf_fuse(vec![arm], RRF_K, 10);
        assert_eq!(ids(&out), vec!["a", "b", "c"]);
    }

    #[test]
    fn doc_in_both_arms_outranks_doc_in_one() {
        // `b` is rank 1 in both arms; `a` is rank 0 in only the first.
        // a: 1/60 = 0.01667. b: 1/61 + 1/61 = 0.03279. b wins.
        let v = vec![doc("a"), doc("b")];
        let f = vec![doc("x"), doc("b")];
        let out = rrf_fuse(vec![v, f], RRF_K, 10);
        assert_eq!(out[0].id, "b", "doc in both arms should rank first");
    }

    #[test]
    fn fused_score_is_set_on_hits() {
        let out = rrf_fuse(vec![vec![doc("a")]], RRF_K, 10);
        assert!((out[0].score.unwrap() - 1.0 / 60.0).abs() < 1e-6);
    }

    #[test]
    fn tie_breaks_by_first_appearance_across_arms_vector_first() {
        // `a` (vector rank 0) and `b` (fts rank 0) both score 1/60.
        // lance concatenates [vector, fts] and keeps first occurrence,
        // so the vector-arm hit wins the tie.
        let vector = vec![doc("a")];
        let fts = vec![doc("b")];
        let out = rrf_fuse(vec![vector, fts], RRF_K, 10);
        assert_eq!(ids(&out), vec!["a", "b"], "vector arm wins equal-score tie");
    }

    #[test]
    fn dedup_keeps_one_row_per_id() {
        let v = vec![doc("a"), doc("b")];
        let f = vec![doc("a"), doc("b")];
        let out = rrf_fuse(vec![v, f], RRF_K, 10);
        assert_eq!(out.len(), 2, "ids must be deduplicated across arms");
    }

    #[test]
    fn limit_truncates_after_fusion() {
        let arm = vec![doc("a"), doc("b"), doc("c"), doc("d")];
        let out = rrf_fuse(vec![arm], RRF_K, 2);
        assert_eq!(ids(&out), vec!["a", "b"]);
    }

    #[test]
    fn empty_arms_yield_empty() {
        let out = rrf_fuse(vec![], RRF_K, 10);
        assert!(out.is_empty());
        let out = rrf_fuse(vec![vec![], vec![]], RRF_K, 10);
        assert!(out.is_empty());
    }

    #[test]
    fn over_fetch_surfaces_a_doc_lance_would_have_dropped() {
        // Simulates the over-fetch win: with a final limit of 1, lance
        // would fetch only the rank-0 of each arm (`a` from vector, `x`
        // from fts) and never see `b`. With a pool of 3, `b` appears at
        // rank 1 in BOTH arms and its summed score beats the singletons.
        // a: 1/60=.01667  x: 1/60=.01667  b: 1/61+1/61=.03279
        let vector = vec![doc("a"), doc("b"), doc("c")];
        let fts = vec![doc("x"), doc("b"), doc("y")];
        let out = rrf_fuse(vec![vector, fts], RRF_K, 1);
        assert_eq!(out[0].id, "b", "doc strong in both arms should win");
    }

    // ---- rrf_fuse_keyed — federated cross-corpus dedup -----------------

    #[test]
    fn keyed_fuse_matches_plain_fuse_when_every_arm_shares_one_key() {
        // Golden — with a single key for every arm, rrf_fuse_keyed must
        // reproduce plain rrf_fuse's ranking + score byte-for-byte (it's
        // strictly the same accumulator, just with an always-equal extra
        // key component).
        let v = vec![doc("a"), doc("b")];
        let f = vec![doc("x"), doc("b")];
        let plain = rrf_fuse(vec![v.clone(), f.clone()], RRF_K, 10);
        let keyed = rrf_fuse_keyed(vec![("k", v), ("k", f)], RRF_K, 10);
        assert_eq!(
            keyed
                .iter()
                .map(|entry| entry.1.id.clone())
                .collect::<Vec<_>>(),
            plain.iter().map(|d| d.id.clone()).collect::<Vec<_>>(),
        );
        for (kentry, pd) in keyed.iter().zip(plain.iter()) {
            assert_eq!(kentry.0, "k");
            assert!((kentry.1.score.unwrap() - pd.score.unwrap()).abs() < 1e-6);
        }
    }

    #[test]
    fn keyed_fuse_keeps_same_id_distinct_across_different_keys() {
        // The federated-search bug this exists to fix: two corpora ("alpha",
        // "beta") each contributing a rank-0 hit with the SAME `doc.id`
        // (a path-hash collision — invariant DCB/host-grammar-v2) must
        // surface as TWO rows, each keeping its own arm's full score —
        // never merged into one, never one silently dropped.
        let alpha = vec![doc("0eb547304658")];
        let beta = vec![doc("0eb547304658")];
        let out = rrf_fuse_keyed(vec![("alpha", alpha), ("beta", beta)], RRF_K, 10);
        assert_eq!(out.len(), 2, "both corpora's hits must survive: {out:?}");
        let keys: Vec<&str> = out.iter().map(|entry| entry.0).collect();
        assert_eq!(keys, vec!["alpha", "beta"], "submission-order tie-break");
        for entry in &out {
            assert_eq!(entry.1.id, "0eb547304658");
            assert!(
                (entry.1.score.unwrap() - 1.0 / RRF_K).abs() < 1e-6,
                "each keeps its own rank-0 score, not a merged sum"
            );
        }
    }

    #[test]
    fn keyed_fuse_still_merges_same_key_arms() {
        // Two arms sharing a key (e.g. a corpus's own bm25 + vector arms)
        // merge exactly like plain rrf_fuse's within-corpus behaviour.
        let vector = vec![doc("a"), doc("b")];
        let bm25 = vec![doc("x"), doc("b")];
        let out = rrf_fuse_keyed(vec![("alpha", vector), ("alpha", bm25)], RRF_K, 10);
        let b_score = out
            .iter()
            .find(|entry| entry.0 == "alpha" && entry.1.id == "b")
            .map(|entry| entry.1.score.unwrap())
            .expect("b present");
        assert!(
            (b_score - (1.0 / 61.0 + 1.0 / 61.0)).abs() < 1e-6,
            "same-key arms must sum contributions like rrf_fuse"
        );
    }

    #[test]
    fn keyed_fuse_limit_and_empty() {
        let out = rrf_fuse_keyed::<&str>(vec![], RRF_K, 10);
        assert!(out.is_empty());
        let arm = vec![doc("a"), doc("b"), doc("c")];
        let out = rrf_fuse_keyed(vec![("k", arm)], RRF_K, 2);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn title_boost_lifts_a_title_match_over_a_higher_scored_non_match() {
        // `a` outscores `b` slightly before boosting; `b`'s title matches
        // the query and `a`'s does not, so the boost flips the order.
        let mut hits = vec![
            {
                let mut d = doc_titled("a", "unrelated heading");
                d.score = Some(0.020);
                d
            },
            {
                let mut d = doc_titled("b", "Server-Sent Events in axum");
                d.score = Some(0.018);
                d
            },
        ];
        apply_title_boost(&mut hits, "server-sent events streaming", TITLE_BOOST);
        assert_eq!(hits[0].id, "b", "title match should be lifted to the top");
        // b: 0.018 * 1.5 = 0.027 > a: 0.020 (unchanged).
        assert!((hits[0].score.unwrap() - 0.027).abs() < 1e-6);
        assert!((hits[1].score.unwrap() - 0.020).abs() < 1e-6);
    }

    #[test]
    fn title_boost_zero_factor_is_a_noop() {
        let mut hits = vec![{
            let mut d = doc_titled("a", "axum SSE");
            d.score = Some(0.02);
            d
        }];
        apply_title_boost(&mut hits, "axum", 0.0);
        assert!((hits[0].score.unwrap() - 0.02).abs() < 1e-9);
    }

    fn degrees(pairs: &[(&str, u32, u32)]) -> std::collections::HashMap<String, (u32, u32)> {
        pairs
            .iter()
            .map(|(id, o, i)| (id.to_string(), (*o, *i)))
            .collect()
    }

    #[test]
    fn graph_boost_lifts_a_linked_doc_over_a_slightly_higher_unlinked_one() {
        // `a` outscores `b` by less than the max boost; `b` is the
        // most-linked artifact (full weight/60), `a` has no backlinks.
        let mut hits = vec![
            {
                let mut d = doc("a");
                d.score = Some(0.0200);
                d
            },
            {
                let mut d = doc("b");
                d.score = Some(0.0180);
                d
            },
        ];
        let deg = degrees(&[("b", 0, 9)]);
        apply_graph_boost(&mut hits, &deg, 1.0);
        // b: 0.018 + 1.0/60 * sqrt(9)/sqrt(9) = 0.018 + 0.01667 = 0.03467 > a.
        assert_eq!(hits[0].id, "b");
        assert!((hits[0].score.unwrap() - (0.018 + 1.0 / 60.0)).abs() < 1e-6);
        assert!(
            (hits[1].score.unwrap() - 0.020).abs() < 1e-9,
            "unlinked hit unchanged"
        );
    }

    #[test]
    fn graph_boost_scales_by_sqrt_of_indegree_ignores_outdegree() {
        let mut hits = vec![
            {
                let mut d = doc("quarter");
                d.score = Some(0.010);
                d
            },
            {
                let mut d = doc("fanout");
                d.score = Some(0.010);
                d
            },
        ];
        // in_max = 16 (held by an id not even in the pool). `quarter` has
        // in=4 → sqrt(4)/sqrt(16) = 0.5 of the full boost. `fanout` has a
        // huge OUT-degree and zero in — no boost.
        let deg = degrees(&[("hub", 0, 16), ("quarter", 0, 4), ("fanout", 99, 0)]);
        apply_graph_boost(&mut hits, &deg, 1.0);
        let q = hits.iter().find(|h| h.id == "quarter").unwrap();
        let f = hits.iter().find(|h| h.id == "fanout").unwrap();
        assert!((q.score.unwrap() - (0.010 + 0.5 / 60.0)).abs() < 1e-6);
        assert!(
            (f.score.unwrap() - 0.010).abs() < 1e-9,
            "out-degree must not boost"
        );
    }

    #[test]
    fn graph_boost_zero_weight_or_empty_graph_is_a_noop() {
        let mk = || {
            vec![{
                let mut d = doc("a");
                d.score = Some(0.02);
                d
            }]
        };
        let mut hits = mk();
        apply_graph_boost(&mut hits, &degrees(&[("a", 0, 5)]), 0.0);
        assert!((hits[0].score.unwrap() - 0.02).abs() < 1e-9);
        let mut hits = mk();
        apply_graph_boost(&mut hits, &degrees(&[]), 1.0);
        assert!((hits[0].score.unwrap() - 0.02).abs() < 1e-9);
        // A graph with edges but zero in-degree anywhere (impossible in
        // practice — every edge has a dst — but the guard is cheap).
        let mut hits = mk();
        apply_graph_boost(&mut hits, &degrees(&[("x", 3, 0)]), 1.0);
        assert!((hits[0].score.unwrap() - 0.02).abs() < 1e-9);
    }

    #[test]
    fn graph_boost_equal_scores_keep_fused_order() {
        // Neither hit has backlinks ⇒ scores unchanged ⇒ the stable sort
        // must preserve the incoming (fused) order exactly.
        let mut hits = vec![
            {
                let mut d = doc("first");
                d.score = Some(0.02);
                d
            },
            {
                let mut d = doc("second");
                d.score = Some(0.02);
                d
            },
        ];
        apply_graph_boost(&mut hits, &degrees(&[("elsewhere", 0, 2)]), 1.0);
        assert_eq!(ids(&hits), vec!["first", "second"]);
    }

    #[test]
    fn title_boost_ignores_short_tokens() {
        // "kb" (len 2) must not boost — too common a token to discriminate.
        let mut hits = vec![{
            let mut d = doc_titled("a", "kb research");
            d.score = Some(0.02);
            d
        }];
        apply_title_boost(&mut hits, "kb", TITLE_BOOST);
        assert!(
            (hits[0].score.unwrap() - 0.02).abs() < 1e-9,
            "2-char token should not trigger a boost"
        );
    }
}
