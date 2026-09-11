//! `aug-lane/1` — the augmentation-lane registry, fact store and
//! per-request classing (V72-H4a; design of record
//! `docs/research/kb-code-v7-continuum-2026-09.html` §The spine P8,
//! §Decisions D7, Track H).
//!
//! A **lane** is one source of facts about code that kb-code did not
//! derive from its own tree-sitter pipeline: the repository's git history,
//! a coverage run, a linter, a SARIF-emitting scanner. `aug-lane/1`
//! generalises the three ad-hoc integrations this crate already ships
//! (kb-lip, rails-lens, the DCB doc-lens) into ONE registry with one
//! storage shape, one freshness story and one CLI — but it does NOT
//! retrofit them in this unit (that is named work later in Track H), so
//! nothing existing changes shape here.
//!
//! ## The three rules this module exists to keep
//!
//! **1. A lane is enabled ONLY by `[lanes]` in kb-code.toml.** Never by a
//! route, never by a request, and above all never by a file inside a
//! repository — a committed `.kbc/lanes.toml` would be remote code
//! execution by `git clone` (the `.vscode/tasks.json` trap; design §P8's
//! security posture #1). [`LANES`] is a Rust table this binary ships;
//! [`config::LanesSection`](crate::config::LanesSection) is the only thing
//! that turns a row of it on, and the default is empty. With every lane
//! off, every response this daemon already gives is byte-identical.
//!
//! **2. The daemon runs no tool.** kb-code-server's invariant 10 — the
//! daemon never spawns a non-git process — is why the registry has two
//! KINDS rather than the design sketch's four source kinds. A
//! [`LaneKind::Derived`] lane is computed by this daemon from git and the
//! mirror alone; an [`LaneKind::Ingested`] lane's facts arrive as a
//! `lane-ingest/1` POST from `kb-code lanes ingest`, which ran the tool on
//! the operator's own box, over the loopback-only mutation lane. There is
//! no third path, and no configuration flag that could create one. The
//! `exec` source kind the research report describes is therefore not a
//! server feature at all; it is the CLI.
//!
//! **3. The trust class is computed per request and never persisted.**
//! `lane_facts` has no class column. [`classing::class_for`] is the ONE
//! function that turns a stored claim into a class, from
//! `min(lane ceiling, per-fact cap, anchor state)` — so a fact whose blob
//! has moved since the tool ran cannot be read back as fresh, and a lane
//! can never mint above its own ceiling. That is root invariant #2's
//! "kb-code mints classes, nothing is cached" and the lsp-live precedent
//! ("exact ONLY blob-verified, computed-fresh-NEVER-persisted"), applied
//! to somebody else's tool output.
//!
//! Facts are **surfaced, never scored**: nothing in this module is
//! reachable from `search`'s ranking, `resolve.rs`'s `CLASS_*`, `usages2`
//! or `codelens/1`. A lane never gates CI and has no place for a
//! summariser — there is no LLM in this daemon.

pub mod adapters;
pub mod classing;
pub mod gc;
pub mod git_behavior;
pub mod ingest;
pub mod routes;

use serde::{Deserialize, Serialize};

/// The wire schema string every `aug-lane/1` response carries.
pub const LANES_SCHEMA: &str = "aug-lane/1";

/// The schema string a `POST /api/lanes/{lane}/ingest` body must declare.
pub const INGEST_SCHEMA: &str = "lane-ingest/1";

/// Whether the daemon computes a lane's facts itself, or receives them.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LaneKind {
    /// Computed by this daemon from git and the mirror, on demand. The
    /// only processes involved are `git` ones (invariant 10 + the git-argv
    /// discipline of invariant 3).
    Derived,
    /// POSTed by `kb-code lanes ingest` after the OPERATOR ran the tool on
    /// their own box. Loopback-only, audited, capped.
    Ingested,
}

/// What a lane's facts were exposed to. Two values, both of which a real
/// row uses — an `off_box` class (for the design's future `http` lanes
/// that hand source bytes to a remote endpoint, structurally capped at
/// `candidate`) is deliberately NOT declared here, because nothing in this
/// unit would ever produce one and a declared-but-empty vocabulary is the
/// v7.0 dead-surface defect (`usages2::UNMINTED_KINDS`' precedent).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    /// Derived by this daemon from the repository's own git history. No
    /// process outside this daemon and `git` ever saw a byte.
    Local,
    /// Produced by a tool the operator ran on their own box; the payload
    /// is that tool's output, and the source never left the machine.
    LocalTool,
}

