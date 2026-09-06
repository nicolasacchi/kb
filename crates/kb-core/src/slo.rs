//! CT-F5 — corpus-health SLOs: the measurement instrument for the
//! connective-tissue program.
//!
//! An SRE-shaped read over tables kb ALREADY has. Four indicators, each with
//! ONE definition (written down once, here, and never restated differently
//! anywhere else), an optional per-kb target from `[kb.<name>.slo]`, and a
//! three-valued status.
//!
//! # Surfaced, never enforced
//!
//! Nothing in this module — or in anything downstream of it — changes
//! behaviour on a missed target. There is no alert, no gate, no retry, no
//! auto-repair, and no scoring consumer: an SLO miss is a number a human
//! reads. That is the whole point. A target you can miss is a claim you can
//! check; the alternative (a health number nothing is measured against) is a
//! vibe. The status vocabulary is deliberately `ok | warn | unknown` with NO
//! `fail`, because "fail" invites something to act on it.
//!
//! # Purity
//!
//! Everything here is a pure function of explicitly-passed inputs: no clock
//! (`now_unix` is an argument), no storage handle, no HTTP client, no
//! filesystem. The caller (`kb-server`'s `routes::slo`) does the reads and
//! hands the raw counts in. That keeps every indicator fixture-testable,
//! including all of its `unknown` paths.
//!
//! # What this module deliberately does NOT do
//!
//! It never calls kb-code. Invariant #2 is that kb extracts HINTS and kb-code
//! mints CLASSES; a "resolution %" computed by asking kb-code whether each
//! path exists would make kb a classifier by proxy and would make this
//! indicator depend on a sibling daemon's uptime. The coderef indicator here
//! is a purely STRUCTURAL, corpus-local measure over kb's own `code_refs`
//! rows — see [`SloKey::CoderefResolutionPct`]'s definition.

use serde::{Deserialize, Serialize};

/// Wire grammar tag, carried on [`SloReport`] so a consumer can tell which
/// indicator set + definitions it is looking at. Bumped only if an existing
/// indicator's DEFINITION changes (adding a new key is additive and does not
/// bump it — an unknown key renders as unknown, it never breaks a decode).
pub const SLO_GRAMMAR: &str = "kb-slo/1";

/// The `code_refs.kind` values that count toward
/// [`SloKey::CoderefResolutionPct`]'s numerator — the LOCAL-TREE PATH SHAPES.
///
/// Single-sourced here and consumed by `Db::code_ref_shape_counts`'s SQL, so
/// the definition in this module's docs and the predicate that implements it
/// can't drift. `coderef_path_shapes_are_exactly_the_path_kinds` pins the set
/// against `crate::coderefs::CodeRefKind`, so adding a kind to that enum
/// forces a decision here rather than silently skewing the ratio.
pub const CODEREF_PATH_SHAPED_KINDS: [&str; 4] = ["path", "path_line", "path_range", "path_list"];

