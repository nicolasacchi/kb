//! Deterministic per-cluster labels for the atlas — c-TF-IDF (BERTopic's
//! "class-based TF-IDF"), computed at atlas recompute/recluster time
//! (W1.B). Pure + LLM-free (root non-goal: no in-daemon LLM); the whole
//! label surface is arithmetic over tokenised `title`/`summary`/`tags`
//! text, deterministic given the same `(id, cluster, title, summary,
//! tags)` input set.
//!
//! ## The formula
//!
//! For a term `t` and cluster `c`:
//!
//! ```text
//! score(t, c) = tf(t, c) * ln(1 + A / f(t))
//! ```
//!
//! - `tf(t, c)` — how many times `t` appears across every doc in cluster `c`.
//! - `f(t)` — how many times `t` appears across ALL clusters (the corpus total).
//! - `A` — mean token count per cluster (total corpus tokens / cluster count),
//!   one scalar shared by every `(t, c)` pair in a single [`compute`] call.
//!
//! Unigrams only for Wave 1 (bigrams are Wave 2). Top 5 terms per cluster,
//! ordered score desc / term lexicographic asc on ties.
//!
//! ## Determinism (crates/kb-core/CLAUDE.md invariant #3)
//!
//! Same invariant atlas coordinates lean on: bit-identical output for the
//! same input, independent of input order or wall-clock time.
//! - Input is defensively sorted by `id` on entry — [`compute`] does not
//!   trust its caller to have already canonicalised order (belt-and-braces,
//!   same posture `atlas::recompute_for_kb_with` takes at its own boundary).
//! - Every aggregation is a `BTreeMap` (sorted-key iteration), never a
//!   `HashMap` — f64 accumulation therefore always visits terms in the same
//!   (lexicographic) order run to run, machine to machine.
//! - No clock, no randomness anywhere in this module. `computed_at` — a
//!   pure bookkeeping timestamp, not an input to the math — is threaded in
//!   by the caller (kb-server's atlas route) rather than read here; see the
//!   wiring in `atlas::recompute_for_kb_with`/`recluster_for_kb_with`.

use std::collections::BTreeMap;

/// One document's label-relevant text, as seen by [`compute`]. `cluster`
/// mirrors `AtlasPoint::cluster` (kb atlas cluster ids are always `>= 0`
/// in practice, but the type stays `i16` to match the lance column and
/// `AtlasPoint`).
#[derive(Debug, Clone, PartialEq)]
pub struct LabelDoc {
    pub id: String,
    pub cluster: i16,
    pub title: String,
    pub summary: String,
    pub tags: Vec<String>,
}

/// One ranked term for a cluster, carrying the full c-TF-IDF decomposition
/// (`tf`, `ft`, `score`) so a caller — the SPA atlas inspector — can render
/// `tf × ln(1 + A/ft) = score` verbatim instead of re-deriving it.
#[derive(Debug, Clone, PartialEq)]
pub struct TermScore {
    pub term: String,
    pub tf: f64,
    pub ft: f64,
    pub score: f64,
}

/// Top-N (`TOP_N`) c-TF-IDF terms for one atlas cluster. `avg_tokens` is
/// the shared `A` constant for the whole [`compute`] call, repeated on
/// every cluster — `compute` returns a flat `Vec<ClusterLabels>` (no
/// wrapper envelope), so this is where "expose A" lands per-cluster
/// rather than at a response-level field that doesn't exist here (the
/// HTTP route composes that response-level field itself).
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterLabels {
    pub cluster: i16,
    pub terms: Vec<TermScore>,
    pub avg_tokens: f64,
}

/// Ranked terms kept per cluster.
const TOP_N: usize = 5;

/// Minimum token length. Filters out 1-2 char noise (ids, units) without a
/// language-aware stemmer — kb's authoring guide (docs/authoring-artifacts.md)
/// doesn't mandate any particular language, so this stays a length gate, not
/// a POS filter.
const MIN_TOKEN_LEN: usize = 3;

/// Small, deliberately short English stopword list (~50 entries). Wave 1
/// unigrams-only scope: no locale/language detection, no stemming — just
/// enough closed-class noise removal that a cluster's top terms read as
/// content words. Sorted for readability; membership check is linear (the
/// list is tiny, so this stays cheap and — unlike a `HashSet` — needs no
/// hasher, though determinism doesn't actually depend on that here since
/// membership testing has no observable iteration order).
const STOPWORDS: &[&str] = &[
    "about", "after", "again", "against", "all", "also", "and", "any", "are", "been", "before",
    "being", "between", "both", "but", "can", "does", "doing", "down", "during", "each", "few",
    "for", "from", "further", "had", "has", "have", "having", "here", "how", "into", "its", "just",
    "more", "most", "not", "now", "off", "once", "only", "other", "our", "out", "over", "own",
    "same", "she", "should", "some", "such", "than", "that", "the", "their", "them", "then",
    "there", "these", "they", "this", "those", "through", "too", "under", "until", "very", "was",
    "were", "what", "when", "where", "which", "while", "will", "with", "would", "your",
];