/// The four read states a fact can be in, ordered so `min` is meaningful
/// (`Orphan < Candidate < Likely < Exact`). This is the SAME vocabulary
/// `resolve.rs` and `entities::class_for` mint in, with `orphan` — the
/// honest "nothing matched" state — as its own floor rather than a
/// silently dropped row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustClass {
    Orphan,
    Candidate,
    Likely,
    Exact,
}

impl TrustClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Orphan => "orphan",
            Self::Candidate => "candidate",
            Self::Likely => "likely",
            Self::Exact => "exact",
        }
    }
}

/// One registry row. A row with `family_prefix` set is a TEMPLATE, not an
/// addressable lane: `sarif.*` describes every SARIF-emitting tool, and
/// `sarif.brakeman` is an INSTANCE of it that exists because the operator
/// named it in `[lanes] enabled`. That is how "a lane per tool, declared
/// in config" coexists with one declaration of the fact schema, the
/// ceiling and the kinds.
#[derive(Debug, Clone, Copy)]
pub struct LaneSpec {
    /// The addressable id, or the template id (`"sarif.*"`).
    pub id: &'static str,
    pub title: &'static str,
    pub kind: LaneKind,
    /// The shape of `value_json`, versioned independently of `aug-lane/1`
    /// itself so a lane can grow its payload without a registry bump.
    pub fact_schema: &'static str,
    /// The closed set of `lane_facts.kind` values this lane may write. The
    /// ingest route enforces it — an unknown kind is refused by row, not
    /// stored and rendered as a mystery.
    pub fact_kinds: &'static [&'static str],
    pub sensitivity: Sensitivity,
    /// The lane can never mint above this, whatever the anchor state says.
    pub trust_ceiling: TrustClass,
    /// Days after ingest a run (and its facts) age out, unless
    /// `[lanes.retention_days]` overrides it.
    pub retention_days_default: u32,
    /// `builtin:<name>` — which parser `kb-code lanes ingest` runs. `None`
    /// for a derived lane, which has no external input to parse.
    pub adapter: Option<&'static str>,
    /// `Some("sarif.")` marks this row a template; instances are
    /// `<prefix><tool>`.
    pub family_prefix: Option<&'static str>,
    /// What the operator would RUN to make this lane answer — echoed by
    /// `GET /api/lanes/facts` beside an enabled lane that has nothing to
    /// say about a path. The design's "a miss is not an error and not a
    /// silent blank" rule; `None` for a lane with no command behind it.
    pub refresh_hint: Option<&'static str>,
}

impl LaneSpec {
    pub fn is_family(&self) -> bool {
        self.family_prefix.is_some()
    }
}

/// `git.behavior`'s id, as the derived-lane code and its tests address it.
pub const GIT_BEHAVIOR: &str = "git.behavior";
/// `coverage.simplecov`'s id.
pub const COVERAGE_SIMPLECOV: &str = "coverage.simplecov";
/// `rubocop`'s id.
pub const RUBOCOP: &str = "rubocop";
/// The SARIF family template's id — never addressable itself.
pub const SARIF_FAMILY: &str = "sarif.*";
/// The prefix an addressable SARIF instance carries.
pub const SARIF_PREFIX: &str = "sarif.";

