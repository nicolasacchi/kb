//! `kb-code bench-search` — W5.5: a retrieval-quality + latency instrument
//! over the unified Search-Everywhere box (`GET /api/search`,
//! `kb_code_server::search::unified`). Pure scoring lives here (unit-tested
//! against a fixture `serde_json::Value` result set, no daemon needed); the
//! HTTP round-trips + CLI rendering are `main.rs`'s `bench_search_cmd`.
//!
//! # Query file format
//!
//! One JSON object per line (blank lines and `#`-prefixed comment lines
//! skipped): `{"query": "...", "expect_path": "...", "expect_kind":
//! "..."}`. `expect_kind` is OPTIONAL and purely a free-form label for the
//! human table's grouping (e.g. `"symbol"`/`"semantic"`/`"error-string"`/
//! `"filename"`) — it does NOT affect scoring. A query's own text is
//! expected to use kb-code's search grammar (`@sym`, `#file`, `/regex/`,
//! `?nl ...`, bare = every lane — see `kb_code_server::search::grammar`)
//! to select which lane(s) it exercises.
//!
//! # Scoring
//!
//! Each query runs exactly ONE unified `/api/search` call — `search::
//! unified::run`'s own `tokio::join!` already runs every SELECTED lane
//! concurrently (bounded by the slowest lane, per that module's own doc),
//! so one round trip covers every lane the query's grammar selected. Every
//! lane PRESENT in the response (not `unavailable_reason`, not `pending`)
//! is scored: a HIT at rank k if some hit within the first k results
//! matches `expect_path` under [`hit_rank`]'s ladder (1-based) — exact on
//! `path`/`session_id`, containment on `title`/`snippet`, so the
//! sessions/transcripts lanes are scoreable at all (V71-D1; they always
//! read 0% before). recall@1/@5 are
//! aggregated PER LANE (denominator = queries where that lane was present)
//! and OVERALL (denominator = every query; a hit if ANY present lane hit
//! at that k). Latency: the WHOLE round-trip wall time is attributed to
//! EVERY lane present in that response — a documented approximation (the
//! lanes ran concurrently server-side; kb-code-server exposes no per-lane
//! server-side timing), not a claim of isolated per-lane measurement.

use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct BenchQuery {
    pub query: String,
    pub expect_path: String,
    #[serde(default)]
    pub expect_kind: Option<String>,
}

/// Parse a `.jsonl` query file's non-blank, non-`#`-comment lines. The
/// error string names the (1-based) offending line — this is a bench
/// operator tool, a loud parse failure beats silently skipping a broken
/// row.
pub fn parse_queries(text: &str) -> Result<Vec<BenchQuery>, String> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let q: BenchQuery =
            serde_json::from_str(line).map_err(|e| format!("line {}: {e}", i + 1))?;
        out.push(q);
    }
    Ok(out)
}

/// One lane's outcome for one query — see the module doc's "Scoring".
#[derive(Debug, Clone, PartialEq)]
pub struct LaneHit {
    pub lane: String,
    /// 1-based rank of the first hit whose `path` equals `expect_path`;
    /// `None` if it never appears among the lane's returned results.
    pub rank: Option<usize>,
}

