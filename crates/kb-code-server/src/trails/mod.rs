//! V74-L3b — `kbc-trail/1`: the navigation record, OFF by default.
//!
//! Design of record: `docs/research/kb-code-v7-continuum-2026-09.html`
//! §Decisions **D12** (tours and trails), **D17** (attention, not
//! comprehension), §P7 (the Trail and the Location Contract) and the
//! Security posture's §Privacy line — "attention and trail data
//! client-local in v7.0; server-side only with opt-in, indicator, purge
//! and retention".
//!
//! # The one paragraph
//!
//! A trail is an ordered list of PLACES the operator went, one dwell
//! number each, recorded only after an explicit opt-in, shown to the
//! operator in full, shown to an agent only in AGGREGATE, purgeable
//! wholesale, and aged out by a retention window. Nothing it records is
//! ever a ranking term, a filter default, a gate or a trust class.
//!
//! # Why pause, purge and retention had to ship in the SAME unit
//!
//! D17 does not say "server-side trails, with purge to follow": it says
//! server-side "only in the milestone that ships pause, purge and
//! retention together". A ledger of where a person looked, with no way to
//! stop it and no way to delete it, is a different product from the one
//! this design describes — so the three controls are not features layered
//! on a store, they are the precondition for the store existing at all.
//! [`crate::trails::routes`] therefore ships `state` (pause), `purge` and
//! [`gc`] (retention) in the same file set as the ingest route, and the
//! ingest route refuses unless [`TrailsSection::enabled`] AND the
//! persisted [`MODE_RECORDING`] both say yes.
//!
//! # Four structural properties (not four promises)
//!
//! 1. **Nothing finer than a step can be stored.** `trail_steps` is the
//!    finest row in the schema and it holds ONE dwell number.
//!    [`reject_sub_step_keys`] refuses a payload that names a viewport
//!    span, a caret, a scroll offset or a per-line dwell — BY NAME, on the
//!    raw JSON, before the typed parse, so the refusal teaches the rule
//!    rather than saying "unknown field" (`boards::lint`'s `coordinates`
//!    rule, same technique for the same reason). A sub-step span is
//!    REFUSED, never silently rounded into a step.
//! 2. **Dwell is derived and quantised.** The client sends
//!    `entered_at`/`left_at`; the server computes the difference and
//!    floors it to [`TrailsSection::step_granularity_secs`]
//!    ([`quantise_dwell`]). There is no client-supplied dwell field at
//!    all, so "never finer than dwell-per-step" is a property of the wire,
//!    not of the caller's manners.
//! 3. **The agent-facing read is aggregate, and its only time key is the
//!    day.** `GET /api/trails/aggregate` groups by `(path, symbol)` and
//!    counts; it reads `trail_steps.day` and never `entered_at`.
//!    [`AggregateRow`] has no timestamp field to leak one into.
//! 4. **No ranking module may import this one.**
//!    [`tests::no_ranking_module_imports_the_trail_ledger`] is a source
//!    scan that fails BY FILE — `claims.rs`'s own pin (invariant 23(a)),
//!    which is root invariant #10's surfaced-never-scored law. A scan has
//!    a scan's limits; it is the same trade `git_argv_lint` (invariant 3)
//!    makes.
//!
//! # What a purge does and does not delete
//!
//! `POST /api/trails/purge` deletes trails and their steps — the movement
//! record, which is the sensitive material. It deliberately does NOT
//! delete an `annotations` row carrying a `trail_id`: a dissent note on an
//! agent-authored trail is the human's OWN words, and `claims`' ruling
//! (invariant 23(a) — authored content is not derived data) applies
//! unchanged. A note whose trail is gone reads back saying so.

pub mod gc;
pub mod routes;

use serde::{Deserialize, Serialize};

/// The wire schema string.
pub const SCHEMA: &str = "kbc-trail/1";

// --- vocabularies ---------------------------------------------------------

pub const MODE_OFF: &str = "off";
pub const MODE_RECORDING: &str = "recording";
pub const MODE_PAUSED: &str = "paused";

/// The CLOSED runtime-state vocabulary, in indicator order. `off` is what
/// a volume with no `trails_state` row reads as: the default is the
/// ABSENCE of a decision, never a decision to record (D17's "off on first
/// boot").
pub const MODES: [&str; 3] = [MODE_OFF, MODE_RECORDING, MODE_PAUSED];

pub fn is_valid_mode(s: &str) -> bool {
    MODES.contains(&s)
}