/// **The registry.** Four rows: three concrete lanes and one family
/// template. Adding a lane means adding a row here — never a second match
/// on a lane id somewhere else (the `syntax::REGISTRY` discipline).
pub const LANES: &[LaneSpec] = &[
    LaneSpec {
        id: GIT_BEHAVIOR,
        title: "Git behaviour",
        kind: LaneKind::Derived,
        fact_schema: "lane-fact/git.behavior/1",
        fact_kinds: &["churn", "co_change", "last_touch"],
        sensitivity: Sensitivity::Local,
        trust_ceiling: TrustClass::Exact,
        // Derived facts are never stored, so this window governs nothing
        // today; it is declared for the same reason the row declares a
        // ceiling — one shape for every lane, and the moment a derived
        // lane starts persisting it inherits a policy rather than needing
        // one invented.
        retention_days_default: 30,
        adapter: None,
        family_prefix: None,
        refresh_hint: None,
    },
    LaneSpec {
        id: COVERAGE_SIMPLECOV,
        title: "Test coverage (SimpleCov)",
        kind: LaneKind::Ingested,
        fact_schema: "lane-fact/coverage/1",
        fact_kinds: &["coverage", "coverage_summary"],
        sensitivity: Sensitivity::LocalTool,
        trust_ceiling: TrustClass::Exact,
        retention_days_default: 30,
        adapter: Some("builtin:simplecov"),
        family_prefix: None,
        refresh_hint: Some(
            "bundle exec rspec  # then: kb-code lanes ingest coverage.simplecov --repo <r> \
             --file coverage/.resultset.json",
        ),
    },
    LaneSpec {
        id: RUBOCOP,
        title: "RuboCop offenses",
        kind: LaneKind::Ingested,
        fact_schema: "lane-fact/diagnostic/1",
        fact_kinds: &["diagnostic"],
        sensitivity: Sensitivity::LocalTool,
        trust_ceiling: TrustClass::Exact,
        retention_days_default: 14,
        adapter: Some("builtin:rubocop"),
        family_prefix: None,
        refresh_hint: Some(
            "rubocop --format json --out rubocop.json  # then: kb-code lanes ingest rubocop \
             --repo <r> --file rubocop.json",
        ),
    },
    LaneSpec {
        id: SARIF_FAMILY,
        title: "SARIF 2.1.0 results",
        kind: LaneKind::Ingested,
        fact_schema: "lane-fact/diagnostic/1",
        fact_kinds: &["diagnostic"],
        sensitivity: Sensitivity::LocalTool,
        trust_ceiling: TrustClass::Exact,
        retention_days_default: 30,
        adapter: Some("builtin:sarif"),
        family_prefix: Some(SARIF_PREFIX),
        refresh_hint: Some(
            "<scanner> --format sarif > results.sarif  # then: kb-code lanes ingest sarif \
             --repo <r> --file results.sarif --lane sarif.<tool>",
        ),
    },
];

/// A lane id that resolved against the registry — either a concrete row,
/// or an instance minted from a family template.
#[derive(Debug, Clone)]
pub struct ResolvedLane {
    pub id: String,
    pub spec: &'static LaneSpec,
}

impl ResolvedLane {
    pub fn kind(&self) -> LaneKind {
        self.spec.kind
    }
    pub fn ceiling(&self) -> TrustClass {
        self.spec.trust_ceiling
    }
    pub fn accepts_kind(&self, kind: &str) -> bool {
        self.spec.fact_kinds.contains(&kind)
    }
}

/// Resolve `id` against [`LANES`].
///
/// A family TEMPLATE id (`"sarif.*"`) resolves to `None` on purpose — it
/// is a declaration, not an address, and letting it through would create
/// one lane where the operator meant one lane per tool. An instance needs
/// a non-empty, `.`-free tail so `sarif.` and `sarif.a.b` are both
/// refused rather than silently becoming lanes.
pub fn resolve(id: &str) -> Option<ResolvedLane> {
    if let Some(spec) = LANES.iter().find(|l| !l.is_family() && l.id == id) {
        return Some(ResolvedLane {
            id: id.to_string(),
            spec,
        });
    }
    for spec in LANES.iter().filter(|l| l.is_family()) {
        let prefix = spec.family_prefix.expect("is_family");
        if let Some(tail) = id.strip_prefix(prefix) {
            if !tail.is_empty() && !tail.contains('.') && tail != "*" {
                return Some(ResolvedLane {
                    id: id.to_string(),
                    spec,
                });
            }
        }
    }
    None
}

/// Every lane the operator has turned on, in registry order then config
/// order. A configured id that resolves against nothing is NOT silently
/// dropped — [`unknown_enabled`] reports it so `GET /api/lanes` and
/// `kb-code lanes list` can name the typo.
pub fn enabled(cfg: &crate::config::LanesSection) -> Vec<ResolvedLane> {
    let mut out: Vec<ResolvedLane> = Vec::new();
    for spec in LANES.iter().filter(|l| !l.is_family()) {
        if cfg.is_enabled(spec.id) {
            out.push(ResolvedLane {
                id: spec.id.to_string(),
                spec,
            });
        }
    }
    for id in &cfg.enabled {
        if LANES.iter().any(|l| !l.is_family() && l.id == *id) {
            continue;
        }
        if let Some(r) = resolve(id) {
            out.push(r);
        }
    }
    out
}

