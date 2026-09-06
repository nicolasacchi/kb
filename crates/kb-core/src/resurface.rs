//! Deterministic resurfacing queue — the pure scoring core (thin slice:
//! open comments + unfinished reads). Design:
//! `docs/research/kb-resurface-queue-2026-07.html`.
//!
//! The queue is a VIEW over existing state (review files + the reading
//! rollup), never state of its own: no queue rows, no dismiss/snooze
//! persistence, no counters. Acting on the underlying object (resolving the
//! comment, finishing the read) clears an item naturally; unfinished reads
//! also FADE — the decay term is the anti-guilt mechanism, not an accident.
//!
//! Deterministic given `(signals, now_unix)` — the caller injects the clock,
//! mirroring `memory::rerank` and `sessions::recollect_score`. Reasons are
//! always surfaced alongside the score (the recollect house style: signals
//! inline, never buried); `--explain` renders the arithmetic from the two
//! weighted terms carried on every item.

/// Weight of the open-comments term. Explicit, human-created debt outranks
/// the implicit unfinished-read signal at equal magnitude.
pub const COMMENT_WEIGHT: f32 = 0.6;
/// Weight of the unfinished-read term.
pub const READ_WEIGHT: f32 = 0.4;
/// Open-comment count where the comment term saturates at 1.0 — the fourth
/// unresolved comment maxes the signal; more adds nothing (piecewise-linear,
/// trivially explainable).
pub const COMMENT_SATURATION: u32 = 4;
/// Half-life (days) of the unfinished-read term. A read untouched this long
/// contributes half; ~90 idle days puts a half-read artifact under
/// [`SCORE_FLOOR`]. The queue forgets so the reader doesn't have to.
pub const READ_HALFLIFE_DAYS: f32 = 45.0;
/// Items scoring below this are dropped entirely — the fade-out floor.
/// Comment-bearing items can't fall below it (1 open comment alone scores
/// `0.6/4 = 0.15`); only decayed reads fade out.
pub const SCORE_FLOOR: f32 = 0.05;

/// W2.9 — the five constants above, promoted to a per-kb-configurable
/// bundle (`[kb.<name>.resurface]` in kb.toml, see
/// [`crate::config::ResurfaceSection`]). `Default` reproduces the shipped
/// constants byte-for-byte, so every golden test below keeps passing
/// unmodified when it threads `ResurfaceWeights::default()` through. Every
/// pure fn in this module takes weights explicitly — no fn bakes in a
/// constant anymore — so the daemon can resolve a per-kb override once at
/// boot ([`ResurfaceWeights::from_section`]) and the SAME struct rides the
/// wire (`routes/resurface.rs`'s `ResurfaceResponse.weights`), so the CLI
/// `--explain` and SPA score-chip renderers show the REAL arithmetic
/// instead of a hardcoded mirror that could silently drift from a tuned kb.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResurfaceWeights {
    pub comment_weight: f32,
    pub read_weight: f32,
    pub comment_saturation: u32,
    pub read_halflife_days: f32,
    pub score_floor: f32,
}

impl Default for ResurfaceWeights {
    fn default() -> Self {
        Self {
            comment_weight: COMMENT_WEIGHT,
            read_weight: READ_WEIGHT,
            comment_saturation: COMMENT_SATURATION,
            read_halflife_days: READ_HALFLIFE_DAYS,
            score_floor: SCORE_FLOOR,
        }
    }
}