/// `expect_path`'s 1-based rank within a lane section's `results` array —
/// `None` if absent.
///
/// V71-D1 — the match key used to be `path` ALONE, which made this
/// instrument structurally blind to two of the six lanes: `SessionHit` and
/// `TranscriptHit` carry no `path` field, so those lanes always read 0%
/// recall no matter how well they retrieved (recon/search.md §4 gap 20 —
/// "the instrument that gates the semantic-default flip is structurally
/// blind to a third of the box"). The key is now a LADDER over the fields a
/// hit can plausibly be addressed by, tried in order:
///
/// 1. `path` — an exact, whole-field match (files/symbols/text/semantic:
///    unchanged, byte-for-byte, so every recorded run stays comparable);
/// 2. `session_id` — an exact match (sessions/transcripts);
/// 3. `title` / `snippet` — a case-insensitive SUBSTRING containment, the
///    only honest key for a lane whose hit is a piece of prose.
///
/// The substring rung is deliberately the LAST one and deliberately not
/// applied to `path`: a bench that scored `app/models/order.rb` as a hit
/// because some other file's snippet mentioned it would be measuring
/// nothing. `expect_path` keeps its name (it is the query file's field) even
/// though it now expresses "the thing this query should surface".
pub fn hit_rank(results: &[serde_json::Value], expect_path: &str) -> Option<usize> {
    let want_lower = expect_path.to_lowercase();
    results
        .iter()
        .position(|h| {
            if h.get("path").and_then(|v| v.as_str()) == Some(expect_path) {
                return true;
            }
            if h.get("session_id").and_then(|v| v.as_str()) == Some(expect_path) {
                return true;
            }
            ["title", "snippet"].iter().any(|field| {
                h.get(*field)
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| s.to_lowercase().contains(&want_lower))
            })
        })
        .map(|i| i + 1)
}

/// A single query's full multi-lane outcome — what one live HTTP round
/// trip produces (via [`score_response`]), or a hand-built test fixture.
#[derive(Debug, Clone)]
pub struct QueryOutcome {
    pub latency_ms: f64,
    /// One entry per lane PRESENT in the response (unavailable/pending
    /// lanes are simply absent — see the module doc).
    pub lanes: Vec<LaneHit>,
}

/// Build a [`QueryOutcome`] from a raw unified `/api/search` JSON body
/// (`{"sections": [...], "query_echo": "..."}`) — pure, no I/O.
pub fn score_response(
    body: &serde_json::Value,
    expect_path: &str,
    latency_ms: f64,
) -> QueryOutcome {
    let mut lanes = Vec::new();
    if let Some(sections) = body.get("sections").and_then(|v| v.as_array()) {
        for sec in sections {
            if sec.get("unavailable_reason").is_some() {
                continue;
            }
            if sec
                .get("pending")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                continue;
            }
            let lane = sec
                .get("lane")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string();
            let results = sec
                .get("results")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let rank = hit_rank(&results, expect_path);
            lanes.push(LaneHit { lane, rank });
        }
    }
    QueryOutcome { latency_ms, lanes }
}

/// Running recall/latency aggregation for one lane (or the `overall`
/// any-lane roll-up) — see [`BenchReport`].
#[derive(Debug, Clone, Default)]
pub struct LaneAgg {
    pub total: usize,
    pub hit_at_1: usize,
    pub hit_at_5: usize,
    pub latencies_ms: Vec<f64>,
}

impl LaneAgg {
    fn record(&mut self, rank: Option<usize>, latency_ms: f64) {
        self.total += 1;
        if let Some(r) = rank {
            if r <= 1 {
                self.hit_at_1 += 1;
            }
            if r <= 5 {
                self.hit_at_5 += 1;
            }
        }
        self.latencies_ms.push(latency_ms);
    }

    pub fn recall_at_1(&self) -> f64 {
        ratio(self.hit_at_1, self.total)
    }

    pub fn recall_at_5(&self) -> f64 {
        ratio(self.hit_at_5, self.total)
    }

    pub fn p50_ms(&self) -> f64 {
        percentile(&self.latencies_ms, 0.50)
    }

    pub fn p95_ms(&self) -> f64 {
        percentile(&self.latencies_ms, 0.95)
    }
}

fn ratio(n: usize, d: usize) -> f64 {
    if d == 0 {
        0.0
    } else {
        n as f64 / d as f64
    }
}