/// The four operator-seeded indicators. A CLOSED set in code; stored as TEXT
/// on the wire and in `slo_snapshots.indicator` so a row written by a newer
/// binary still reads on an older one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SloKey {
    /// **Definition.** Of every row this corpus has in `code_refs`, the
    /// percentage whose `kind` carries a LOCAL-TREE PATH SHAPE — i.e. one of
    /// `path` / `path_line` / `path_range` / `path_list`.
    ///
    /// Deliberately EXCLUDED from the numerator (all four are still in the
    /// denominator, because they are real extractions):
    ///
    /// * `symbol_method` / `symbol_const` — a `Namespace::Class` or
    ///   `Class#method` hint names no file. kb has no symbol table, so
    ///   nothing kb-local could ever point it at a path.
    /// * `issue` — an issue URL resolves against a tracker, not a tree.
    /// * `external` — a gem/vendor path is, by the extractor's own closed
    ///   grammar, deliberately outside the local tree.
    ///
    /// **This is a structural measure, not a resolution result.** kb has no
    /// checkout and never asks kb-code (invariant #2): a `path`-shaped hint
    /// counted here may still point at a file that no longer exists. What the
    /// number answers is "what share of what this corpus cites is even the
    /// KIND of thing a code daemon could resolve" — a corpus drifting toward
    /// bare symbol names and vendored paths is drifting out of reach of the
    /// doc↔code bridge, and that is the drift this indicator exists to catch.
    ///
    /// Higher is better; a configured target is a MINIMUM. `unknown` when the
    /// corpus has no `code_refs` rows at all (never 0% — a corpus that cites
    /// no code has not failed at citing code).
    CoderefResolutionPct,

    /// **Definition.** The number of DOCUMENTS in this corpus carrying a
    /// `kb_session` lance value for which NO `sessions` row exists anywhere
    /// on this daemon.
    ///
    /// "Anywhere on this daemon" is load-bearing and matches how every other
    /// session join already works: `kb_session` is a HINT on the doc, the
    /// canonical row lives in a per-kb `sessions` table, and a memory written
    /// during a session routinely lives in a DIFFERENT corpus than the
    /// transcript that produced it (invariant #11's cross-kb fan-out). A
    /// per-kb-only check would therefore report every memory in a
    /// memories corpus as an orphan, which is noise, not a signal.
    ///
    /// An orphan is a dangling provenance pointer: a doc that claims a
    /// session as its origin, whose origin nothing can produce. Usually it
    /// means the transcript was never captured, was deleted, or that the
    /// corpus holding transcripts is not mounted on this daemon — the last of
    /// which is a legitimate deployment, so a nonzero count is an
    /// invitation to look, not a defect.
    ///
    /// Lower is better; a configured target is a MAXIMUM. A corpus where no
    /// doc carries a `kb_session` at all measures an honest `0` (a count over
    /// an empty set needs no inference), with the emptiness stated in
    /// `detail`.
    OrphanKbSessions,

    /// **Definition.** Over the censused captures in this corpus's `sessions`
    /// table, `failed / (marker_parsed + fallback_parsed + failed)` as a
    /// percentage — the share of recall injections that parsed via NEITHER
    /// the `kb-recall/1` machine marker nor the free-text fallback grammar,
    /// and therefore produced no `memory_recalls` row.
    ///
    /// This is the CT-A3 census (`kb_core::sessions::view::DerivedRecalls`),
    /// persisted per capture by the `memory-recall-ledger` hook into V0039's
    /// three nullable `sessions` columns. Captures predating V0039 carry NULL
    /// and are excluded from BOTH halves of the fraction — see the migration
    /// header for why NULL and 0 must stay distinguishable. Reads scope to
    /// the newest capture per session (invariant #11) or a long session
    /// re-captured fifty times would weight fifty-fold.
    ///
    /// A nonzero rate means the recall ledger is silently under-recording:
    /// injections happened, memories were shown to an agent, and no row
    /// exists to say so. That is the one failure in the provenance chain that
    /// is invisible from every other surface, which is why it is an SLO.
    ///
    /// Lower is better; a configured target is a MAXIMUM. `unknown` when no
    /// capture carries a census, or when the censused captures walked zero
    /// injected hits (an empty denominator is not 0%).
    LedgerParseFailurePct,

    /// **Definition.** Hours between now and the newest `sessions.started_at`
    /// in this corpus.
    ///
    /// The staleness of the capture pipeline itself. A sessions corpus whose
    /// newest capture is four days old usually means the Stop hook stopped
    /// firing — a failure mode that is otherwise completely silent, because
    /// nothing errors: the daemon is healthy, search works, and the corpus
    /// simply stops growing.
    ///
    /// `started_at` (not `ended_at`, not the artifact mtime) because it is
    /// the session's own clock, and it is what the newest-capture ordering
    /// everywhere else already keys on. A clock skew that puts the newest
    /// capture in the future clamps to `0.0` rather than reporting a negative
    /// age.
    ///
    /// Lower is better; a configured target is a MAXIMUM. `unknown` when the
    /// corpus has no `sessions` rows (a non-sessions corpus is not stale, it
    /// simply has no capture pipeline).
    CaptureFreshnessHours,
}

/// Which way a target reads. Not a policy — purely how [`SloStatus`] is
/// derived from `(value, target)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SloDirection {
    /// The target is a MINIMUM: `warn` when `value < target`.
    HigherIsBetter,
    /// The target is a MAXIMUM: `warn` when `value > target`.
    LowerIsBetter,
}