impl ResurfaceWeights {
    /// Resolve from a kb.toml `[kb.<name>.resurface]` section. Each of the
    /// five fields is resolved INDEPENDENTLY (mirroring the
    /// `AtlasOverrides::from_section` per-field precedent): a present-but
    /// -nonsensical value warns and falls back to the shipped default for
    /// just that field, never rejecting the whole section or failing boot.
    /// "Nonsensical" = non-finite or non-positive for the four weight-ish
    /// fields; `comment_saturation == 0` for the saturation count (it's a
    /// divisor in [`comment_component`]).
    pub fn from_section(section: Option<&crate::config::ResurfaceSection>) -> Self {
        let d = Self::default();
        let Some(s) = section else {
            return d;
        };

        let comment_weight = match s.comment_weight {
            Some(w) if w.is_finite() && w > 0.0 => w,
            Some(w) => {
                tracing::warn!(
                    value = w,
                    "[kb.*.resurface] comment_weight must be a positive finite number; \
                     falling back to the default 0.6"
                );
                d.comment_weight
            }
            None => d.comment_weight,
        };
        let read_weight = match s.read_weight {
            Some(w) if w.is_finite() && w > 0.0 => w,
            Some(w) => {
                tracing::warn!(
                    value = w,
                    "[kb.*.resurface] read_weight must be a positive finite number; falling \
                     back to the default 0.4"
                );
                d.read_weight
            }
            None => d.read_weight,
        };
        let comment_saturation = match s.comment_saturation {
            Some(0) => {
                tracing::warn!(
                    "[kb.*.resurface] comment_saturation must be greater than 0 (it divides \
                     the comment component); falling back to the default 4"
                );
                d.comment_saturation
            }
            Some(n) => n,
            None => d.comment_saturation,
        };
        let read_halflife_days = match s.read_halflife_days {
            Some(h) if h.is_finite() && h > 0.0 => h,
            Some(h) => {
                tracing::warn!(
                    value = h,
                    "[kb.*.resurface] read_halflife_days must be a positive finite number; \
                     falling back to the default 45.0"
                );
                d.read_halflife_days
            }
            None => d.read_halflife_days,
        };
        let score_floor = match s.score_floor {
            Some(f) if f.is_finite() && f >= 0.0 => f,
            Some(f) => {
                tracing::warn!(
                    value = f,
                    "[kb.*.resurface] score_floor must be a non-negative finite number; \
                     falling back to the default 0.05"
                );
                d.score_floor
            }
            None => d.score_floor,
        };

        Self {
            comment_weight,
            read_weight,
            comment_saturation,
            read_halflife_days,
            score_floor,
        }
    }
}

/// Raw per-artifact inputs, gathered by the caller (route) from the review
/// dir walk and the reading rollup. Either signal may be absent.
#[derive(Debug, Clone)]
pub struct ResurfaceSignals {
    pub artifact_id: String,
    /// Count of `status == open` comments on the artifact.
    pub open_comments: u32,
    /// `created_at` (unix secs) of the oldest open comment — surfaced in the
    /// reason string, deliberately NOT scored (the recollect precedent:
    /// error_count/staleness are surfaced, never score terms).
    pub oldest_open_unix: Option<i64>,
    /// Scroll completion of the newest visit, `0..=100`, when the artifact is
    /// in progress (opened, unfinished, no override).
    pub completion_pct: Option<u8>,
    /// `started_at` of the newest open visit (unix secs).
    pub last_opened_unix: Option<i64>,
}

/// One scored queue entry. `comment_term + read_term == score` — the terms
/// are the weighted contributions, so an explain renderer needs no
/// re-derivation.
#[derive(Debug, Clone)]
pub struct ScoredResurface {
    pub signals: ResurfaceSignals,
    pub comment_term: f32,
    pub read_term: f32,
    pub score: f32,
}

/// Unweighted comment component in `[0, 1]`: `min(open, SATURATION)/SATURATION`.
/// No decay — an unresolved comment is explicit, resolvable debt; letting it
/// fade would silently drop a promise.
pub fn comment_component(open_comments: u32, weights: &ResurfaceWeights) -> f32 {
    open_comments.min(weights.comment_saturation) as f32 / weights.comment_saturation as f32
}

/// Unweighted read component in `[0, 1]`: progress (endowed effort — the
/// 70%-read doc outranks the 10% skim) times a half-life decay on idleness.
/// A future `last_opened` (clock skew) clamps to zero idle.
pub fn read_component(
    completion_pct: u8,
    last_opened_unix: i64,
    now_unix: i64,
    weights: &ResurfaceWeights,
) -> f32 {
    let progress = f32::from(completion_pct.min(100)) / 100.0;
    let idle_days = (now_unix - last_opened_unix).max(0) as f32 / 86_400.0;
    progress * 0.5_f32.powf(idle_days / weights.read_halflife_days)
}