/// Configured lane ids that resolve against no registry row — reported,
/// never ignored.
pub fn unknown_enabled(cfg: &crate::config::LanesSection) -> Vec<String> {
    cfg.enabled
        .iter()
        .filter(|id| resolve(id).is_none())
        .cloned()
        .collect()
}

/// `(lane_id, oldest_ingested_at_to_keep)` for every ENABLED lane that
/// stores facts, at `now`. A lane the operator turned off is deliberately
/// absent: its rows stop being read, but they are not deleted out from
/// under a re-enable — retention is a policy over live lanes, not a purge
/// of disabled ones.
pub fn retention_cutoffs(cfg: &crate::config::LanesSection, now_unix: i64) -> Vec<(String, i64)> {
    enabled(cfg)
        .into_iter()
        .filter(|l| l.spec.kind == LaneKind::Ingested)
        .map(|l| {
            let days = cfg.retention_days(&l.id, l.spec.retention_days_default) as i64;
            (l.id, now_unix - days * 86_400)
        })
        .collect()
}

// --- route contracts (invariant 15) --------------------------------------

use crate::entities::RouteContract;

fn lanes_params_accept_without(_omit: &str) -> bool {
    // `GET /api/lanes` answers with no params at all (`?repo=` only
    // narrows the counts), so there is nothing it can be missing.
    true
}

fn facts_params_accept_without(omit: &str) -> bool {
    accepts_without::<routes::FactsParams>(&[("repo", "r"), ("path", "a.rb")], omit)
}

fn summary_params_accept_without(omit: &str) -> bool {
    accepts_without::<routes::SummaryParams>(&[("repo", "r")], omit)
}

fn ingest_params_accept_without(omit: &str) -> bool {
    accepts_without::<ingest::IngestParams>(&[("repo", "r")], omit)
}

fn accepts_without<T: serde::de::DeserializeOwned>(pairs: &[(&str, &str)], omit: &str) -> bool {
    let mut map = serde_json::Map::new();
    for (k, v) in pairs {
        if *k != omit {
            map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
        }
    }
    serde_json::from_value::<T>(serde_json::Value::Object(map)).is_ok()
}

pub const LANES_ROUTE: RouteContract = RouteContract {
    path: "/api/lanes",
    handler: "lanes::routes::lanes_route",
    required_params: &[],
    params_accept_without: lanes_params_accept_without,
};

pub const LANE_FACTS_ROUTE: RouteContract = RouteContract {
    path: "/api/lanes/facts",
    handler: "lanes::routes::facts_route",
    required_params: &["repo", "path"],
    params_accept_without: facts_params_accept_without,
};

pub const LANE_SUMMARY_ROUTE: RouteContract = RouteContract {
    path: "/api/lanes/summary",
    handler: "lanes::routes::summary_route",
    required_params: &["repo"],
    params_accept_without: summary_params_accept_without,
};

pub const LANE_INGEST_ROUTE: RouteContract = RouteContract {
    path: "/api/lanes/{lane}/ingest",
    handler: "lanes::ingest::ingest_route",
    required_params: &["repo"],
    params_accept_without: ingest_params_accept_without,
};