impl SloDirection {
    /// Wire spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            SloDirection::HigherIsBetter => "higher_is_better",
            SloDirection::LowerIsBetter => "lower_is_better",
        }
    }
}

impl SloKey {
    /// Every indicator, in the canonical order every surface renders them:
    /// the doc↔code lane, then the sessions lane's three.
    pub const ALL: [SloKey; 4] = [
        SloKey::CoderefResolutionPct,
        SloKey::OrphanKbSessions,
        SloKey::LedgerParseFailurePct,
        SloKey::CaptureFreshnessHours,
    ];

    /// Wire/DB/config spelling. ONE name per indicator: the `[kb.*.slo]`
    /// config key, the JSON `key`, the `slo_snapshots.indicator` value and
    /// the CLI's column header are all this exact string.
    pub fn as_str(&self) -> &'static str {
        match self {
            SloKey::CoderefResolutionPct => "coderef_resolution_pct",
            SloKey::OrphanKbSessions => "orphan_kb_sessions",
            SloKey::LedgerParseFailurePct => "ledger_parse_failure_pct",
            SloKey::CaptureFreshnessHours => "capture_freshness_hours",
        }
    }

    /// Inverse of [`Self::as_str`]; `None` outside the closed set.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "coderef_resolution_pct" => SloKey::CoderefResolutionPct,
            "orphan_kb_sessions" => SloKey::OrphanKbSessions,
            "ledger_parse_failure_pct" => SloKey::LedgerParseFailurePct,
            "capture_freshness_hours" => SloKey::CaptureFreshnessHours,
            _ => return None,
        })
    }

    /// Short human label for the SPA panel + `kb slo status`.
    pub fn label(&self) -> &'static str {
        match self {
            SloKey::CoderefResolutionPct => "code-ref path shape",
            SloKey::OrphanKbSessions => "orphan kb_session docs",
            SloKey::LedgerParseFailurePct => "recall ledger parse failures",
            SloKey::CaptureFreshnessHours => "capture freshness",
        }
    }

    /// Unit of `value`: `percent` | `count` | `hours`.
    pub fn unit(&self) -> &'static str {
        match self {
            SloKey::CoderefResolutionPct | SloKey::LedgerParseFailurePct => "percent",
            SloKey::OrphanKbSessions => "count",
            SloKey::CaptureFreshnessHours => "hours",
        }
    }

    /// How a configured target reads for this indicator.
    pub fn direction(&self) -> SloDirection {
        match self {
            SloKey::CoderefResolutionPct => SloDirection::HigherIsBetter,
            SloKey::OrphanKbSessions
            | SloKey::LedgerParseFailurePct
            | SloKey::CaptureFreshnessHours => SloDirection::LowerIsBetter,
        }
    }
}

/// Three-valued, deliberately without `fail`.
///
/// * `ok` — measured, a target is configured, and the target is met.
/// * `warn` — measured, a target is configured, and the target is missed.
/// * `unknown` — EITHER the value could not be computed (the inputs genuinely
///   aren't there) OR no target is configured (there is nothing to judge it
///   against). The two cases are told apart by `value`: `None` is the first,
///   `Some` with a `None` target is the second. Both are honestly "no verdict
///   available", and collapsing them into a fake `ok` would let an unmeasured
///   corpus read as a healthy one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SloStatus {
    Ok,
    Warn,
    Unknown,
}

impl SloStatus {
    /// Wire/DB spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            SloStatus::Ok => "ok",
            SloStatus::Warn => "warn",
            SloStatus::Unknown => "unknown",
        }
    }

    /// Inverse of [`Self::as_str`]; `None` outside the closed set.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ok" => SloStatus::Ok,
            "warn" => SloStatus::Warn,
            "unknown" => SloStatus::Unknown,
            _ => return None,
        })
    }
}

/// One indicator, computed. The wire shape of `GET /api/kb/{kb}/slo`'s
/// `indicators[]` and of one `slo_snapshots` row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SloIndicator {
    /// [`SloKey::as_str`].
    pub key: String,
    pub label: String,
    /// `percent` | `count` | `hours`.
    pub unit: String,
    /// `higher_is_better` | `lower_is_better`.
    pub direction: String,
    /// The measurement, or `None` when the inputs genuinely aren't there.
    /// Percentages + hours are rounded to two decimals so the wire value, the
    /// stored snapshot and the rendered string are the same number.
    pub value: Option<f64>,
    /// The configured target, or `None` when `[kb.*.slo]` set none.
    pub target: Option<f64>,
    pub status: SloStatus,
    /// One honest sentence: the numerator/denominator behind the value, or
    /// exactly WHY it is unknown. Never a recommendation — this surface
    /// reports, it does not instruct.
    pub detail: String,
}