/// Tokenize `text`: Unicode-safe lowercase, split on non-alphanumeric
/// boundaries, keep tokens with `>= MIN_TOKEN_LEN` chars (counted, not
/// bytes — Unicode-safe for accented input), drop stopwords. Unigrams
/// only — bigrams are Wave 2.
fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .filter(|w| w.chars().count() >= MIN_TOKEN_LEN && !STOPWORDS.contains(&w.as_str()))
        .collect()
}

/// Compute deterministic c-TF-IDF labels for every cluster present in
/// `docs`. Empty input (or a corpus whose tokens are all stopwords/short)
/// yields an empty `Vec` — never panics.
pub fn compute(docs: &[LabelDoc]) -> Vec<ClusterLabels> {
    // GC-B1-style boundary canonicalisation: sort a local copy by id so the
    // accumulation order below is independent of the caller's iteration
    // order, even though `atlas::recompute_for_kb_with`/`recluster_for_kb_with`
    // already hand us id-sorted input — this module doesn't trust that.
    let mut sorted: Vec<&LabelDoc> = docs.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));

    // Per-cluster term counts, the corpus-wide term total, and per-cluster
    // token totals — all `BTreeMap` so every downstream iteration visits
    // keys in sorted order (never HashMap iteration order; invariant #3).
    let mut per_cluster: BTreeMap<i16, BTreeMap<String, u64>> = BTreeMap::new();
    let mut global: BTreeMap<String, u64> = BTreeMap::new();
    let mut cluster_token_totals: BTreeMap<i16, u64> = BTreeMap::new();

    for doc in &sorted {
        let mut text =
            String::with_capacity(doc.title.len() + doc.summary.len() + doc.tags.len() * 8 + 8);
        text.push_str(&doc.title);
        text.push(' ');
        text.push_str(&doc.summary);
        for tag in &doc.tags {
            text.push(' ');
            text.push_str(tag);
        }
        let tokens = tokenize(&text);
        *cluster_token_totals.entry(doc.cluster).or_insert(0) += tokens.len() as u64;
        let cluster_terms = per_cluster.entry(doc.cluster).or_default();
        for tok in tokens {
            *cluster_terms.entry(tok.clone()).or_insert(0) += 1;
            *global.entry(tok).or_insert(0) += 1;
        }
    }

    // A = mean token count per cluster — one scalar shared by every
    // (term, cluster) pair below. `cluster_token_totals.len()` (not
    // `per_cluster.len()`, though they're always equal — every cluster
    // that produced a token also produced a per_cluster entry) is the
    // cluster count; clamp to 1 so an all-stopword/empty corpus can't
    // divide by zero.
    let num_clusters = cluster_token_totals.len().max(1) as f64;
    let total_tokens: u64 = cluster_token_totals.values().sum();
    let avg_tokens = total_tokens as f64 / num_clusters;

    per_cluster
        .into_iter()
        .map(|(cluster, terms)| {
            // `terms` (a BTreeMap<String, u64>) yields lexicographic key
            // order via `into_iter()` — the fixed f64-accumulation order
            // invariant #3 asks for.
            let mut scored: Vec<TermScore> = terms
                .into_iter()
                .map(|(term, tf)| {
                    let tf_f = tf as f64;
                    // `ft` is always >= `tf` (every count folded into
                    // `cluster_terms` is folded into `global` in the same
                    // pass above), so this is never zero and the `ln`
                    // argument is always finite and > 1.
                    let ft = *global.get(&term).unwrap_or(&0) as f64;
                    let score = tf_f * (1.0 + avg_tokens / ft).ln();
                    TermScore {
                        term,
                        tf: tf_f,
                        ft,
                        score,
                    }
                })
                .collect();
            // Score desc; ties broken term lexicographic asc — a total
            // order since `term` is unique per cluster (BTreeMap keys).
            scored.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.term.cmp(&b.term))
            });
            scored.truncate(TOP_N);
            ClusterLabels {
                cluster,
                terms: scored,
                avg_tokens,
            }
        })
        .collect()
}