/// Nearest-rank percentile over an UNSORTED sample (sorts a clone). `p` in
/// `[0.0, 1.0]`; empty input -> `0.0`.
pub fn percentile(samples: &[f64], p: f64) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = ((p * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    sorted[rank - 1]
}

/// The whole bench run's aggregate: per-lane stats (in a deterministic,
/// alphabetically-sorted `BTreeMap`) + the OVERALL (any-selected-lane)
/// numbers.
#[derive(Debug, Clone, Default)]
pub struct BenchReport {
    pub lanes: BTreeMap<String, LaneAgg>,
    pub overall: LaneAgg,
    pub queries: usize,
}

impl BenchReport {
    pub fn record(&mut self, outcome: &QueryOutcome) {
        self.queries += 1;
        let mut any_hit_1 = false;
        let mut any_hit_5 = false;
        for lh in &outcome.lanes {
            let agg = self.lanes.entry(lh.lane.clone()).or_default();
            agg.record(lh.rank, outcome.latency_ms);
            if let Some(r) = lh.rank {
                if r <= 1 {
                    any_hit_1 = true;
                }
                if r <= 5 {
                    any_hit_5 = true;
                }
            }
        }
        self.overall.total += 1;
        if any_hit_1 {
            self.overall.hit_at_1 += 1;
        }
        if any_hit_5 {
            self.overall.hit_at_5 += 1;
        }
        self.overall.latencies_ms.push(outcome.latency_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(lane: &str, paths: &[&str]) -> serde_json::Value {
        let results: Vec<serde_json::Value> = paths
            .iter()
            .map(|p| serde_json::json!({ "path": p }))
            .collect();
        serde_json::json!({ "lane": lane, "results": results, "truncated": false })
    }

    #[test]
    fn parse_queries_skips_blank_and_comment_lines() {
        let text = "\n# a comment\n{\"query\":\"foo\",\"expect_path\":\"a.rs\"}\n   \n{\"query\":\"bar\",\"expect_path\":\"b.rs\",\"expect_kind\":\"symbol\"}\n";
        let qs = parse_queries(text).unwrap();
        assert_eq!(qs.len(), 2);
        assert_eq!(qs[0].query, "foo");
        assert_eq!(qs[1].expect_kind.as_deref(), Some("symbol"));
    }

    #[test]
    fn parse_queries_reports_the_offending_line_number() {
        let text = "{\"query\":\"ok\",\"expect_path\":\"a.rs\"}\nnot json\n";
        let err = parse_queries(text).unwrap_err();
        assert!(err.starts_with("line 2:"), "got: {err}");
    }

    #[test]
    fn hit_rank_finds_exact_path_match_one_based() {
        let results = vec![
            serde_json::json!({"path": "src/a.rs"}),
            serde_json::json!({"path": "src/b.rs"}),
            serde_json::json!({"path": "src/c.rs"}),
        ];
        assert_eq!(hit_rank(&results, "src/b.rs"), Some(2));
        assert_eq!(hit_rank(&results, "src/zzz.rs"), None);
    }

    /// V71-D1 — the pathless lanes are no longer structurally unscoreable
    /// (recon gap 20). The `path` rung is unchanged for every lane that has
    /// one, so recorded runs stay comparable.
    #[test]
    fn hit_rank_scores_the_pathless_lanes_by_session_id_title_or_snippet() {
        assert_eq!(
            hit_rank(&[serde_json::json!({"session_id": "s1"})], "s1"),
            Some(1)
        );
        assert_eq!(
            hit_rank(
                &[serde_json::json!({"title": "Fixed the Gizmo race"})],
                "gizmo race"
            ),
            Some(1)
        );
        assert_eq!(
            hit_rank(
                &[serde_json::json!({"snippet": "we changed app/jobs/billing.rb"})],
                "app/jobs/billing.rb"
            ),
            Some(1)
        );
        // A hit with none of the four fields still misses, and a `path`
        // match stays EXACT — never a substring, or the bench would credit
        // any file whose text mentions the expected one.
        assert_eq!(hit_rank(&[serde_json::json!({"repo": "kb"})], "s1"), None);
        assert_eq!(
            hit_rank(
                &[serde_json::json!({"path": "src/order_extra.rs"})],
                "src/order.rs"
            ),
            None
        );
    }

    #[test]
    fn score_response_skips_unavailable_and_pending_lanes() {
        let body = serde_json::json!({
            "sections": [
                section("files", &["src/a.rs"]),
                { "lane": "symbols", "results": [], "truncated": false, "unavailable_reason": "q must not be empty" },
                { "lane": "semantic", "results": [], "truncated": false, "pending": true },
            ],
        });
        let outcome = score_response(&body, "src/a.rs", 12.5);
        assert_eq!(outcome.lanes.len(), 1);
        assert_eq!(outcome.lanes[0].lane, "files");
        assert_eq!(outcome.lanes[0].rank, Some(1));
    }

    #[test]
    fn score_response_records_a_miss_as_none_not_absence() {
        let body = serde_json::json!({ "sections": [section("files", &["src/z.rs"])] });
        let outcome = score_response(&body, "src/a.rs", 5.0);
        assert_eq!(outcome.lanes.len(), 1);
        assert_eq!(outcome.lanes[0].rank, None);
    }

    #[test]
    fn percentile_matches_hand_computed_values() {
        let xs = vec![10.0, 20.0, 30.0, 40.0, 50.0];
        assert_eq!(percentile(&xs, 0.50), 30.0);
        assert_eq!(percentile(&xs, 1.0), 50.0);
        assert_eq!(percentile(&[], 0.5), 0.0);
    }

    #[test]
    fn lane_agg_recall_and_percentiles() {
        let mut agg = LaneAgg::default();
        agg.record(Some(1), 10.0);
        agg.record(Some(3), 20.0);
        agg.record(None, 30.0);
        agg.record(Some(5), 40.0);
        assert_eq!(agg.total, 4);
        assert_eq!(agg.hit_at_1, 1);
        assert_eq!(agg.hit_at_5, 3);
        assert_eq!(agg.recall_at_1(), 0.25);
        assert_eq!(agg.recall_at_5(), 0.75);
        assert_eq!(agg.p50_ms(), 20.0);
    }

    #[test]
    fn bench_report_overall_is_hit_if_any_present_lane_hit() {
        let mut report = BenchReport::default();
        // files misses at top-5, symbols hits at rank 2 -> overall hit@5,
        // not hit@1 (neither lane hit rank 1).
        report.record(&QueryOutcome {
            latency_ms: 15.0,
            lanes: vec![
                LaneHit {
                    lane: "files".to_string(),
                    rank: None,
                },
                LaneHit {
                    lane: "symbols".to_string(),
                    rank: Some(2),
                },
            ],
        });
        assert_eq!(report.queries, 1);
        assert_eq!(report.overall.hit_at_1, 0);
        assert_eq!(report.overall.hit_at_5, 1);
        assert_eq!(report.lanes["files"].hit_at_5, 0);
        assert_eq!(report.lanes["symbols"].hit_at_5, 1);
    }

    #[test]
    fn bench_report_per_lane_denominator_is_queries_where_that_lane_was_present() {
        let mut report = BenchReport::default();
        report.record(&QueryOutcome {
            latency_ms: 1.0,
            lanes: vec![LaneHit {
                lane: "files".to_string(),
                rank: Some(1),
            }],
        });
        report.record(&QueryOutcome {
            latency_ms: 1.0,
            lanes: vec![LaneHit {
                lane: "symbols".to_string(),
                rank: Some(1),
            }],
        });
        // "files" only ever appeared in query 1 -> its own denominator is 1,
        // not 2 (the total query count).
        assert_eq!(report.lanes["files"].total, 1);
        assert_eq!(report.lanes["symbols"].total, 1);
        assert_eq!(report.queries, 2);
    }
}