/// The whole per-kb reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SloReport {
    /// [`SLO_GRAMMAR`].
    pub grammar: String,
    pub kb: String,
    /// Wall clock (unix seconds) the caller measured at — passed in, never
    /// read from a clock here.
    pub computed_at_unix: i64,
    /// Always all four of [`SloKey::ALL`], in that order. An indicator is
    /// never omitted: "we could not measure this" is itself a reading, and
    /// dropping the row would make a broken input look like a missing
    /// feature.
    pub indicators: Vec<SloIndicator>,
    /// How many indicators are `warn` — a convenience for the SPA badge and
    /// the CLI's exit summary. It is a COUNT, not a verdict, and nothing
    /// branches on it.
    pub warn_count: usize,
}

/// The four optional targets, resolved from `[kb.<name>.slo]`. `None`
/// everywhere (a kb with no `[slo]` section) still produces a full report —
/// every indicator measured, every status `unknown` for want of a target.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SloTargets {
    /// Minimum acceptable [`SloKey::CoderefResolutionPct`].
    pub coderef_resolution_pct: Option<f64>,
    /// Maximum acceptable [`SloKey::OrphanKbSessions`].
    pub orphan_kb_sessions: Option<f64>,
    /// Maximum acceptable [`SloKey::LedgerParseFailurePct`].
    pub ledger_parse_failure_pct: Option<f64>,
    /// Maximum acceptable [`SloKey::CaptureFreshnessHours`].
    pub capture_freshness_hours: Option<f64>,
}

impl SloTargets {
    /// The target for one key, if configured.
    pub fn get(&self, key: SloKey) -> Option<f64> {
        match key {
            SloKey::CoderefResolutionPct => self.coderef_resolution_pct,
            SloKey::OrphanKbSessions => self.orphan_kb_sessions,
            SloKey::LedgerParseFailurePct => self.ledger_parse_failure_pct,
            SloKey::CaptureFreshnessHours => self.capture_freshness_hours,
        }
    }
}

/// Every raw count the four indicators need, read from EXISTING tables by the
/// caller. Nothing here is derived — this struct is the seam that keeps the
/// computation pure and every `unknown` path fixture-testable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SloInputs {
    /// Total rows in this kb's `code_refs`.
    pub coderef_total: u64,
    /// Of those, rows whose `kind` is one of the four path shapes.
    pub coderef_path_shaped: u64,
    /// Docs in this kb carrying a non-empty `kb_session`.
    pub docs_with_kb_session: u64,
    /// Of those, docs whose session id has no `sessions` row anywhere on this
    /// daemon.
    pub orphan_kb_session_docs: u64,
    /// CT-A3 census sums over the newest capture per session, counting only
    /// captures whose census is non-NULL.
    pub recall_marker_parsed: u64,
    pub recall_fallback_parsed: u64,
    pub recall_failed: u64,
    /// How many captures contributed to the three counters above. Zero means
    /// "no capture carries a census" — distinct from "captures carry a census
    /// and saw no injections", which is a zero denominator with a nonzero
    /// count here.
    pub recall_censused_captures: u64,
    /// Newest `sessions.started_at` in this kb, or `None` when the table is
    /// empty.
    pub newest_session_started_at: Option<i64>,
}

/// Round to two decimals, half-away-from-zero. Applied to every non-integer
/// value so the JSON body, the stored snapshot and the printed string are
/// literally the same number (a trend log comparing `81.33333333333333` to
/// `81.33` would show phantom movement).
fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Derive a status from a measurement and an optional target. See
/// [`SloStatus`] for why an absent target is `unknown` rather than `ok`.
fn status_for(value: Option<f64>, target: Option<f64>, direction: SloDirection) -> SloStatus {
    let (Some(v), Some(t)) = (value, target) else {
        return SloStatus::Unknown;
    };
    let met = match direction {
        SloDirection::HigherIsBetter => v >= t,
        SloDirection::LowerIsBetter => v <= t,
    };
    if met {
        SloStatus::Ok
    } else {
        SloStatus::Warn
    }
}