pub const ORIGIN_RECORDED: &str = "recorded";
pub const ORIGIN_AUTHORED: &str = "authored";

/// The CLOSED origin vocabulary — D12's two trails. A RECORDED trail is
/// the operator's own movement; an AUTHORED one is a path an agent laid
/// down for the human to walk with `]`/`[`.
pub const ORIGINS: [&str; 2] = [ORIGIN_RECORDED, ORIGIN_AUTHORED];

pub fn is_valid_origin(s: &str) -> bool {
    ORIGINS.contains(&s)
}

/// The typed hop, design §P7 VERBATIM. Closed, so a typo is a refusal
/// rather than a hop kind nothing can filter on.
pub const VIA_KINDS: [&str; 11] = [
    "search",
    "definition_of",
    "usage_of",
    "caller_of",
    "blame",
    "why",
    "story",
    "review",
    "framework",
    "manual",
    "agent_suggested",
];

pub fn is_valid_via(s: &str) -> bool {
    VIA_KINDS.contains(&s)
}

/// Keys a step payload may NOT carry, checked on the RAW JSON before the
/// typed parse. Every one of them is a way of describing something FINER
/// than a step — the exact thing D17 rules out — and refusing them by name
/// is what makes the refusal a teaching surface instead of serde's
/// generic "unknown field" (the `boards::lint` `coordinates` precedent).
///
/// `deny_unknown_fields` on [`StepIn`] would already reject each of these.
/// This list exists so the CALLER is told which rule they hit.
pub const SUB_STEP_KEYS: [&str; 12] = [
    "spans",
    "viewport",
    "viewport_spans",
    "lines_seen",
    "line_dwell",
    "dwell_by_line",
    "dwell_ms",
    "dwell_secs",
    "caret",
    "scroll",
    "scroll_y",
    "selection",
];

// --- caps -----------------------------------------------------------------

/// Steps accepted in ONE `POST /api/trails/steps` batch.
pub const MAX_STEPS_PER_BATCH: usize = 128;
/// Steps one trail may hold. A batch that would cross it is REFUSED with
/// both numbers, never truncated — the same "a cap is a refusal, not a
/// silent clip" rule `boards::MAX_NODES` states.
pub const MAX_STEPS_PER_TRAIL: usize = 5_000;
/// Steps an AUTHORED trail may declare in one `POST /api/trails`.
pub const MAX_AUTHORED_STEPS: usize = 200;
/// Raw request-body cap, enforced BEFORE the JSON parse.
pub const MAX_INGEST_BYTES: usize = 256 * 1024;
/// Longest opaque `session_hint` / `title` / `note` accepted.
pub const MAX_LABEL_LEN: usize = 200;
/// Trails one list read returns.
pub const DEFAULT_LIST_LIMIT: usize = 50;
pub const MAX_LIST_LIMIT: usize = 500;
/// Rows one aggregate read returns.
pub const MAX_AGGREGATE_ROWS: usize = 500;
/// A single step's dwell is clamped to this many seconds. A tab left open
/// overnight is not eight hours of attention, and an unclamped number
/// would be the first thing anyone was tempted to rank on.
pub const MAX_DWELL_SECS: i64 = 30 * 60;

// --- the derived dwell ----------------------------------------------------

/// `left_at - entered_at`, floored to `granularity` and clamped to
/// [`MAX_DWELL_SECS`]. Pure; the ONLY place a dwell number is minted.
///
/// A NEGATIVE or absent interval is `0`, not an error: a step the operator
/// left before the clock agreed they arrived is a clock artefact, and
/// dropping the whole hop for it would lose the fact that they were there.
/// A sub-granularity interval floors to `0` — the step is still recorded
/// (they went there), with an honest zero dwell.
pub fn quantise_dwell(entered_at: i64, left_at: Option<i64>, granularity: i64) -> i64 {
    let g = granularity.max(1);
    let raw = left_at.unwrap_or(entered_at).saturating_sub(entered_at);
    if raw <= 0 {
        return 0;
    }
    (raw / g * g).min(MAX_DWELL_SECS)
}

/// The UTC calendar day (`YYYY-MM-DD`) a unix second falls on — the
/// aggregate's ONLY time key (D17: "no ordering below the day").
pub fn day_of(unix: i64) -> String {
    chrono::DateTime::from_timestamp(unix, 0)
        .unwrap_or_else(|| chrono::DateTime::from_timestamp(0, 0).expect("epoch is representable"))
        .format("%Y-%m-%d")
        .to_string()
}