/// Score one artifact's signals at `now_unix`. Returns the two weighted
/// terms; total = their sum.
pub fn score(signals: &ResurfaceSignals, now_unix: i64, weights: &ResurfaceWeights) -> (f32, f32) {
    let ct = if signals.open_comments > 0 {
        weights.comment_weight * comment_component(signals.open_comments, weights)
    } else {
        0.0
    };
    let rt = match (signals.completion_pct, signals.last_opened_unix) {
        (Some(pct), Some(opened)) => {
            weights.read_weight * read_component(pct, opened, now_unix, weights)
        }
        _ => 0.0,
    };
    (ct, rt)
}

/// Score, floor-filter, and order a candidate set: score desc, then
/// `artifact_id` asc — a total, deterministic order.
pub fn rank(
    candidates: Vec<ResurfaceSignals>,
    now_unix: i64,
    weights: &ResurfaceWeights,
) -> Vec<ScoredResurface> {
    let mut out: Vec<ScoredResurface> = candidates
        .into_iter()
        .map(|signals| {
            let (comment_term, read_term) = score(&signals, now_unix, weights);
            ScoredResurface {
                score: comment_term + read_term,
                comment_term,
                read_term,
                signals,
            }
        })
        .filter(|s| s.score >= weights.score_floor)
        .collect();
    out.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.signals.artifact_id.cmp(&b.signals.artifact_id))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_780_000_000;
    const DAY: i64 = 86_400;

    /// The golden tests below pin the SHIPPED default arithmetic — every
    /// call threads this explicitly (never a bare `ResurfaceWeights::default()`
    /// scattered around) so a future edit to the fn signatures can't
    /// silently start ignoring weights again.
    fn w() -> ResurfaceWeights {
        ResurfaceWeights::default()
    }

    fn sig(id: &str, open: u32, pct: Option<u8>, opened: Option<i64>) -> ResurfaceSignals {
        ResurfaceSignals {
            artifact_id: id.into(),
            open_comments: open,
            oldest_open_unix: None,
            completion_pct: pct,
            last_opened_unix: opened,
        }
    }

    #[test]
    fn deterministic_same_inputs_same_output() {
        let mk = || {
            vec![
                sig("bbb", 1, Some(40), Some(NOW - 3 * DAY)),
                sig("aaa", 0, Some(80), Some(NOW - 10 * DAY)),
            ]
        };
        let a = rank(mk(), NOW, &w());
        let b = rank(mk(), NOW, &w());
        let flat = |v: &[ScoredResurface]| {
            v.iter()
                .map(|s| (s.signals.artifact_id.clone(), s.score))
                .collect::<Vec<_>>()
        };
        assert_eq!(flat(&a), flat(&b));
    }

    #[test]
    fn read_term_halves_at_halflife() {
        let fresh = read_component(50, NOW, NOW, &w());
        let stale = read_component(50, NOW - 45 * DAY, NOW, &w());
        let ratio = stale / fresh;
        assert!((0.49..=0.51).contains(&ratio), "ratio {ratio}");
        assert!((fresh - 0.5).abs() < 1e-6);
    }

    #[test]
    fn comment_component_saturates_at_four() {
        assert_eq!(comment_component(4, &w()), 1.0);
        assert_eq!(comment_component(9, &w()), 1.0);
        assert!((comment_component(1, &w()) - 0.25).abs() < 1e-6);
        // The oldest-open timestamp is surfaced, never scored: same count,
        // different ages, same score.
        let young = sig("a", 2, None, None);
        let old = ResurfaceSignals {
            oldest_open_unix: Some(NOW - 300 * DAY),
            ..sig("a", 2, None, None)
        };
        assert_eq!(score(&young, NOW, &w()), score(&old, NOW, &w()));
    }

    #[test]
    fn decayed_read_falls_below_floor_and_is_dropped() {
        // 50% read, 100 idle days: 0.4·0.5·0.5^(100/45) ≈ 0.043 < 0.05.
        let ranked = rank(
            vec![sig("gone", 0, Some(50), Some(NOW - 100 * DAY))],
            NOW,
            &w(),
        );
        assert!(ranked.is_empty());
        // A single open comment (0.15) never fades below the floor.
        let kept = rank(vec![sig("kept", 1, None, None)], NOW, &w());
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn one_comment_beats_shallow_fresh_read_but_not_deep_one() {
        let ranked = rank(
            vec![
                sig("shallow", 0, Some(30), Some(NOW)), // 0.4·0.30 = 0.12
                sig("commented", 1, None, None),        // 0.6·0.25 = 0.15
                sig("deep", 0, Some(80), Some(NOW)),    // 0.4·0.80 = 0.32
            ],
            NOW,
            &w(),
        );
        let ids: Vec<&str> = ranked
            .iter()
            .map(|s| s.signals.artifact_id.as_str())
            .collect();
        assert_eq!(ids, ["deep", "commented", "shallow"]);
    }

    #[test]
    fn equal_scores_tiebreak_by_artifact_id() {
        let ranked = rank(
            vec![sig("zzz", 2, None, None), sig("aaa", 2, None, None)],
            NOW,
            &w(),
        );
        let ids: Vec<&str> = ranked
            .iter()
            .map(|s| s.signals.artifact_id.as_str())
            .collect();
        assert_eq!(ids, ["aaa", "zzz"]);
    }

    #[test]
    fn terms_sum_to_score_for_explain_rendering() {
        let ranked = rank(vec![sig("x", 2, Some(62), Some(NOW - 5 * DAY))], NOW, &w());
        let s = &ranked[0];
        assert!((s.comment_term + s.read_term - s.score).abs() < 1e-6);
        assert!(s.comment_term > 0.0 && s.read_term > 0.0);
    }

    // W2.9 — weights become configurable; a doubled comment_weight must
    // double the comment_term (and leave the read_term untouched), proving
    // the fns genuinely read the passed-in struct rather than a baked-in
    // constant anywhere in the call chain.
    #[test]
    fn custom_comment_weight_scales_comment_term_only() {
        let s = sig("x", 2, Some(50), Some(NOW));
        let (base_ct, base_rt) = score(&s, NOW, &w());

        let doubled = ResurfaceWeights {
            comment_weight: w().comment_weight * 2.0,
            ..w()
        };
        let (ct2, rt2) = score(&s, NOW, &doubled);

        assert!((ct2 - base_ct * 2.0).abs() < 1e-6);
        assert!((rt2 - base_rt).abs() < 1e-6);
    }

    #[test]
    fn custom_saturation_and_halflife_change_the_arithmetic() {
        // Saturation of 1 maxes the comment term at a single open comment.
        let sat1 = ResurfaceWeights {
            comment_saturation: 1,
            ..w()
        };
        assert_eq!(comment_component(1, &sat1), 1.0);
        assert_eq!(comment_component(4, &sat1), 1.0);

        // A shorter half-life decays a stale read faster than the default.
        let short = ResurfaceWeights {
            read_halflife_days: 10.0,
            ..w()
        };
        let default_stale = read_component(50, NOW - 10 * DAY, NOW, &w());
        let short_stale = read_component(50, NOW - 10 * DAY, NOW, &short);
        assert!(short_stale < default_stale);
    }

    #[test]
    fn from_section_none_is_the_default() {
        assert_eq!(
            ResurfaceWeights::from_section(None),
            ResurfaceWeights::default()
        );
    }

    #[test]
    fn from_section_partial_override_keeps_other_defaults() {
        let section = crate::config::ResurfaceSection {
            comment_weight: Some(1.2),
            ..Default::default()
        };
        let resolved = ResurfaceWeights::from_section(Some(&section));
        assert_eq!(resolved.comment_weight, 1.2);
        assert_eq!(
            resolved.read_weight,
            ResurfaceWeights::default().read_weight
        );
        assert_eq!(
            resolved.comment_saturation,
            ResurfaceWeights::default().comment_saturation
        );
        assert_eq!(
            resolved.read_halflife_days,
            ResurfaceWeights::default().read_halflife_days
        );
        assert_eq!(
            resolved.score_floor,
            ResurfaceWeights::default().score_floor
        );
    }

    #[test]
    fn from_section_nonsense_warns_and_falls_back_per_field() {
        let section = crate::config::ResurfaceSection {
            comment_weight: Some(-1.0),
            read_weight: Some(f32::NAN),
            comment_saturation: Some(0),
            read_halflife_days: Some(0.0),
            score_floor: Some(f32::NEG_INFINITY),
        };
        assert_eq!(
            ResurfaceWeights::from_section(Some(&section)),
            ResurfaceWeights::default()
        );
    }
}