fn indicator(
    key: SloKey,
    value: Option<f64>,
    targets: &SloTargets,
    detail: String,
) -> SloIndicator {
    let target = targets.get(key);
    SloIndicator {
        key: key.as_str().to_string(),
        label: key.label().to_string(),
        unit: key.unit().to_string(),
        direction: key.direction().as_str().to_string(),
        value,
        target,
        status: status_for(value, target, key.direction()),
        detail,
    }
}

/// Compute the full report. Pure: `now_unix` is the caller's clock, `inputs`
/// are the caller's reads.
pub fn build(kb: &str, inputs: &SloInputs, targets: &SloTargets, now_unix: i64) -> SloReport {
    let indicators = vec![
        coderef_indicator(inputs, targets),
        orphan_indicator(inputs, targets),
        ledger_indicator(inputs, targets),
        freshness_indicator(inputs, targets, now_unix),
    ];
    let warn_count = indicators
        .iter()
        .filter(|i| i.status == SloStatus::Warn)
        .count();
    SloReport {
        grammar: SLO_GRAMMAR.to_string(),
        kb: kb.to_string(),
        computed_at_unix: now_unix,
        indicators,
        warn_count,
    }
}

fn coderef_indicator(inputs: &SloInputs, targets: &SloTargets) -> SloIndicator {
    let key = SloKey::CoderefResolutionPct;
    if inputs.coderef_total == 0 {
        return indicator(
            key,
            None,
            targets,
            "no code-ref hints extracted in this corpus — nothing to measure".to_string(),
        );
    }
    let pct = round2((inputs.coderef_path_shaped as f64 / inputs.coderef_total as f64) * 100.0);
    indicator(
        key,
        Some(pct),
        targets,
        format!(
            "{} of {} extracted hints carry a local-tree path shape \
             (symbol / issue / external hints are counted in the total, never the numerator; \
             kb has no checkout and never asks kb-code whether the path exists)",
            inputs.coderef_path_shaped, inputs.coderef_total,
        ),
    )
}

fn orphan_indicator(inputs: &SloInputs, targets: &SloTargets) -> SloIndicator {
    let key = SloKey::OrphanKbSessions;
    let detail = if inputs.docs_with_kb_session == 0 {
        "no doc in this corpus carries a kb_session — a count over an empty set".to_string()
    } else {
        format!(
            "{} of {} docs carrying a kb_session have no sessions row on this daemon \
             (the join is daemon-wide: a memory's transcript routinely lives in another corpus)",
            inputs.orphan_kb_session_docs, inputs.docs_with_kb_session,
        )
    };
    indicator(
        key,
        Some(inputs.orphan_kb_session_docs as f64),
        targets,
        detail,
    )
}

fn ledger_indicator(inputs: &SloInputs, targets: &SloTargets) -> SloIndicator {
    let key = SloKey::LedgerParseFailurePct;
    if inputs.recall_censused_captures == 0 {
        return indicator(
            key,
            None,
            targets,
            "no capture in this corpus carries a recall parse census — captures written before \
             V0039 are not backfilled, so this reads unknown until the next capture lands"
                .to_string(),
        );
    }
    let walked = inputs.recall_marker_parsed + inputs.recall_fallback_parsed + inputs.recall_failed;
    if walked == 0 {
        return indicator(
            key,
            None,
            targets,
            format!(
                "{} censused capture(s), but none walked a single injected recall hit — \
                 an empty denominator is not 0%",
                inputs.recall_censused_captures,
            ),
        );
    }
    let pct = round2((inputs.recall_failed as f64 / walked as f64) * 100.0);
    indicator(
        key,
        Some(pct),
        targets,
        format!(
            "{} of {} injected recall hits parsed via neither grammar across {} censused \
             capture(s) ({} via the kb-recall/1 marker, {} via the free-text fallback)",
            inputs.recall_failed,
            walked,
            inputs.recall_censused_captures,
            inputs.recall_marker_parsed,
            inputs.recall_fallback_parsed,
        ),
    )
}