/// Recover the shared `A` (mean tokens per cluster) constant from one
/// stored `(tf, ft, score)` triple. `A` isn't persisted directly — the
/// `atlas_labels` sqlite table only carries the per-term decomposition
/// (crates/kb-core/migrations/V0027__atlas_labels.sql) — but every row
/// written by one [`compute`] call shares the exact same `A`, and
/// `score = tf * ln(1 + A/ft)` inverts cleanly: `A = ft * (exp(score/tf) -
/// 1)`. Used by the `GET /api/kb/{kb}/atlas/labels` route to fill the
/// response's top-level `avg_tokens` field from any one stored row.
/// Returns `0.0` for a degenerate `tf <= 0.0` (never produced by
/// `compute`, but keeps this helper total for a hand-crafted/corrupt row).
pub fn recover_avg_tokens(tf: f64, ft: f64, score: f64) -> f64 {
    if tf <= 0.0 {
        return 0.0;
    }
    ft * ((score / tf).exp() - 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(id: &str, cluster: i16, title: &str, summary: &str, tags: &[&str]) -> LabelDoc {
        LabelDoc {
            id: id.into(),
            cluster,
            title: title.into(),
            summary: summary.into(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn empty_corpus_yields_empty_labels() {
        assert_eq!(compute(&[]), Vec::<ClusterLabels>::new());
    }

    #[test]
    fn single_cluster_edge_case_does_not_panic() {
        let docs = vec![
            doc(
                "a",
                0,
                "rust ownership borrow checker",
                "memory safety",
                &[],
            ),
            doc(
                "b",
                0,
                "borrow checker rust lifetimes",
                "ownership rules",
                &[],
            ),
        ];
        let labels = compute(&docs);
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0].cluster, 0);
        assert!(!labels[0].terms.is_empty());
        assert!(labels[0].terms.len() <= TOP_N);
    }

    #[test]
    fn bit_identical_across_two_runs() {
        let docs = vec![
            doc(
                "a",
                0,
                "rust ownership borrow",
                "memory safety systems",
                &[],
            ),
            doc(
                "b",
                1,
                "python duck typing",
                "dynamic dispatch runtime",
                &[],
            ),
            doc("c", 0, "borrow checker lifetimes", "rust memory model", &[]),
            doc(
                "d",
                1,
                "python generators async",
                "coroutine scheduling",
                &[],
            ),
        ];
        let a = compute(&docs);
        let b = compute(&docs);
        assert_eq!(a, b);
    }

    #[test]
    fn shuffled_input_order_yields_identical_output() {
        let docs = vec![
            doc(
                "z",
                0,
                "rust ownership borrow",
                "memory safety systems",
                &[],
            ),
            doc(
                "m",
                1,
                "python duck typing",
                "dynamic dispatch runtime",
                &[],
            ),
            doc("a", 0, "borrow checker lifetimes", "rust memory model", &[]),
            doc(
                "q",
                1,
                "python generators async",
                "coroutine scheduling",
                &[],
            ),
        ];
        let mut shuffled = docs.clone();
        shuffled.reverse();
        let straight = compute(&docs);
        let reversed = compute(&shuffled);
        assert_eq!(straight, reversed, "input order must not affect output");
    }

    #[test]
    fn score_decomposition_identity_holds() {
        let docs = vec![
            doc(
                "a",
                0,
                "rust ownership borrow checker",
                "memory safety",
                &[],
            ),
            doc(
                "b",
                1,
                "python duck typing dynamic",
                "runtime dispatch",
                &[],
            ),
        ];
        let labels = compute(&docs);
        for cl in &labels {
            for t in &cl.terms {
                let expected = t.tf * (1.0 + cl.avg_tokens / t.ft).ln();
                assert!(
                    (t.score - expected).abs() < 1e-9,
                    "score decomposition mismatch for {:?}: {} vs {expected}",
                    t.term,
                    t.score
                );
            }
        }
    }

    #[test]
    fn utf8_accented_tokens_survive_tokenization() {
        // Italian text — accented + apostrophe-joined words must not be
        // mangled by a byte-oriented splitter.
        let docs = vec![
            doc("a", 0, "città perché così l'anima", "società italiana", &[]),
            doc("b", 0, "perché città bellissima", "società civile", &[]),
        ];
        let labels = compute(&docs);
        assert_eq!(labels.len(), 1);
        let terms: Vec<&str> = labels[0].terms.iter().map(|t| t.term.as_str()).collect();
        // "città" and "perché" repeat across both docs — they should rank
        // among the top terms, with accents intact (not stripped/mangled).
        assert!(
            terms.contains(&"città") || terms.contains(&"perché"),
            "expected accented Italian terms in {terms:?}"
        );
        for t in &terms {
            assert!(t.chars().count() >= MIN_TOKEN_LEN);
        }
    }

    #[test]
    fn short_and_stopword_tokens_are_dropped() {
        let docs = vec![doc(
            "a",
            0,
            "the a an is of it to",
            "at by no not too own",
            &[],
        )];
        let labels = compute(&docs);
        // Every token here is either < MIN_TOKEN_LEN or a stopword.
        assert_eq!(labels.len(), 1);
        assert!(labels[0].terms.is_empty());
    }

    #[test]
    fn top_n_capped_at_five_ordered_score_desc_term_asc() {
        // 8 distinct terms, each appearing a DIFFERENT number of times so
        // ties don't obscure the score-desc ordering, plus two same-count
        // terms (alpha/zeta) to probe the lexicographic tiebreak.
        let mut title = String::new();
        for (word, times) in [
            ("aardvark", 1),
            ("alpha", 2),
            ("bravo", 3),
            ("charlie", 4),
            ("delta", 5),
            ("echo", 6),
            ("foxtrot", 7),
            ("zeta", 2),
        ] {
            for _ in 0..times {
                title.push_str(word);
                title.push(' ');
            }
        }
        let docs = vec![doc("a", 0, &title, "", &[])];
        let labels = compute(&docs);
        assert_eq!(labels.len(), 1);
        let terms = &labels[0].terms;
        assert_eq!(terms.len(), TOP_N);
        // Highest tf (single-cluster corpus ⇒ tf drives score monotonically
        // for a fixed ft==tf) wins: foxtrot(7), echo(6), delta(5),
        // charlie(4), bravo(3) — aardvark/alpha/zeta are edged out.
        let ordered: Vec<&str> = terms.iter().map(|t| t.term.as_str()).collect();
        assert_eq!(
            ordered,
            vec!["foxtrot", "echo", "delta", "charlie", "bravo"]
        );
        for w in terms.windows(2) {
            assert!(w[0].score >= w[1].score, "terms must be score-descending");
        }
    }

    #[test]
    fn avg_tokens_is_shared_across_every_cluster() {
        let docs = vec![
            doc("a", 0, "rust ownership borrow checker", "memory", &[]),
            doc("b", 1, "python duck typing dynamic", "dispatch", &[]),
            doc(
                "c",
                2,
                "golang goroutines channels select",
                "concurrency",
                &[],
            ),
        ];
        let labels = compute(&docs);
        assert_eq!(labels.len(), 3);
        let a0 = labels[0].avg_tokens;
        for cl in &labels {
            assert_eq!(cl.avg_tokens, a0, "A must be identical across clusters");
        }
        assert!(a0 > 0.0);
    }

    #[test]
    fn recover_avg_tokens_round_trips_through_the_stored_decomposition() {
        let docs = vec![
            doc(
                "a",
                0,
                "rust ownership borrow",
                "memory safety systems",
                &[],
            ),
            doc(
                "b",
                1,
                "python duck typing",
                "dynamic dispatch runtime",
                &[],
            ),
            doc("c", 0, "borrow checker lifetimes", "rust memory model", &[]),
        ];
        let labels = compute(&docs);
        for cl in &labels {
            for t in &cl.terms {
                let recovered = recover_avg_tokens(t.tf, t.ft, t.score);
                assert!(
                    (recovered - cl.avg_tokens).abs() < 1e-6,
                    "recovered A {recovered} != original {}",
                    cl.avg_tokens
                );
            }
        }
    }

    #[test]
    fn recover_avg_tokens_degenerate_tf_is_zero() {
        assert_eq!(recover_avg_tokens(0.0, 5.0, 1.0), 0.0);
        assert_eq!(recover_avg_tokens(-1.0, 5.0, 1.0), 0.0);
    }

    #[test]
    fn tags_and_title_and_summary_all_contribute_tokens() {
        let docs = vec![doc(
            "a",
            0,
            "artichoke",
            "brontosaurus",
            &["crustacean", "dandelion"],
        )];
        let labels = compute(&docs);
        let terms: std::collections::HashSet<&str> =
            labels[0].terms.iter().map(|t| t.term.as_str()).collect();
        assert!(terms.contains("artichoke"));
        assert!(terms.contains("brontosaurus"));
        assert!(terms.contains("crustacean"));
        assert!(terms.contains("dandelion"));
    }
}
