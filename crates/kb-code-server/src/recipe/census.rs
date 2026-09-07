//! The per-step census (V74-L3a, D11: "per-step census — why is this
//! empty").
//!
//! A ranked list that comes back empty is the single most common way a
//! read surface lies: "0 rows" reads as "there is nothing to worry
//! about" whether the truth is *nothing matched*, *the lane is switched
//! off*, *the scope excluded everything*, or *the index has not been
//! built*. `recipes/1` shipped one blunt instrument for this
//! (`inputs_missing`) and three of its six recipes still managed to
//! report an honest-looking empty set over a missing input (the recon's
//! findings 4 and 5, and D11's "gate/row parity" repair).
//!
//! So every step of a `kbc-recipe/1` run carries a census: the INPUT
//! counts it actually read, the FILTERS it actually applied, and — when
//! it returned nothing — one [`EmptyReason`] from a CLOSED vocabulary.
//! The vocabulary is closed for the same reason `rails::Honesty`'s four
//! states and `boards::resolve`'s eleven reasons are: a free-text reason
//! is a reason nobody can test for, and a UI cannot offer "run the
//! backfill" next to a sentence.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Why a step returned nothing. CLOSED.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmptyReason {
    /// The step reads per-input and was given none.
    NoInputs,
    /// The step BEFORE this one returned nothing, so this one had
    /// nothing to work on. Distinct from `no-inputs`: the wiring is
    /// fine, the upstream question just had no answer.
    UpstreamEmpty,
    /// Rows existed and every one of them failed this step's filters.
    /// The ONLY reason that means "nothing to worry about".
    FilteredOut,
    /// Rows existed and the run's kbc-scope/1 selection excluded them
    /// all — a narrowing the caller chose, not a fact about the repo.
    ScopeExcluded,
    /// An `aug-lane/1` lane the recipe names is not enabled in
    /// `[lanes]`. A recipe can never turn one on (invariant 21(a)).
    LaneDisabled,
    /// A lane/orphan-lane name this binary does not know.
    LaneUnknown,
    /// A known lane that could not answer this time (rails/1's own
    /// `unavailable` state, with its reason carried through).
    LaneUnavailable,
    /// The index this step reads has no rows yet — a MISSING INPUT, and
    /// the reason string names the command that builds it.
    NoIndex,
    /// rails/1 found no Rails structure at all.
    NotARailsApp,
    /// A parameter resolved to an empty value, so the step had no
    /// question to ask.
    ParamEmpty,
    /// The run's wall-clock budget was spent before this step started.
    BudgetExhausted,
}

impl EmptyReason {
    pub fn as_str(self) -> &'static str {
        match self {
            EmptyReason::NoInputs => "no-inputs",
            EmptyReason::UpstreamEmpty => "upstream-empty",
            EmptyReason::FilteredOut => "filtered-out",
            EmptyReason::ScopeExcluded => "scope-excluded",
            EmptyReason::LaneDisabled => "lane-disabled",
            EmptyReason::LaneUnknown => "lane-unknown",
            EmptyReason::LaneUnavailable => "lane-unavailable",
            EmptyReason::NoIndex => "no-index",
            EmptyReason::NotARailsApp => "not-a-rails-app",
            EmptyReason::ParamEmpty => "param-empty",
            EmptyReason::BudgetExhausted => "budget-exhausted",
        }
    }

    /// Whether this reason means "the repo is fine". Exactly one does.
    /// A UI that dims an empty step must key on THIS, not on `rows == 0`.
    pub fn is_clean(self) -> bool {
        matches!(self, EmptyReason::FilteredOut)
    }

    pub const ALL: &'static [EmptyReason] = &[
        EmptyReason::NoInputs,
        EmptyReason::UpstreamEmpty,
        EmptyReason::FilteredOut,
        EmptyReason::ScopeExcluded,
        EmptyReason::LaneDisabled,
        EmptyReason::LaneUnknown,
        EmptyReason::LaneUnavailable,
        EmptyReason::NoIndex,
        EmptyReason::NotARailsApp,
        EmptyReason::ParamEmpty,
        EmptyReason::BudgetExhausted,
    ];
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct StepCensus {
    /// Present iff the step returned zero rows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub empty_reason: Option<EmptyReason>,
    /// Named counts of what the step actually READ — `mirror_files`,
    /// `entity_defs`, `path_stats`, `hits`. A reader comparing these to
    /// `rows` can see where the funnel narrowed without guessing.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, i64>,
    /// The filters this step actually applied, rendered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters_applied: Vec<String>,
    /// Anything the step could not do and had to say out loud (a budget
    /// that bit, a scope that refused, an input it skipped rather than
    /// guessed).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl StepCensus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn empty(reason: EmptyReason) -> Self {
        StepCensus {
            empty_reason: Some(reason),
            ..Default::default()
        }
    }

    pub fn input(&mut self, key: &str, n: i64) {
        self.inputs.insert(key.to_string(), n);
    }

    pub fn filter(&mut self, f: impl Into<String>) {
        self.filters_applied.push(f.into());
    }

    pub fn note(&mut self, n: impl Into<String>) {
        self.notes.push(n.into());
    }

    /// Builder form of [`Self::filter`], for the one-line refusal paths.
    pub fn with_filter(mut self, f: impl Into<String>) -> Self {
        self.filters_applied.push(f.into());
        self
    }

    /// A one-line human rendering — what `kb-code recipe run` prints
    /// under an empty step, and what a UI puts in its EmptyState.
    pub fn explain(&self) -> String {
        let Some(r) = self.empty_reason else {
            return String::new();
        };
        let mut s = match r {
            EmptyReason::NoInputs => "no inputs: this step reads per-address and got none".into(),
            EmptyReason::UpstreamEmpty => "the previous step returned nothing".into(),
            EmptyReason::FilteredOut => {
                let read: i64 = self.inputs.values().copied().max().unwrap_or(0);
                format!("read {read} row(s); every one failed this step's filters")
            }
            EmptyReason::ScopeExcluded => "the scope excluded every row".into(),
            EmptyReason::LaneDisabled => "that lane is not enabled in [lanes]".into(),
            EmptyReason::LaneUnknown => "no lane by that name".into(),
            EmptyReason::LaneUnavailable => "the lane could not answer".into(),
            EmptyReason::NoIndex => "the index this step reads is empty".into(),
            EmptyReason::NotARailsApp => "rails/1 found no Rails structure here".into(),
            EmptyReason::ParamEmpty => "a parameter resolved to an empty value".into(),
            EmptyReason::BudgetExhausted => "the run budget was spent before this step".into(),
        };
        if !self.filters_applied.is_empty() {
            s.push_str(" — ");
            s.push_str(&self.filters_applied.join("; "));
        }
        s
    }
}