fn freshness_indicator(inputs: &SloInputs, targets: &SloTargets, now_unix: i64) -> SloIndicator {
    let key = SloKey::CaptureFreshnessHours;
    let Some(newest) = inputs.newest_session_started_at else {
        return indicator(
            key,
            None,
            targets,
            "this corpus holds no captured sessions — there is no capture pipeline to be stale"
                .to_string(),
        );
    };
    // Clock skew (a capture stamped in the future) clamps to 0 rather than
    // reporting a negative age — "fresher than now" is not a reading.
    let age_secs = (now_unix - newest).max(0);
    let hours = round2(age_secs as f64 / 3600.0);
    indicator(
        key,
        Some(hours),
        targets,
        format!("newest capture started_at is {newest} ({hours}h before the read)"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_targets() -> SloTargets {
        SloTargets::default()
    }

    fn find(r: &SloReport, key: SloKey) -> &SloIndicator {
        r.indicators
            .iter()
            .find(|i| i.key == key.as_str())
            .unwrap_or_else(|| panic!("indicator {} missing from report", key.as_str()))
    }

    // --- grammar + shape ---------------------------------------------------

    #[test]
    fn grammar_and_key_set_are_pinned() {
        assert_eq!(SLO_GRAMMAR, "kb-slo/1");
        let keys: Vec<&str> = SloKey::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "coderef_resolution_pct",
                "orphan_kb_sessions",
                "ledger_parse_failure_pct",
                "capture_freshness_hours",
            ],
            "the indicator key set is the config key set, the wire key set AND the \
             slo_snapshots.indicator vocabulary — changing it is a four-surface break"
        );
        for k in SloKey::ALL {
            assert_eq!(SloKey::parse(k.as_str()), Some(k));
        }
        assert_eq!(SloKey::parse("nope"), None);
        for s in [SloStatus::Ok, SloStatus::Warn, SloStatus::Unknown] {
            assert_eq!(SloStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(SloStatus::parse("fail"), None, "there is no `fail` status");
    }

    /// The numerator's kind set is the doc↔code bridge's own vocabulary. A
    /// new `CodeRefKind` variant must be classified deliberately — landing in
    /// the denominator by default is fine, silently landing in neither (or
    /// mis-spelled into the SQL) is not.
    #[test]
    fn coderef_path_shapes_are_exactly_the_path_kinds() {
        use crate::coderefs::CodeRefKind;
        // Every listed spelling is a real kind.
        for k in CODEREF_PATH_SHAPED_KINDS {
            assert!(
                CodeRefKind::parse(k).is_some(),
                "{k} is not a CodeRefKind spelling — the SQL predicate would match nothing"
            );
        }
        // …and the kinds NOT listed are exactly the four non-path ones.
        let non_path: Vec<&str> = [
            CodeRefKind::SymbolMethod,
            CodeRefKind::SymbolConst,
            CodeRefKind::Issue,
            CodeRefKind::External,
        ]
        .iter()
        .map(|k| k.as_str())
        .collect();
        for k in &non_path {
            assert!(
                !CODEREF_PATH_SHAPED_KINDS.contains(k),
                "{k} names no local-tree path and must stay out of the numerator"
            );
        }
        assert_eq!(
            CODEREF_PATH_SHAPED_KINDS.len() + non_path.len(),
            8,
            "CodeRefKind gained a variant — classify it into the numerator or the \
             denominator explicitly (see SloKey::CoderefResolutionPct)"
        );
    }

    #[test]
    fn a_report_always_carries_all_four_indicators_even_with_no_inputs() {
        let r = build("k", &SloInputs::default(), &no_targets(), 1_000);
        assert_eq!(r.indicators.len(), 4);
        assert_eq!(r.grammar, SLO_GRAMMAR);
        assert_eq!(r.kb, "k");
        assert_eq!(r.computed_at_unix, 1_000);
        // Nothing measurable, nothing configured — but every row present.
        assert_eq!(r.warn_count, 0);
    }

    // --- coderef resolution ------------------------------------------------

    #[test]
    fn coderef_pct_counts_path_shapes_over_every_extracted_hint() {
        let inputs = SloInputs {
            coderef_total: 8,
            coderef_path_shaped: 6,
            ..Default::default()
        };
        let r = build("k", &inputs, &no_targets(), 0);
        let i = find(&r, SloKey::CoderefResolutionPct);
        assert_eq!(i.value, Some(75.0));
        assert_eq!(i.unit, "percent");
        assert_eq!(i.direction, "higher_is_better");
        assert!(i.detail.contains("6 of 8"), "{}", i.detail);
    }

    #[test]
    fn coderef_pct_is_unknown_not_zero_when_the_corpus_cites_no_code() {
        let r = build("k", &SloInputs::default(), &no_targets(), 0);
        let i = find(&r, SloKey::CoderefResolutionPct);
        assert_eq!(i.value, None, "0% would claim the corpus failed at citing");
        assert_eq!(i.status, SloStatus::Unknown);
    }

    #[test]
    fn coderef_target_is_a_minimum() {
        let targets = SloTargets {
            coderef_resolution_pct: Some(80.0),
            ..Default::default()
        };
        let below = build(
            "k",
            &SloInputs {
                coderef_total: 10,
                coderef_path_shaped: 7,
                ..Default::default()
            },
            &targets,
            0,
        );
        assert_eq!(
            find(&below, SloKey::CoderefResolutionPct).status,
            SloStatus::Warn
        );
        let at = build(
            "k",
            &SloInputs {
                coderef_total: 10,
                coderef_path_shaped: 8,
                ..Default::default()
            },
            &targets,
            0,
        );
        assert_eq!(
            find(&at, SloKey::CoderefResolutionPct).status,
            SloStatus::Ok,
            "exactly meeting a minimum is ok, not warn"
        );
    }

    // --- orphan kb_session -------------------------------------------------

    #[test]
    fn orphan_count_is_zero_and_says_so_when_nothing_carries_a_session() {
        let r = build("k", &SloInputs::default(), &no_targets(), 0);
        let i = find(&r, SloKey::OrphanKbSessions);
        assert_eq!(
            i.value,
            Some(0.0),
            "a count over an empty set is 0, not unknown"
        );
        assert!(i.detail.contains("empty set"), "{}", i.detail);
    }

    #[test]
    fn orphan_count_warns_above_its_maximum() {
        let inputs = SloInputs {
            docs_with_kb_session: 40,
            orphan_kb_session_docs: 3,
            ..Default::default()
        };
        let targets = SloTargets {
            orphan_kb_sessions: Some(0.0),
            ..Default::default()
        };
        let r = build("k", &inputs, &targets, 0);
        let i = find(&r, SloKey::OrphanKbSessions);
        assert_eq!(i.value, Some(3.0));
        assert_eq!(i.status, SloStatus::Warn);
        assert_eq!(r.warn_count, 1);
        assert!(i.detail.contains("3 of 40"), "{}", i.detail);
        assert!(
            i.detail.contains("daemon-wide"),
            "the cross-kb join must be stated, or a reader mistakes it for a per-kb check: {}",
            i.detail
        );
    }

    // --- ledger parse failures ---------------------------------------------

    #[test]
    fn ledger_rate_is_failed_over_every_walked_hit() {
        let inputs = SloInputs {
            recall_marker_parsed: 90,
            recall_fallback_parsed: 8,
            recall_failed: 2,
            recall_censused_captures: 12,
            ..Default::default()
        };
        let r = build("k", &inputs, &no_targets(), 0);
        let i = find(&r, SloKey::LedgerParseFailurePct);
        assert_eq!(i.value, Some(2.0));
        assert!(i.detail.contains("2 of 100"), "{}", i.detail);
        assert!(i.detail.contains("12 censused"), "{}", i.detail);
    }

    #[test]
    fn ledger_rate_is_unknown_when_no_capture_carries_a_census() {
        // The pre-V0039 corpus: captures exist, none was censused.
        let inputs = SloInputs {
            newest_session_started_at: Some(500),
            ..Default::default()
        };
        let r = build("k", &inputs, &no_targets(), 1_000);
        let i = find(&r, SloKey::LedgerParseFailurePct);
        assert_eq!(i.value, None);
        assert_eq!(i.status, SloStatus::Unknown);
        assert!(i.detail.contains("not backfilled"), "{}", i.detail);
    }

    #[test]
    fn ledger_rate_is_unknown_not_zero_on_an_empty_denominator() {
        // Censused captures exist, but none of them walked an injected hit —
        // a 0% failure rate here would claim a health nothing measured.
        let inputs = SloInputs {
            recall_censused_captures: 5,
            ..Default::default()
        };
        let r = build("k", &inputs, &no_targets(), 0);
        let i = find(&r, SloKey::LedgerParseFailurePct);
        assert_eq!(i.value, None);
        assert!(i.detail.contains("empty denominator"), "{}", i.detail);
    }

    // --- capture freshness -------------------------------------------------

    #[test]
    fn freshness_is_hours_since_the_newest_started_at() {
        let inputs = SloInputs {
            newest_session_started_at: Some(1_000_000),
            recall_censused_captures: 0,
            ..Default::default()
        };
        // 5400s = 1.5h
        let r = build("k", &inputs, &no_targets(), 1_005_400);
        let i = find(&r, SloKey::CaptureFreshnessHours);
        assert_eq!(i.value, Some(1.5));
        assert_eq!(i.unit, "hours");
        assert_eq!(i.direction, "lower_is_better");
    }

    #[test]
    fn freshness_clamps_clock_skew_to_zero_rather_than_going_negative() {
        let inputs = SloInputs {
            newest_session_started_at: Some(2_000),
            ..Default::default()
        };
        let r = build("k", &inputs, &no_targets(), 1_000);
        assert_eq!(find(&r, SloKey::CaptureFreshnessHours).value, Some(0.0));
    }

    #[test]
    fn freshness_is_unknown_on_a_corpus_with_no_captures() {
        let r = build("k", &SloInputs::default(), &no_targets(), 9_999);
        let i = find(&r, SloKey::CaptureFreshnessHours);
        assert_eq!(i.value, None);
        assert!(i.detail.contains("no captured sessions"), "{}", i.detail);
    }

    // --- status semantics --------------------------------------------------

    #[test]
    fn a_measured_value_with_no_target_is_unknown_never_a_free_ok() {
        let inputs = SloInputs {
            coderef_total: 4,
            coderef_path_shaped: 4,
            ..Default::default()
        };
        let r = build("k", &inputs, &no_targets(), 0);
        let i = find(&r, SloKey::CoderefResolutionPct);
        assert_eq!(i.value, Some(100.0));
        assert_eq!(
            i.status,
            SloStatus::Unknown,
            "an untargeted indicator has no verdict; `ok` would be an unearned pass"
        );
    }

    #[test]
    fn an_unmeasurable_indicator_with_a_target_is_still_unknown() {
        let targets = SloTargets {
            capture_freshness_hours: Some(24.0),
            ..Default::default()
        };
        let r = build("k", &SloInputs::default(), &targets, 0);
        assert_eq!(
            find(&r, SloKey::CaptureFreshnessHours).status,
            SloStatus::Unknown
        );
        assert_eq!(r.warn_count, 0, "an unknown is never a warn");
    }

    #[test]
    fn warn_count_sums_only_warns() {
        let inputs = SloInputs {
            coderef_total: 10,
            coderef_path_shaped: 1,
            docs_with_kb_session: 5,
            orphan_kb_session_docs: 5,
            recall_censused_captures: 1,
            recall_marker_parsed: 1,
            newest_session_started_at: Some(0),
            ..Default::default()
        };
        let targets = SloTargets {
            coderef_resolution_pct: Some(90.0),
            orphan_kb_sessions: Some(0.0),
            ledger_parse_failure_pct: Some(1.0),
            capture_freshness_hours: Some(1.0),
        };
        let r = build("k", &inputs, &targets, 3_600 * 3);
        // coderef 10% < 90 → warn; orphans 5 > 0 → warn; ledger 0% ≤ 1 → ok;
        // freshness 3h > 1h → warn.
        assert_eq!(
            find(&r, SloKey::LedgerParseFailurePct).status,
            SloStatus::Ok
        );
        assert_eq!(r.warn_count, 3);
    }

    #[test]
    fn values_are_rounded_to_two_decimals_so_the_log_does_not_drift() {
        let inputs = SloInputs {
            coderef_total: 3,
            coderef_path_shaped: 1,
            ..Default::default()
        };
        let r = build("k", &inputs, &no_targets(), 0);
        assert_eq!(find(&r, SloKey::CoderefResolutionPct).value, Some(33.33));
    }
}