/// A new trail id: `trl_` + 12 hex — the `set_`/`clm_` shape, minted with
/// the SAME `annotations::short_random_hex` those two use rather than a
/// third copy of a two-line generator.
pub fn new_trail_id() -> String {
    format!("trl_{}", crate::annotations::short_random_hex())
}

/// Refuse a raw ingest payload that names anything finer than a step.
/// Walks every object key at every depth — a sub-step key nested inside a
/// step object is the shape this actually has to catch.
pub fn reject_sub_step_keys(raw: &serde_json::Value) -> Result<(), String> {
    fn walk(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(map) => {
                for (k, child) in map {
                    if SUB_STEP_KEYS.contains(&k.as_str()) {
                        out.push(k.clone());
                    }
                    walk(child, out);
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(|i| walk(i, out)),
            _ => {}
        }
    }
    let mut found = Vec::new();
    walk(raw, &mut found);
    found.sort();
    found.dedup();
    if found.is_empty() {
        return Ok(());
    }
    Err(format!(
        "a trail step is the FINEST thing this daemon records, and {} names something finer \
         (kbc-trail/1, design D17: dwell is per STEP, never per line, viewport, caret or \
         scroll position). Send one step per place you went, with `entered_at` and \
         `left_at`; the daemon derives the dwell and quantises it. This payload is \
         REFUSED, not rounded.",
        found
            .iter()
            .map(|f| format!("{f:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

// --- the wire -------------------------------------------------------------

/// One recorded step, as the SPA sends it.
///
/// `deny_unknown_fields` for the reason `boards::BoardDoc` states: a typo'd
/// key in a payload that will be summarised later must fail loudly now.
/// Note what is NOT here: no dwell (derived), no viewport, no caret, no
/// scroll. There is nowhere to put them.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StepIn {
    /// One of [`VIA_KINDS`].
    pub via: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_start: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_end: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_sha: Option<String>,
    pub entered_at: i64,
    /// Absent = still open; the step records a zero dwell until a later
    /// batch supersedes it. An interval, never a dwell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// `POST /api/trails/steps` — one batch.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StepsIn {
    pub repo: String,
    /// The SPA's own opaque label, stored verbatim and never parsed.
    /// Deliberately not a `sessions.session_id`: this daemon makes no join
    /// between a trail and a transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_hint: Option<String>,
    pub steps: Vec<StepIn>,
}

/// `POST /api/trails` — an AUTHORED trail, laid down whole by an agent.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TrailIn {
    pub schema: String,
    pub repo: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Must be `true`. Present as a required, explicit field rather than
    /// inferred from the route, so a document on disk says what it is.
    pub authored: bool,
    pub steps: Vec<StepIn>,
}

/// One row of the ONLY agent-facing read. Note the absence: no
/// `entered_at`, no ordinal, no trail id, no timestamp of any kind finer
/// than a day.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AggregateRow {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    pub steps: i64,
    pub dwell_secs: i64,
    pub days: i64,
    pub first_day: String,
    pub last_day: String,
}

/// The trails state — the TopBar indicator's source of truth.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StateOut {
    pub schema: &'static str,
    /// The operator's master switch, `[trails] enabled`. `false` ⇒ every
    /// write refuses and no mode transition is possible.
    pub enabled: bool,
    /// One of [`MODES`]. `off` on a volume that has never opted in.
    pub mode: &'static str,
    /// Every mode, so the indicator never has to infer the vocabulary.
    pub modes_available: Vec<&'static str>,
    pub retention_days: u32,
    pub step_granularity_secs: i64,
    /// When the mode last changed; `null` while it has never been set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed_unix: Option<i64>,
    /// Whether a mode transition is possible from THIS caller (a loopback
    /// peer with the feature enabled). The SPA hides the control when this
    /// is false rather than offering a button that 403s.
    pub mutable: bool,
    /// What this daemon will and will not do, in words — always present,
    /// so the indicator can show the posture without a doc lookup.
    pub notes: Vec<String>,
}

/// Canonicalise a mode string to one of [`MODES`]' `'static` values, so
/// every wire struct can hold a `&'static str` and no read can invent a
/// mode. An unrecognised stored value reads as [`MODE_OFF`] — a store that
/// somehow holds a mode this binary does not know must fail CLOSED, not
/// record.
pub fn mode_static(raw: &str) -> &'static str {
    MODES
        .iter()
        .copied()
        .find(|m| *m == raw)
        .unwrap_or(MODE_OFF)
}

// --- route contracts (invariant 15) ---------------------------------------

use crate::entities::RouteContract;

pub const TRAILS_STATE_ROUTE: RouteContract = RouteContract {
    path: "/api/trails/state",
    handler: "trails::routes::get_state",
    required_params: &[],
    params_accept_without: routes::state_params_accept_without,
};

pub const TRAILS_LIST_ROUTE: RouteContract = RouteContract {
    path: "/api/trails",
    handler: "trails::routes::list_trails",
    required_params: &["repo"],
    params_accept_without: routes::list_params_accept_without,
};

pub const TRAIL_GET_ROUTE: RouteContract = RouteContract {
    path: "/api/trails/{id}",
    handler: "trails::routes::get_trail",
    required_params: &["repo"],
    params_accept_without: routes::get_params_accept_without,
};

pub const TRAILS_AGGREGATE_ROUTE: RouteContract = RouteContract {
    path: "/api/trails/aggregate",
    handler: "trails::routes::aggregate_trails",
    required_params: &["repo"],
    params_accept_without: routes::aggregate_params_accept_without,
};

/// This unit's TRAIL read surface. The five mutations (`state`, `steps`,
/// `purge`, `fork`, the authored `POST /api/trails`) are deliberately
/// absent for the reason `boards::V74_L1_ROUTES` records for its own four:
/// a `RouteContract` describes a query-param surface, and a POST whose
/// payload IS the contract has nothing for `params_accept_without` to say.
pub const V74_L3B_TRAIL_ROUTES: &[RouteContract] = &[
    TRAILS_STATE_ROUTE,
    TRAILS_LIST_ROUTE,
    TRAIL_GET_ROUTE,
    TRAILS_AGGREGATE_ROUTE,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_vocabularies_are_closed() {
        for m in MODES {
            assert!(is_valid_mode(m));
            assert_eq!(mode_static(m), m);
        }
        assert!(!is_valid_mode("Recording"));
        assert_eq!(
            mode_static("something-a-newer-binary-wrote"),
            MODE_OFF,
            "an unknown stored mode must fail CLOSED — a daemon that cannot read its own \
             opt-in must not record"
        );
        for o in ORIGINS {
            assert!(is_valid_origin(o));
        }
        assert!(!is_valid_origin("recorded "));
        for v in VIA_KINDS {
            assert!(is_valid_via(v));
        }
        assert!(!is_valid_via("definition"));
        assert!(!is_valid_via(""));
    }

    #[test]
    fn dwell_is_floored_to_the_granularity_and_never_negative_or_unbounded() {
        assert_eq!(quantise_dwell(100, Some(112), 1), 12);
        assert_eq!(
            quantise_dwell(100, Some(112), 5),
            10,
            "floored, not rounded"
        );
        assert_eq!(quantise_dwell(100, Some(103), 5), 0, "sub-granularity → 0");
        assert_eq!(quantise_dwell(100, None, 1), 0, "an open step dwells 0");
        assert_eq!(quantise_dwell(100, Some(50), 1), 0, "a clock artefact → 0");
        assert_eq!(
            quantise_dwell(0, Some(i64::MAX), 1),
            MAX_DWELL_SECS,
            "an unbounded dwell is the first thing anyone would rank on"
        );
        // A granularity of 0 or a negative one cannot divide by zero.
        assert_eq!(quantise_dwell(100, Some(112), 0), 12);
        assert_eq!(quantise_dwell(100, Some(112), -3), 12);
    }

    #[test]
    fn the_day_key_is_utc_and_stable() {
        assert_eq!(day_of(0), "1970-01-01");
        assert_eq!(day_of(1_757_000_000), day_of(1_757_000_000 + 60));
        assert_ne!(day_of(1_757_000_000), day_of(1_757_000_000 + 86_400));
    }

    #[test]
    fn a_sub_step_payload_is_refused_by_name_at_any_depth() {
        let ok = serde_json::json!({
            "repo": "r",
            "steps": [{"via": "manual", "path": "a.rb", "entered_at": 1}]
        });
        assert!(reject_sub_step_keys(&ok).is_ok());

        for key in SUB_STEP_KEYS {
            let nested = serde_json::json!({
                "repo": "r",
                "steps": [{"via": "manual", "entered_at": 1, key: 3}]
            });
            let err = reject_sub_step_keys(&nested)
                .expect_err("a sub-step key nested inside a step must be refused");
            assert!(err.contains(key), "the refusal must name {key:?}: {err}");
            assert!(
                err.contains("REFUSED, not rounded"),
                "the refusal must say it is a refusal: {err}"
            );
        }
        // …and deeper than one nesting level.
        let deep = serde_json::json!({"a": {"b": [{"c": {"viewport": [1, 2]}}]}});
        assert!(reject_sub_step_keys(&deep).is_err());
    }

    #[test]
    fn a_step_payload_has_nowhere_to_put_a_dwell_or_a_viewport() {
        let err =
            serde_json::from_str::<StepIn>(r#"{"via":"manual","entered_at":1,"dwell_secs":30}"#)
                .expect_err("dwell is DERIVED — a client-supplied one must not parse");
        assert!(err.to_string().contains("dwell_secs"), "{err}");
        let err =
            serde_json::from_str::<StepIn>(r#"{"via":"manual","entered_at":1,"viewport":[1,9]}"#)
                .expect_err("a viewport span must not parse");
        assert!(err.to_string().contains("viewport"), "{err}");
        // The legitimate shape still round-trips.
        let s: StepIn = serde_json::from_str(
            r#"{"via":"definition_of","path":"a.rb","line_start":3,"entered_at":10,"left_at":22}"#,
        )
        .expect("the sanctioned shape parses");
        assert_eq!(s.via, "definition_of");
        assert_eq!(quantise_dwell(s.entered_at, s.left_at, 1), 12);
    }

    /// D17's structural pin, and `claims.rs`'s (invariant 23(a)) applied to
    /// this ledger: attention is "never a gate, never a score term". A
    /// source scan has a scan's limits — it is the same trade
    /// `git_argv_lint` (invariant 3) makes — but it fails BY FILE, which is
    /// what a reviewer needs.
    #[test]
    fn no_ranking_module_imports_the_trail_ledger() {
        const RANKERS: &[(&str, &str)] = &[
            ("search/matcher.rs", include_str!("../search/matcher.rs")),
            ("search/unified.rs", include_str!("../search/unified.rs")),
            ("search/results.rs", include_str!("../search/results.rs")),
            ("review_inbox.rs", include_str!("../review_inbox.rs")),
            (
                "review_analytics.rs",
                include_str!("../review_analytics.rs"),
            ),
            ("unified_inbox.rs", include_str!("../unified_inbox.rs")),
            ("resolve.rs", include_str!("../resolve.rs")),
            ("usages2.rs", include_str!("../usages2.rs")),
        ];
        for (name, src) in RANKERS {
            assert!(
                !src.contains("trails::") && !src.contains("crate::trails"),
                "{name} names the trail ledger — kbc-trail/1 is attention, and D17 rules it \
                 is NEVER a gate and NEVER a score term: a trail may be shown beside a row, \
                 never folded into its rank"
            );
        }
    }

    /// The aggregate's shape is the privacy contract: an agent must not be
    /// able to reconstruct WHEN, only WHAT and HOW MUCH, and never below
    /// the day.
    #[test]
    fn an_aggregate_row_carries_no_timestamp_finer_than_a_day() {
        let row = AggregateRow {
            path: Some("app/models/order.rb".into()),
            symbol: None,
            steps: 4,
            dwell_secs: 120,
            days: 2,
            first_day: "2026-09-01".into(),
            last_day: "2026-09-02".into(),
        };
        let json = serde_json::to_value(&row).expect("serialize");
        let keys: Vec<&str> = json
            .as_object()
            .expect("object")
            .keys()
            .map(|s| s.as_str())
            .collect();
        for forbidden in [
            "entered_at",
            "left_at",
            "ordinal",
            "trail_id",
            "created_unix",
            "updated_unix",
            "at",
            "timestamp",
        ] {
            assert!(
                !keys.contains(&forbidden),
                "the aggregate must not carry {forbidden:?} — D17 caps the agent-facing \
                 surface at counts per file/symbol with no ordering below the day"
            );
        }
        for day in [&row.first_day, &row.last_day] {
            assert_eq!(day.len(), 10, "a day key is YYYY-MM-DD, never a timestamp");
        }
    }

    #[test]
    fn every_declared_route_path_is_api_nested_and_distinct() {
        assert!(!V74_L3B_TRAIL_ROUTES.is_empty());
        let mut seen = std::collections::BTreeSet::new();
        for c in V74_L3B_TRAIL_ROUTES {
            assert!(c.path.starts_with("/api/"), "{}", c.path);
            assert!(seen.insert(c.path), "duplicate contract for {}", c.path);
        }
    }
}