pub const V72_H4A_ROUTES: &[RouteContract] = &[
    LANES_ROUTE,
    LANE_FACTS_ROUTE,
    LANE_SUMMARY_ROUTE,
    LANE_INGEST_ROUTE,
];

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTER_SRC: &str = include_str!("../router.rs");

    fn cfg(enabled: &[&str]) -> crate::config::LanesSection {
        crate::config::LanesSection {
            enabled: enabled.iter().map(|s| s.to_string()).collect(),
            retention_days: Default::default(),
        }
    }

    #[test]
    fn every_declared_v72_h4a_route_is_registered_and_requires_its_params() {
        for c in V72_H4A_ROUTES {
            let nested = c.path.strip_prefix("/api").expect("/api-prefixed");
            assert!(
                ROUTER_SRC.contains(&format!("\"{nested}\"")),
                "{}: no `.route(\"{nested}\", ...)` in router.rs",
                c.path
            );
            assert!(
                ROUTER_SRC.contains(c.handler),
                "{}: handler {} is not wired in router.rs",
                c.path,
                c.handler
            );
            assert!(
                (c.params_accept_without)(""),
                "{}: its own params struct rejects a complete query",
                c.path
            );
            for p in c.required_params {
                assert!(
                    !(c.params_accept_without)(p),
                    "{}: params deserialize without required {p:?}",
                    c.path
                );
            }
        }
    }

    #[test]
    fn registry_ids_are_unique_and_kinds_are_closed() {
        let mut seen = std::collections::BTreeSet::new();
        for l in LANES {
            assert!(seen.insert(l.id), "duplicate lane id {}", l.id);
            assert!(!l.fact_kinds.is_empty(), "{}: no fact kinds", l.id);
            let mut kinds = std::collections::BTreeSet::new();
            for k in l.fact_kinds {
                assert!(kinds.insert(*k), "{}: duplicate fact kind {k}", l.id);
            }
            match l.kind {
                LaneKind::Derived => assert!(
                    l.adapter.is_none(),
                    "{}: a derived lane parses no external input",
                    l.id
                ),
                LaneKind::Ingested => assert!(
                    l.adapter.is_some(),
                    "{}: an ingested lane needs an adapter name",
                    l.id
                ),
            }
            assert!(
                l.retention_days_default > 0,
                "{}: retention must be a positive window",
                l.id
            );
        }
    }

    #[test]
    fn a_family_template_is_not_addressable_but_its_instances_are() {
        assert!(resolve(SARIF_FAMILY).is_none());
        assert!(resolve("sarif.").is_none());
        assert!(resolve("sarif.a.b").is_none());
        let r = resolve("sarif.brakeman").expect("an instance resolves");
        assert_eq!(r.id, "sarif.brakeman");
        assert_eq!(r.spec.id, SARIF_FAMILY);
        assert_eq!(r.ceiling(), TrustClass::Exact);
        assert!(r.accepts_kind("diagnostic"));
        assert!(!r.accepts_kind("coverage"));
    }

    #[test]
    fn nothing_is_enabled_by_default_and_a_typo_is_reported_not_ignored() {
        let empty = crate::config::LanesSection::default();
        assert!(enabled(&empty).is_empty());
        assert!(unknown_enabled(&empty).is_empty());

        let c = cfg(&["git.behavior", "sarif.brakeman", "coverage.simplecoV"]);
        let ids: Vec<String> = enabled(&c).into_iter().map(|l| l.id).collect();
        assert_eq!(ids, vec!["git.behavior", "sarif.brakeman"]);
        assert_eq!(unknown_enabled(&c), vec!["coverage.simplecoV".to_string()]);
    }

    #[test]
    fn retention_cutoffs_cover_enabled_ingested_lanes_only() {
        let mut c = cfg(&["git.behavior", "rubocop", "sarif.brakeman"]);
        c.retention_days.insert("rubocop".into(), 3);
        let cutoffs = retention_cutoffs(&c, 1_000_000);
        let by_lane: std::collections::BTreeMap<_, _> = cutoffs.into_iter().collect();
        // The derived lane stores nothing, so it is never swept.
        assert!(!by_lane.contains_key("git.behavior"));
        assert_eq!(by_lane["rubocop"], 1_000_000 - 3 * 86_400);
        assert_eq!(by_lane["sarif.brakeman"], 1_000_000 - 30 * 86_400);
    }

    #[test]
    fn a_zero_retention_override_falls_back_to_the_registry_default() {
        let mut c = cfg(&["rubocop"]);
        c.retention_days.insert("rubocop".into(), 0);
        let cutoffs = retention_cutoffs(&c, 1_000_000);
        assert_eq!(
            cutoffs,
            vec![("rubocop".to_string(), 1_000_000 - 14 * 86_400)]
        );
    }

    #[test]
    fn trust_classes_order_so_min_is_the_weaker_of_two() {
        assert!(TrustClass::Orphan < TrustClass::Candidate);
        assert!(TrustClass::Candidate < TrustClass::Likely);
        assert!(TrustClass::Likely < TrustClass::Exact);
        assert_eq!(
            TrustClass::Exact.min(TrustClass::Candidate),
            TrustClass::Candidate
        );
    }
}
