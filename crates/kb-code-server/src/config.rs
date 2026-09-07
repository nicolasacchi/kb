//! `kb-code.toml` — schema v1. Lives at `<config-dir>/kb-code.toml`, where
//! `<config-dir>` is `KbPaths::new("kb-code").config` (`kb_core::paths` —
//! the same `KB_HOME`/`KB_CONFIG_DIR` layout kb itself uses; W1.2's plan
//! note: `KbPaths::new`'s config/cache roots are the fixed app-name `kb`
//! directory regardless of the daemon-name argument passed in — only the
//! *state* dir is namespaced per daemon — so zero collision with kb.toml
//! comes from the distinct **filename** `kb-code.toml`, not a distinct
//! directory).
//!
//! Six sections: `[server]` (kb's `addr = "host:port"` idiom, NOT a bare
//! port — matches `kb_core::config::ServerSection`), `[[repos]]` (an array
//! of `{ name, path }` tables, the corpora kb-code browses), `[watcher]`
//! (W1.4 — the live-mirror watcher's backend selection; see
//! `crate::mirror::parse_watch_mode` for the `mode` string's grammar),
//! `[semantic]` (W2.3 — the semantic search lane's off-by-default, per-repo
//! staged-rollout config; see `SemanticSection`'s doc), `[transcripts]`
//! (W2.5 — the raw-transcripts full-text lane; see `TranscriptsSection`),
//! and `[doclens]` (DCB W1.C — the doc↔code bridge's kb allowlist, browser
//! origin allowlist and cost knobs, plus W3.A's background-sync knobs
//! `sync_interval_secs`/`sync_on_boot`; see [`DoclensSection`]).
//!
//! (The enumeration above names the sections a hand-written `kb-code.toml`
//! is expected to carry; several more — `[kb_daemon]`, `[backfill]`,
//! `[github]`, `[occurrences]`, `[scopes]`, `[review]`, `[behavioral]`,
//! `[scip]` — have accreted since and are documented on their own structs
//! below.)

use kb_core::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Top-level `kb-code.toml`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KbCodeConfig {
    #[serde(default)]
    pub server: ServerSection,

    /// `[[repos]]` — the repos kb-code browses. Empty by default (a
    /// freshly-installed kb-code has nothing configured yet); `load`
    /// canonicalizes each path and drops (with a `tracing::warn!`) any
    /// entry that doesn't resolve to a real git repository, rather than
    /// failing the whole daemon boot over one stale entry.
    #[serde(default)]
    pub repos: Vec<RepoEntry>,

    /// `[watcher]` — W1.4's live-mirror watcher backend selection. Stored
    /// as a raw string (mirrors `ServerSection::addr`'s convention: config
    /// structs hold strings, the owning module parses them) — see
    /// `crate::mirror::parse_watch_mode`.
    #[serde(default)]
    pub watcher: WatcherSection,

    /// `[semantic]` — W2.3's semantic search lane: off by default, PER-REPO
    /// staged rollout (see `SemanticSection`'s doc — a cold-fleet embed is
    /// hours of wall-clock work).
    #[serde(default)]
    pub semantic: SemanticSection,

    /// `[transcripts]` — W2.5's PULL-ONLY raw-transcripts full-text lane
    /// (`crate::transcripts`). See [`TranscriptsSection`].
    #[serde(default)]
    pub transcripts: TranscriptsSection,

    /// `[kb_daemon]` — W2.4's federation target for the Search-Everywhere
    /// box's SESSIONS lane (`~query`, `GET /api/sessions/recollect` on the
    /// user's own `kb` daemon — a DIFFERENT process/crate entirely, see
    /// [`KbDaemonSection`]).
    #[serde(default)]
    pub kb_daemon: KbDaemonSection,

    /// `[backfill]` — W3.6's join-ladder precompute (`crate::join::
    /// backfill`): how far back it walks a repo's commit history, and
    /// whether a run is kicked off automatically at daemon boot. See
    /// [`BackfillSection`].
    #[serde(default)]
    pub backfill: BackfillSection,

    /// `[github]` — Phase G-server's GitHub READ overlay (`crate::github`):
    /// PR listing/comments plus the `refs/kbc/pr/<n>` fetch. See
    /// [`GithubSection`].
    #[serde(default)]
    pub github: GithubSection,

    /// `[occurrences]` — B5a's cost knob for the token-level occurrences
    /// pass (`crate::occurrences`): ON by default (unlike `[semantic]`'s
    /// off-by-default allowlist), with a per-repo DENYLIST rather than an
    /// allowlist — see [`OccurrencesSection`]'s doc for why the polarity is
    /// flipped relative to `[semantic]`.
    #[serde(default)]
    pub occurrences: OccurrencesSection,

    /// `[scopes]` — Phase N named path-set globs (`ScopesSection`). Free-form
    /// map of name → list of globs; no built-in names hardcoded in logic.
    /// Consumed by `GET /api/todos?scope=` (include) / `?scope=!<name>`
    /// (exclude). See [`ScopesSection`].
    #[serde(default)]
    pub scopes: ScopesSection,

    /// `[review]` — V3.R1 local review sessions (`crate::reviews`):
    /// auto-capture of patchsets when an open review's `head_ref` tip
    /// moves, and the per-review patchset ceiling. See [`ReviewSection`].
    #[serde(default)]
    pub review: ReviewSection,

    /// `[behavioral]` — V3.2-B1 history counters for hotspots / coupling /
    /// ownership / age (`crate::behavioral`). See [`BehavioralSection`].
    #[serde(default)]
    pub behavioral: BehavioralSection,

    /// `[doclens]` — DCB W1.C's doc↔code bridge (`crate::doclens`). OFF by
    /// default (`kbs` empty). See [`DoclensSection`].
    #[serde(default)]
    pub doclens: DoclensSection,

    /// `[scip]` — PRR-N12 (N1)'s per-repo SCIP-indexer config
    /// (`crate::scip`, `kb-code scip run`). Empty `repos` by default (no
    /// repo carries exact-tier SCIP automation until explicitly
    /// configured). See [`ScipSection`].
    #[serde(default)]
    pub scip: ScipSection,

    /// `[rails_lens]` — PRR-N3's per-repo override for `frameworks::rails`
    /// detection (`crate::frameworks::rails::detect_is_rails` auto-detects
    /// `config/routes.rb` + a `Gemfile` `gem "rails"`/`gem 'rails'` line).
    /// See [`RailsLensSection`].
    #[serde(default)]
    pub rails_lens: RailsLensSection,

    /// `[[intel.providers]]` — PRR-L2's lip/1 provider registry
    /// (`crate::lip`, design-lip.md's "kb-code-server integration" +
    /// design-addendum-2.md §D). Empty by default (no repo carries a
    /// live-LSP overlay until explicitly configured). See [`IntelSection`].
    #[serde(default)]
    pub intel: IntelSection,

    /// `[search]` — V71-D1's per-signal ranking flags for the
    /// Search-Everywhere box (`search::Factors`). Every field has a
    /// default, so an existing `kb-code.toml` with no `[search]` table
    /// keeps the shipped ranking. See [`SearchSection`].
    #[serde(default)]
    pub search: SearchSection,
    #[serde(default)]
    pub lanes: LanesSection,

    /// `[branches]` — V75-M3's `branch-facts/1` knobs (today: the D18
    /// agent-provenance email set). Every field has a default, so an
    /// existing `kb-code.toml` with no `[branches]` table keeps the
    /// shipped behaviour. See [`BranchesSection`].
    #[serde(default)]
    pub branches: BranchesSection,

    /// `[comments]` — V72-J1's `comments/1` annotation keyword grammar
    /// (`crate::comments::keywords`). Empty by default, which resolves to
    /// the shipped eight-keyword set; a non-empty list REPLACES it
    /// wholesale. See [`CommentsSection`].
    #[serde(default)]
    pub comments: CommentsSection,

    /// `[trails]` — V74-L3b's `kbc-trail/1` navigation record
    /// (`crate::trails`). **OFF by default**, and that default is a
    /// design ruling rather than a conservative choice: D17 permits a
    /// server-side attention ledger only as "an explicit opt-in that is
    /// off on first boot". See [`TrailsSection`].
    #[serde(default)]
    pub trails: TrailsSection,

    /// `[security]` — V70-A2's local-daemon hardening knobs (SEC-13's
    /// server-enforced secret denylist + SEC-02's strict-request-header
    /// opt-in). Every field has a safe default, so an existing
    /// `kb-code.toml` with no `[security]` table keeps the shipped
    /// posture. See [`SecuritySection`].
    #[serde(default)]
    pub security: SecuritySection,
}

/// `[trails]` — `kbc-trail/1`'s three knobs (V74-L3b, design D17).
///
/// `enabled` is the OPERATOR's master switch and it defaults to `false`.
/// That is not a cautious default that could be flipped for convenience:
/// D17 permits a server-side attention ledger only in a milestone that
/// ships pause, purge and retention together, behind "an explicit opt-in
/// that is off on first boot with a visible indicator". With this `false`
/// (or the section absent entirely — the common case) every trail WRITE
/// route refuses, `GET /api/trails/state` reports `enabled: false`, and
/// the retention sweep starts no task.
///
/// The runtime mode (`off` / `recording` / `paused`) is a SEPARATE,
/// persisted decision in the `trails_state` table, changed only by the
/// loopback-only, audited `POST /api/trails/state`. Two switches, and they
/// mean different things: this key is "the operator permits the feature to
/// exist", the row is "the operator has turned it on right now". A fresh
/// volume reads `off` even with this `true`, because the absence of a
/// decision is not a decision to record.
///
/// ```toml
/// [trails]
/// enabled = true
/// retention_days = 30
/// step_granularity_secs = 1
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrailsSection {
    /// The master switch. `false` (the default) ⇒ nothing is ever
    /// recorded and no mode transition is possible.
    #[serde(default)]
    pub enabled: bool,
    /// How long a trail survives before the background sweep
    /// (`trails::gc`) removes it, in days. `0` disables the sweep
    /// entirely, which is an operator's explicit "keep everything"
    /// (surfaced in `GET /api/trails/state`), never a silent default.
    #[serde(default = "default_trails_retention_days")]
    pub retention_days: u32,
    /// The dwell floor, in seconds. A step's dwell is `left_at -
    /// entered_at` FLOORED to a multiple of this — the "never finer than
    /// dwell-per-step" rule made arithmetic. Values below 1 are treated
    /// as 1 by `trails::quantise_dwell`.
    #[serde(default = "default_trails_step_granularity_secs")]
    pub step_granularity_secs: i64,
}

fn default_trails_retention_days() -> u32 {
    30
}

fn default_trails_step_granularity_secs() -> i64 {
    1
}

impl Default for TrailsSection {
    fn default() -> Self {
        Self {
            enabled: false,
            retention_days: default_trails_retention_days(),
            step_granularity_secs: default_trails_step_granularity_secs(),
        }
    }
}

/// `[comments]` — the `comments/1` annotation keyword grammar (V72-J1).
///
/// `keywords` REPLACES the shipped default set rather than extending it,
/// which is the whole point: the default is a vocabulary (RuboCop's six
/// plus the two markers the pre-`comments/1` TODO index scanned), and a
/// team whose codebase uses `DEBT`/`PERF` instead wants those EIGHT gone,
/// not eight more. An all-blank list falls back to the defaults rather
/// than indexing zero annotations
/// (`comments::KeywordSet::from_config`).
///
/// The effective set is part of the per-row `comments_version` key
/// (`comments::comments_version_for`), so changing it re-extracts every
/// file instead of leaving rows classified under the old vocabulary. It
/// is resolved ONCE at boot — same no-live-reload posture as
/// `[occurrences]`/`[scopes]` — and threaded to every `ingest::index_file`
/// call, so the boot walk and a later watcher event can never disagree.
/// `GET /api/comments/keywords` reports what is actually in force.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CommentsSection {
    /// Uppercase annotation keywords. Empty ⇒ the shipped default set.
    #[serde(default)]
    pub keywords: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatcherSection {
    /// `"auto"` (default) | `"notify"` | `"poll"` — parsed via
    /// `crate::mirror::parse_watch_mode`, which warns and falls back to
    /// `"auto"` on anything else (including kb's OWN `"native"` spelling —
    /// deliberately not accepted here, see that function's doc).
    #[serde(default = "WatcherSection::default_mode")]
    pub mode: String,
}

impl WatcherSection {
    fn default_mode() -> String {
        "auto".to_string()
    }
}

impl Default for WatcherSection {
    fn default() -> Self {
        Self {
            mode: Self::default_mode(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerSection {
    /// Listen address, `host:port` — kb's own idiom (`kb_core::config::
    /// ServerSection::addr`), deliberately NOT a bare port so the same
    /// config shape supports a non-loopback bind without a schema change.
    #[serde(default = "ServerSection::default_addr")]
    pub addr: String,

    /// V70-A2 (SEC-02) — the NON-loopback `Host:` values this daemon
    /// answers to, beside the always-allowed loopback forms
    /// (`localhost`/`127.0.0.1`/`[::1]`/`::1`, any port). Empty by default,
    /// which is what a pure dev-box install wants; a reverse-proxied
    /// deployment (prod `kbc.example.com`) must list its public hostname here to
    /// get strict Host checking on every request rather than only on the
    /// loopback-bypass path — see [`crate::security::origin`]'s module doc
    /// for the exact admission table and why an EMPTY list deliberately
    /// does not break an already-deployed proxy.
    #[serde(default)]
    pub hostnames: Vec<String>,

    /// V70-A2 (SEC-15) — permits on the daemon-wide git-subprocess fan-out
    /// semaphore ([`crate::state::AppState::git_fanout`]). Bounds how many
    /// `git` children the merge-check / branches ahead-behind lanes may
    /// have in flight AT ONCE across every request on a documented
    /// IO-bound host; `0` is coerced to 1 (a semaphore with no permits
    /// would wedge those two routes forever).
    #[serde(default = "ServerSection::default_git_fanout")]
    pub git_fanout: usize,
}

impl ServerSection {
    /// `127.0.0.1:4747` — kb's daemon default is `:4000`; kb-code picks an
    /// adjacent-but-distinct default port so both can run side by side
    /// without a config edit.
    pub const DEFAULT_ADDR: &'static str = "127.0.0.1:4747";

    /// V70-A2 — four concurrent git children, the number the design brief
    /// names. Small on purpose: this host is IO-bound (RAID5), and the
    /// lanes this bounds are fan-outs, not single-shot reads.
    pub const DEFAULT_GIT_FANOUT: usize = 4;

    fn default_addr() -> String {
        Self::DEFAULT_ADDR.to_string()
    }

    fn default_git_fanout() -> usize {
        Self::DEFAULT_GIT_FANOUT
    }

    /// The configured fan-out, coerced to at least one permit.
    pub fn resolved_git_fanout(&self) -> usize {
        self.git_fanout.max(1)
    }
}

impl Default for ServerSection {
    fn default() -> Self {
        Self {
            addr: Self::default_addr(),
            hostnames: Vec::new(),
            git_fanout: Self::default_git_fanout(),
        }
    }
}

/// `[security]` — V70-A2's local-daemon hardening knobs.
///
/// `secret_globs` is ADDITIVE to the built-in denylist
/// ([`crate::security::secrets::BUILTIN_SECRET_GLOBS`]): an operator can
/// widen the policy but never narrow it from config, so a repo can never
/// re-open `.env` by shipping its own `kb-code.toml`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SecuritySection {
    /// Extra denylist globs, additive to the built-ins. Basename patterns
    /// (`*.pem`) match any directory; a pattern containing `/`
    /// (`config/credentials/*`) matches the repo-relative path.
    #[serde(default)]
    pub secret_globs: Vec<String>,

    /// When `true`, the `X-Kbc-Request: 1` mutation header is required on
    /// EVERY mutating `/api` request, not only on the browser-originated
    /// ones (those that carry an `Origin` header). Default `false` — the
    /// same "absent Origin = a local process, not a confused deputy"
    /// posture `kb_server::middleware::origin_allowlist` already records,
    /// so curl/CLI/in-process tests keep working unchanged. See
    /// [`crate::security::origin`]'s module doc.
    #[serde(default)]
    pub strict_request_header: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoEntry {
    pub name: String,
    pub path: PathBuf,
}

/// `[search]` — V71-D1 (design D3): one boolean per ranking FACTOR,
/// mapped straight onto [`crate::search::Factors`].
///
/// kb's MI-W5.R amendment is the precedent this section follows exactly: a
/// single omnibus "smart ranking" switch is what forced that milestone to
/// split its own flag after the bench measured only ONE of its two factors,
/// so each factor here gets its own key, its own documented default and its
/// own line in `explain:1`'s decomposition — and a factor whose flag is off
/// is SKIPPED, never multiplied in as a neutral 1.0.
///
/// ```toml
/// [search]
/// frecency = true            # files lane: open-history recency boost
/// demote_generated = false   # vendored/generated paths score lower
/// lexical_rarity = true      # text lane: rank by matched-atom rarity
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchSection {
    pub frecency: bool,
    pub demote_generated: bool,
    pub lexical_rarity: bool,
}

impl Default for SearchSection {
    fn default() -> Self {
        // Deliberately delegated to `search::Factors::default()` rather than
        // repeated here: two default tables that could disagree is exactly
        // how a documented default and a shipped default drift apart.
        let d = crate::search::Factors::default();
        Self {
            frecency: d.frecency,
            demote_generated: d.demote_generated,
            lexical_rarity: d.lexical_rarity,
        }
    }
}

impl SearchSection {
    /// The runtime view every lane reads (`AppState::search_factors`).
    pub fn factors(&self) -> crate::search::Factors {
        crate::search::Factors {
            frecency: self.frecency,
            demote_generated: self.demote_generated,
            lexical_rarity: self.lexical_rarity,
        }
    }
}

/// `[branches]` — V75-M3's `branch-facts/1` knobs. One field today, and
/// it exists because the D18 agent-provenance ladder's `likely` rung is an
/// EMAIL match: the addresses an operator's own agent harness commits
/// under are a deployment fact this binary cannot know.
///
/// Empty (the default) resolves to
/// [`crate::history::facts::DEFAULT_AGENT_EMAILS`] rather than to "no
/// `likely` rung at all" — a `likely` rung that is dead unless configured
/// would be a surface that silently does nothing. A non-empty list
/// REPLACES the default wholesale (the `[comments] keywords` precedent),
/// so an operator can also narrow it to nothing meaningful on purpose.
///
/// Note what is deliberately NOT configurable: the `exact` rung. A trailer
/// naming the run is evidence regardless of deployment, and letting config
/// widen `exact` would be the one way to get "a human's commit labelled
/// agent" back.
///
/// ```toml
/// [branches]
/// agent_emails = ["noreply@anthropic.com", "bot@example.invalid"]
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BranchesSection {
    #[serde(default)]
    pub agent_emails: Vec<String>,
}

impl BranchesSection {
    /// The effective agent-email set — the configured list, or the shipped
    /// default when it is empty.
    pub fn resolved_agent_emails(&self) -> Vec<String> {
        if self.agent_emails.is_empty() {
            crate::history::facts::DEFAULT_AGENT_EMAILS
                .iter()
                .map(|s| s.to_string())
                .collect()
        } else {
            self.agent_emails.clone()
        }
    }
}

/// `[lanes]` — V72-H4a's `aug-lane/1` gate. The registry
/// (`crate::lanes::LANES`) is a Rust table this binary ships; THIS section
/// is the only thing that turns a row of it on.
///
/// Two rules, both load-bearing and both stated in the design's own
/// security posture (§P8 #1): a lane is enabled ONLY here — never by a
/// route, never by a request, and never by a file inside a repository (a
/// committed `.kbc/lanes.toml` would be remote code execution by `git
/// clone`, the `.vscode/tasks.json` trap) — and the default is EMPTY, so a
/// daemon that says nothing about lanes has every lane off and every
/// existing response byte-identical.
///
/// `retention_days` overrides a lane's registry default; a lane not named
/// here keeps `LaneSpec::retention_days_default`. A `0` is not "keep
/// forever" — it falls back to the registry default, because a fact store
/// with no retention is exactly the unbounded-growth resource bug §P8's
/// retention rule exists to prevent.
///
/// ```toml
/// [lanes]
/// enabled = ["git.behavior", "coverage.simplecov", "sarif.brakeman"]
///
/// [lanes.retention_days]
/// "coverage.simplecov" = 14
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LanesSection {
    #[serde(default)]
    pub enabled: Vec<String>,
    #[serde(default)]
    pub retention_days: std::collections::BTreeMap<String, u32>,
}

impl LanesSection {
    /// `true` iff `lane_id` appears verbatim in `enabled`. Exact string
    /// match, never a prefix or glob: `sarif.*` is a registry FAMILY
    /// template, and enabling one of its instances means naming that
    /// instance (`sarif.brakeman`), so a typo is a lane that stays off
    /// rather than a wildcard that quietly opens more than was meant.
    pub fn is_enabled(&self, lane_id: &str) -> bool {
        self.enabled.iter().any(|l| l == lane_id)
    }

    /// The retention window for `lane_id`, in days — the operator's
    /// override when it is a positive value, else `default_days`.
    pub fn retention_days(&self, lane_id: &str, default_days: u32) -> u32 {
        match self.retention_days.get(lane_id) {
            Some(&d) if d > 0 => d,
            _ => default_days,
        }
    }
}

/// `[scip]` — PRR-N12 (N1)'s per-repo SCIP-indexer config, array-of-tables
/// mirroring [`SemanticSection`]'s per-repo shape (a top-level section keyed
/// by repo name, rather than extending [`RepoEntry`] itself — lower blast
/// radius, matches this file's own "one top-level section per feature"
/// idiom, see the module doc). The daemon ONLY PARSES this — it never
/// spawns a configured `command` itself (`crate::scip`'s module doc's
/// "server dependency-free" law); `kb-code scip run` (kb-code-cli, N2) is
/// the ONLY thing that ever executes a configured argv, reading it back off
/// `GET /api/repos`. A repo with no `[[scip.repos]]` entry simply reports
/// `ScipStatus::configured = false` (`routes::repos`) — this section
/// carries zero implicit defaults per repo.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ScipSection {
    #[serde(default)]
    pub repos: Vec<ScipRepoEntry>,
}

impl ScipSection {
    /// Look up the configured SCIP entry for `repo_name`, or `None` when
    /// unconfigured — the ONE predicate `routes::repos`'s `ScipStatus`
    /// computation and `kb-code scip run`'s `--all` target selection both
    /// read (the latter via the SAME field echoed back on the wire), so a
    /// repo's "is SCIP configured" answer can never drift between the two.
    pub fn for_repo(&self, repo_name: &str) -> Option<&ScipRepoEntry> {
        self.repos.iter().find(|r| r.name == repo_name)
    }
}

/// One `[[scip.repos]]` entry — a SCIP indexer invocation for one
/// configured `[[repos]]` NAME.
///
/// ```toml
/// [[scip.repos]]
/// name = "kb"
/// command = ["rust-analyzer", "scip", "."]
/// output = "index.scip"
/// langs = ["rust"]
/// ```
///
/// `command` is an argv array (`command[0]` is the executable, the rest its
/// arguments) — NEVER a shell string: `kb-code scip run` spawns it via
/// `std::process::Command::new(command[0]).args(&command[1..])`, matching
/// the injection-safety discipline this codebase already applies to git
/// refs (`reject_user_ref`/`is_full_sha` in `reviews.rs`). `output` is
/// REPO-ROOT-RELATIVE (e.g. `"index.scip"`) — `kb-code scip run` chains
/// straight into the existing `scip ingest` code path against
/// `<repo_path>/<output>` once the indexer exits successfully. `langs` is
/// informational only (surfaced on `GET /api/repos`'s `ScipStatus` for the
/// operator, and used to compute `ScipStatus::docs_total`) — it never gates
/// or validates the ingest itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScipRepoEntry {
    pub name: String,
    pub command: Vec<String>,
    pub output: String,
    #[serde(default)]
    pub langs: Vec<String>,
}

/// `[semantic]` — W2.3's semantic search lane config. `enabled = false` +
/// `repos = []` by default: a cold-fleet embed is HOURS of wall-clock work
/// (every distinct blob_hash across every enabled repo, chunked + embedded
/// through a single niced subprocess), so a repo only gets chunked+embedded
/// once it is BOTH `enabled = true` AND named in `repos` — a per-repo
/// staged rollout, not a single all-or-nothing switch. The flag gates BOTH
/// the background indexer (`semantic::indexer::SemanticIndexer` is only
/// spawned when `enabled` — see `lib.rs::bind_and_spawn`) and the search
/// route (`routes::search_semantic` 400s for a repo not in `repos`, with a
/// hint, rather than silently returning empty results).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SemanticSection {
    /// Master switch. `false` (the default) means the semantic lane never
    /// spawns its embedder subprocess at all — a fresh kb-code install pays
    /// zero extra cost until an operator opts in.
    #[serde(default)]
    pub enabled: bool,

    /// Allowlist of repo NAMEs (matching a `[[repos]] name`) opted into the
    /// semantic lane. Empty by default — `enabled = true` alone embeds
    /// nothing; a repo must ALSO be named here (the per-repo staged
    /// rollout). An entry naming a repo `[[repos]]` doesn't define is
    /// simply inert (never matched by `repo_enabled`), not an error — same
    /// "stale config entry shouldn't fail boot" posture as `[[repos]]`
    /// itself.
    #[serde(default)]
    pub repos: Vec<String>,

    /// nice value for the `jina-embeddings-v2-base-code` embedder
    /// subprocess. Mirrors kb's own `[indexer] indexer_nice`
    /// (`kb_core::config::IndexerSection`) — same default (20, lowest
    /// priority) and the same `[0, 19]` clamp. `None` → the default.
    #[serde(default)]
    pub nice: Option<i32>,
}

impl SemanticSection {
    /// Matches `kb_core::config::IndexerSection::DEFAULT_INDEXER_NICE`.
    pub const DEFAULT_NICE: i32 = 20;

    /// Resolved nice value — config wins, else the default; clamped to
    /// `[0, 19]` (negative values would mean "raise priority," which only
    /// root can do and this daemon never expects).
    pub fn resolved_nice(&self) -> i32 {
        self.nice.unwrap_or(Self::DEFAULT_NICE).clamp(0, 19)
    }

    /// Whether `repo_name` is opted into the semantic lane — BOTH the
    /// master switch AND the per-repo allowlist must agree. The single
    /// predicate both the indexer's boot wiring and the search route's
    /// 400-gate call, so the two can never drift on what "enabled" means.
    pub fn repo_enabled(&self, repo_name: &str) -> bool {
        self.enabled && self.repos.iter().any(|r| r == repo_name)
    }

    /// Every currently-enabled repo name — `lib.rs::bind_and_spawn` passes
    /// this to `SemanticIndexer::spawn` rather than re-deriving the filter
    /// there.
    pub fn enabled_repo_names(&self) -> Vec<String> {
        if !self.enabled {
            return Vec::new();
        }
        self.repos.clone()
    }
}

/// `[occurrences]` — B5a's cost knob for `crate::occurrences`'s token-level
/// pass (B2, widened to eight languages in B5b). The B2 bench
/// (`tests/measure/occurrences_bench.rs`) measured this pass's cost on kb's own
/// `crates/` tree at +142% db size / +42% walk time for zero rows dropped —
/// expensive enough that an operator indexing a much larger monorepo needs a
/// way to opt individual repos OUT without losing the pass for the rest of
/// their fleet. Mirrors [`SemanticSection`]'s shape (an `enabled` master
/// switch plus a per-repo list, `repo_enabled` deciding both) but with the
/// OPPOSITE polarity: `enabled = true` by default (occurrences is a core
/// browsing feature — file-local resolve/xrefs degrade to word-scan without
/// it, unlike semantic search which is a genuinely optional lane) and
/// `disabled_repos` is a DENYLIST (a repo is on by default; naming it here
/// opts it OUT), rather than `[semantic]`'s allowlist (a repo is off by
/// default; naming it there opts it IN). `ingest::index_file` checks
/// [`Self::repo_enabled`] before ever calling `occurrences::
/// extract_occurrences` — when off for a repo, `has_occurrences` simply
/// never becomes `true` for that repo's blobs, and `resolve.rs`
/// transparently degrades to its existing word-scan fallback (no separate
/// code path needed there).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OccurrencesSection {
    #[serde(default = "OccurrencesSection::default_enabled")]
    pub enabled: bool,

    /// Repo NAMEs (matching a `[[repos]] name`) opted OUT of the occurrences
    /// pass despite the master switch being on. Empty by default — every
    /// configured repo gets occurrences unless explicitly listed here. An
    /// entry naming a repo `[[repos]]` doesn't define is simply inert (same
    /// "stale config entry shouldn't fail boot" posture as `[[repos]]`
    /// itself and `[semantic] repos`).
    #[serde(default)]
    pub disabled_repos: Vec<String>,
}

impl OccurrencesSection {
    fn default_enabled() -> bool {
        true
    }

    /// Whether `repo_name` gets the occurrences pass — the ONE predicate
    /// both `ingest::index_file`'s callers (`lib.rs`'s boot walk,
    /// `sink.rs`'s live-mirror worker) consult, so the ingest-time gate and
    /// any future config-surfaced status can never drift on what "enabled"
    /// means (mirrors `SemanticSection::repo_enabled`'s same rationale).
    pub fn repo_enabled(&self, repo_name: &str) -> bool {
        self.enabled && !self.disabled_repos.iter().any(|r| r == repo_name)
    }
}

impl Default for OccurrencesSection {
    fn default() -> Self {
        Self {
            enabled: Self::default_enabled(),
            disabled_repos: Vec::new(),
        }
    }
}

/// `[rails_lens]` — PRR-N3's operator override for the Rails-lens detection
/// gate (`crate::frameworks::rails::detect_is_rails`, which greps a repo's
/// working tree for `config/routes.rb` + a `Gemfile` `gem "rails"`/
/// `gem 'rails'` line — see that fn's doc). Unlike `[semantic]`/
/// `[occurrences]`, the base decision here is neither "off unless listed"
/// nor "on unless listed" — it's AUTO-DETECTED per repo, and this section is
/// a pair of override lists an operator reaches for only when auto-detect is
/// wrong (a vendored Rails-shaped fixture repo that isn't really a Rails
/// app, or a Rails app that keeps `routes.rb` somewhere non-conventional).
/// `disabled_repos` is checked FIRST (an explicit "never" always wins over
/// an explicit "always"), matching the fail-safe posture of every other
/// per-repo override list in this file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RailsLensSection {
    /// Repo NAMEs (matching a `[[repos]] name`) force-ENABLED regardless of
    /// what auto-detection says. Empty by default.
    #[serde(default)]
    pub repos: Vec<String>,

    /// Repo NAMEs force-DISABLED regardless of auto-detection (checked
    /// before `repos` — see the struct doc). Empty by default.
    #[serde(default)]
    pub disabled_repos: Vec<String>,
}

impl RailsLensSection {
    /// The one predicate every caller consults: `auto_detected` is the
    /// result of `frameworks::rails::detect_is_rails(repo_root)`, resolved
    /// ONCE per repo by the caller (a working-tree scan — see that fn's
    /// doc for why this must never be re-run per file). `disabled_repos`
    /// wins over `repos`, which wins over the auto-detected default.
    pub fn repo_enabled(&self, repo_name: &str, auto_detected: bool) -> bool {
        if self.disabled_repos.iter().any(|r| r == repo_name) {
            return false;
        }
        if self.repos.iter().any(|r| r == repo_name) {
            return true;
        }
        auto_detected
    }
}

/// `[[intel.providers]]` — PRR-L2's lip/1 provider registry (`crate::lip`,
/// design-lip.md's "kb-code-server integration" + design-addendum-2.md §D).
/// Array-of-tables, mirroring [`ScipSection`]'s per-repo shape: a top-level
/// `[intel]` section (NOT the pre-existing `crate::intel` Rust module — same
/// English word, unrelated concept: that module is the resolve-ladder's
/// cross-file/arity/access scoring machinery, this section is the live-LSP
/// overlay's provider config, parsed and consumed entirely by `crate::lip`)
/// holding zero or more configured adapter endpoints.
///
/// ```toml
/// [[intel.providers]]
/// name  = "ruby"
/// url   = "http://127.0.0.1:4841"
/// langs = ["ruby"]
/// repos = ["acme-shop"]
/// ```
///
/// `[semantic]`-style allowlist semantics (mirrors
/// [`SemanticSection::repo_enabled`]): a provider only applies to a
/// `(repo, lang)` pair when BOTH its `langs` and `repos` lists name them —
/// an EMPTY `repos` opts in ZERO repos, never "every repo" (an allowlist
/// that's absent is off, not wide-open — same posture as `[semantic]`'s
/// own `repos` field).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IntelSection {
    #[serde(default)]
    pub providers: Vec<IntelProviderEntry>,
}

impl IntelSection {
    /// The FIRST configured provider whose `langs`/`repos` allowlist covers
    /// `(repo_name, lang_id)` — configured-order wins, same "config order is
    /// the tiebreak" convention `ScipSection::for_repo` and `resolve.rs`'s
    /// other-repos tier both use. `None` when no provider is configured at
    /// all, or none names both this repo AND this language.
    pub fn provider_for(&self, repo_name: &str, lang_id: &str) -> Option<&IntelProviderEntry> {
        self.providers.iter().find(|p| {
            p.repos.iter().any(|r| r == repo_name) && p.langs.iter().any(|l| l == lang_id)
        })
    }
}

/// One `[[intel.providers]]` entry — a configured lip/1 adapter endpoint.
/// `name` is this daemon's own label for the provider (need not match the
/// adapter's own `server_name`) and is what `GET /api/repos`'s `intel.
/// provider` field echoes back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntelProviderEntry {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub langs: Vec<String>,
    #[serde(default)]
    pub repos: Vec<String>,
}

/// `[scopes]` — Phase N ("Navigate") named path sets: free-form map of
/// scope name → list of glob patterns. No built-in names are special-
/// cased in code — `generated`/`tests` below are just the documented
/// defaults an operator gets when the section is absent. Used by
/// `GET /api/todos?scope=<name>` (include files matching any pattern)
/// and `?scope=!<name>` (exclude those files). Matching is implemented
/// in [`crate::scopes`] (no new crate dep — reuses + extends the same
/// lightweight glob shapes `kb_core::watcher::path_matches_skip_pattern`
/// already covers).
///
/// Example `kb-code.toml`:
///
/// ```toml
/// [scopes]
/// generated = ["**/dist/**", "**/*.lock", "**/node_modules/**"]
/// tests = ["**/tests/**", "**/*.test.*", "**/e2e/**"]
/// ```
///
/// Serde flattens this as a bare map under `[scopes]` (each key is a
/// scope name, each value a string array of globs) — there is no nested
/// table per scope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScopesSection {
    /// Free-form name → patterns map. Defaults to the documented
    /// `generated` + `tests` entries when the section is absent.
    #[serde(flatten, default = "ScopesSection::default_map")]
    pub map: std::collections::BTreeMap<String, Vec<String>>,
}

impl ScopesSection {
    fn default_map() -> std::collections::BTreeMap<String, Vec<String>> {
        let mut m = std::collections::BTreeMap::new();
        m.insert(
            "generated".to_string(),
            vec![
                "**/dist/**".to_string(),
                "**/*.lock".to_string(),
                "**/node_modules/**".to_string(),
            ],
        );
        m.insert(
            "tests".to_string(),
            vec![
                "**/tests/**".to_string(),
                "**/*.test.*".to_string(),
                "**/e2e/**".to_string(),
            ],
        );
        m
    }

    /// Look up a scope by name — `None` if unknown (route layer 404s).
    pub fn get(&self, name: &str) -> Option<&[String]> {
        self.map.get(name).map(|v| v.as_slice())
    }
}

impl Default for ScopesSection {
    fn default() -> Self {
        Self {
            map: Self::default_map(),
        }
    }
}

/// `[transcripts]` — the raw-transcripts search lane's config (W2.5, see
/// `crate::transcripts`'s module doc). Every field has a default, so an
/// absent `[transcripts]` section behaves exactly like
/// `[transcripts]\nenabled = true` with every other field defaulted — a
/// fresh kb-code install indexes the operator's own local Claude Code
/// transcripts out of the box, with no config required.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptsSection {
    #[serde(default = "TranscriptsSection::default_enabled")]
    pub enabled: bool,
    /// Raw (possibly `~`-prefixed) root path — resolve via
    /// [`Self::resolved_root`], never used verbatim as a filesystem path.
    #[serde(default = "TranscriptsSection::default_root")]
    pub root: String,
    /// Project directory NAMES (the immediate child of `root`, e.g.
    /// `"-home-user-some-project"`) to skip entirely — both the startup walk
    /// and the live watcher's per-event filter honour this
    /// (`transcripts::indexer::walk_transcripts_root`/`accept_event_path`).
    #[serde(default)]
    pub exclude_projects: Vec<String>,
    /// Index `thinking` content blocks (default `true`) — see
    /// `transcripts::parse`'s module doc for why this is a toggle rather
    /// than always-on/always-off.
    #[serde(default = "TranscriptsSection::default_index_thinking")]
    pub index_thinking: bool,
}

impl TranscriptsSection {
    fn default_enabled() -> bool {
        true
    }

    fn default_root() -> String {
        "~/.claude/projects".to_string()
    }

    fn default_index_thinking() -> bool {
        true
    }

    /// Tilde-expand [`Self::root`] (`~` / `~/...` → `$HOME/...`) WITHOUT
    /// canonicalizing or requiring it to exist yet — mirrors
    /// `resolve_repos`'s "don't fail boot over a path that isn't there
    /// yet" posture; `transcripts::indexer::TranscriptWatcher::start`
    /// tolerates a missing root the same way `mirror::arm_entries`
    /// tolerates a not-yet-existing watch path (logs a warning, keeps
    /// booting).
    pub fn resolved_root(&self) -> PathBuf {
        expand_tilde(&self.root)
    }
}

impl Default for TranscriptsSection {
    fn default() -> Self {
        Self {
            enabled: Self::default_enabled(),
            root: Self::default_root(),
            exclude_projects: Vec::new(),
            index_thinking: Self::default_index_thinking(),
        }
    }
}

/// `[kb_daemon]` — where the Search-Everywhere box's SESSIONS lane
/// (`search::sessions`) federates to. `kb` (the OTHER daemon in this
/// workspace, `crates/kb-server`) owns the session digests kb-code has no
/// copy of and never will (R1's "sessions are episodic memory, PULL-only" —
/// kb-code borrows the surface over HTTP rather than re-indexing anything).
/// On by default, pointed at kb's OWN documented default bind
/// (`kb_server::state`'s `127.0.0.1:4000`) — a fresh kb-code install next to
/// a fresh kb install federates with zero config, exactly like
/// `[transcripts]`'s zero-config default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KbDaemonSection {
    #[serde(default = "KbDaemonSection::default_enabled")]
    pub enabled: bool,
    #[serde(default = "KbDaemonSection::default_url")]
    pub url: String,
    /// Path to a file holding kb's bearer token (e.g.
    /// `~/.config/kb/token`) — the FILE path lives in config, never the
    /// secret itself (same convention as kb's own deploy tooling). Needed
    /// whenever kb's `auth_bearer` doesn't see this daemon as loopback —
    /// the docker-published prod shape: the container's peer is the bridge
    /// gateway IP, so loopback bypass never applies and a token-less
    /// federation call 401s. `None` (the default) sends no Authorization
    /// header — correct for the native side-by-side install where both
    /// daemons share the host loopback.
    #[serde(default)]
    pub token_file: Option<PathBuf>,
    /// The BROWSER-facing base URL for links kb-code's UI builds into kb's
    /// OWN SPA (e.g. session digest permalinks — `web-code/src/lib/
    /// searchLanes.ts`'s `sessionUrl`). `None` (the default) falls back to
    /// `url` above via [`Self::public_base`] — correct for the native
    /// side-by-side install, where `url` (the federation target) is
    /// already something a co-located browser can load directly. The
    /// hosted/container shape MUST set this explicitly: there, `url` is a
    /// container hostname (e.g. `http://kb:4000`) the daemon-to-daemon
    /// federation call resolves fine but a browser never can, so the link
    /// base has to be a distinct, publicly-routable URL (e.g.
    /// `https://kb.example.com`).
    #[serde(default)]
    pub public_url: Option<String>,
}

impl KbDaemonSection {
    pub const DEFAULT_URL: &'static str = "http://127.0.0.1:4000";

    fn default_enabled() -> bool {
        true
    }

    fn default_url() -> String {
        Self::DEFAULT_URL.to_string()
    }

    /// Read + trim the bearer token from `token_file`. `None` when unset,
    /// unreadable, or empty — every failure degrades to the token-less
    /// request shape (the kb daemon then answers 401 for a non-loopback
    /// caller, surfaced by each lane as its ordinary `BadStatus`
    /// unavailability, never a kb-code-side error).
    pub fn bearer_token(&self) -> Option<String> {
        let path = self.token_file.as_ref()?;
        let raw = std::fs::read_to_string(path).ok()?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    /// Resolve the browser-facing base URL for links into kb's SPA:
    /// `public_url` if the operator set one, else `url` (see
    /// `public_url`'s doc for why the fallback is correct for the native
    /// install but wrong for a hosted/container deployment).
    pub fn public_base(&self) -> &str {
        self.public_url.as_deref().unwrap_or(&self.url)
    }
}

impl Default for KbDaemonSection {
    fn default() -> Self {
        Self {
            enabled: Self::default_enabled(),
            url: Self::default_url(),
            token_file: None,
            public_url: None,
        }
    }
}

/// `[backfill]` — W3.6's join-ladder PRECOMPUTE config (`crate::join::
/// backfill::backfill_repo`): a proactive, cache-warming walk of a repo's
/// commit history through the join ladder, so `kb-code why`/`story`/
/// `join`'s first live query against an old commit doesn't pay the
/// resolution cost on the spot. `depth` governs how far back the walk
/// goes — `"all"` (the default, an explicit operator ruling: precompute the
/// whole history rather than defaulting to a narrow window that would quietly
/// leave old commits unresolved) or `"<N>days"` (e.g. `"30days"`), a lookback
/// window from "now". `on_boot` is a SEPARATE, independently-defaulted-off
/// switch (`kb-code backfill` / `POST /api/backfill` is the primary,
/// explicit path — see that verb's doc) — an operator opts into a background
/// boot-time run only once they know the shape of their own repo's history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackfillSection {
    #[serde(default = "BackfillSection::default_depth")]
    pub depth: String,
    #[serde(default)]
    pub on_boot: bool,
}

impl BackfillSection {
    pub const DEFAULT_DEPTH: &'static str = "all";

    fn default_depth() -> String {
        Self::DEFAULT_DEPTH.to_string()
    }

    /// `"all"` (case-insensitive) → `None` — the precompute walks the
    /// repo's WHOLE history (still capped defensively — see
    /// `join::backfill::MAX_WALK_COMMITS` — a runaway/enormous history can't
    /// make one walk unbounded, mirroring `provenance::report::
    /// MAX_ALLOWED_MAX_COUNT`'s precedent). `"<N>days"` (N >= 1, e.g.
    /// `"30days"`) → `Some(Duration)`, a lookback window from "now" that
    /// `join::backfill::backfill_repo` turns into a `git log
    /// --since=@<cutoff>` bound. Any other spelling (a config typo) is
    /// INVALID — rather than silently narrowing or widening the walk in a
    /// way the operator didn't ask for, this falls back to `"all"`'s `None`
    /// with a loud `tracing::warn!`, mirroring `mirror::parse_watch_mode`'s
    /// tolerant-parse-but-never-fail-boot convention.
    pub fn resolved_depth(&self) -> Option<Duration> {
        let trimmed = self.depth.trim();
        if trimmed.eq_ignore_ascii_case("all") {
            return None;
        }
        let lower = trimmed.to_ascii_lowercase();
        if let Some(n) = lower.strip_suffix("days") {
            if let Ok(days) = n.parse::<u64>() {
                if days > 0 {
                    return Some(Duration::from_secs(days * 86_400));
                }
            }
        }
        tracing::warn!(
            value = %self.depth,
            "unrecognised [backfill] depth; falling back to \"all\" (valid: \"all\" | \"<N>days\")",
        );
        None
    }
}

impl Default for BackfillSection {
    fn default() -> Self {
        Self {
            depth: Self::default_depth(),
            on_boot: false,
        }
    }
}

/// `[github]` — Phase G-server's GitHub READ overlay (`crate::github`): PR
/// listing/comments federated to the real GitHub REST API, plus the one
/// `refs/kbc/pr/<n>` ref-fetch. Every field optional/defaulted — a fresh
/// kb-code install with no `[github]` section at all still works
/// unauthenticated against the real API for a public repo (a lower rate
/// limit, same shape), same "zero-config default" posture as
/// `[transcripts]`/`[kb_daemon]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GithubSection {
    /// Path to a file holding a GitHub personal access token — same
    /// file-not-secret convention as `[kb_daemon] token_file`
    /// (`KbDaemonSection::bearer_token`'s doc: the FILE path lives in
    /// config, never the secret itself). `None` (the default) sends every
    /// request unauthenticated — fine for a public repo, just GitHub's
    /// lower unauthenticated rate limit.
    #[serde(default)]
    pub token_file: Option<PathBuf>,
    /// The GitHub REST API base URL. `DEFAULT_API_BASE` in production; the
    /// ONLY reason this is a config knob rather than a hardcoded constant
    /// in `github.rs` is so tests can point it at a local mock server
    /// (`crate::github`'s own test module, and `tests/review_routes.rs`)
    /// instead of ever touching the real network.
    #[serde(default = "GithubSection::default_api_base")]
    pub api_base: String,
}

impl GithubSection {
    pub const DEFAULT_API_BASE: &'static str = "https://api.github.com";

    fn default_api_base() -> String {
        Self::DEFAULT_API_BASE.to_string()
    }

    /// Read + trim the token from `token_file` — identical contract to
    /// `KbDaemonSection::bearer_token` (degrades to `None` on a missing,
    /// unreadable, or empty file; never an error).
    pub fn bearer_token(&self) -> Option<String> {
        let path = self.token_file.as_ref()?;
        let raw = std::fs::read_to_string(path).ok()?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }
}

impl Default for GithubSection {
    fn default() -> Self {
        Self {
            token_file: None,
            api_base: Self::default_api_base(),
        }
    }
}

/// `[review]` — V3.R1 local review sessions (patchset auto-capture + GC).
/// Explicit `POST /api/reviews/{id}/snapshot` always works regardless of
/// `patchset_capture`; that flag only gates the mirror-driven auto path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewSection {
    /// Master switch for auto-capture on `repo.head_moved` (default true).
    #[serde(default = "ReviewSection::default_patchset_capture")]
    pub patchset_capture: bool,
    /// Max patchsets retained per review. Beyond this, capture GCs the
    /// OLDEST (`ps_number` keeps counting up, never reused). Default 50.
    #[serde(default = "ReviewSection::default_max_patchsets")]
    pub max_patchsets: u32,
    /// S2-B ("Mobile mutations," `/tmp/design-s2.md`) — graduates FIVE
    /// review-mutation route families (finding disposition PUT/DELETE,
    /// verdict PUT/DELETE, finding/verdict publish-recording POST, and
    /// manual finding create POST — `router.rs`'s `review_remote`
    /// sub-router, gated by `crate::review_gate::review_mutations_gate`)
    /// off pure loopback-only so a non-loopback caller carrying a valid
    /// bearer token can reach them too. Default `false` — FAIL-CLOSED: a
    /// non-loopback caller keeps getting the SAME loopback-only-style 404
    /// until an operator opts in. The working-tree mutation lane
    /// (`checkout`, suggestion apply/apply-batch, `scip/ingest`,
    /// `prs/fetch`) and every OTHER review mutation (create/snapshot/
    /// patch/delete/viewed/gc/pr/sweep/report, `findings/import`) NEVER
    /// move — this flag has zero reach there, by construction (see
    /// `review_mutations_gate`'s own module doc).
    #[serde(default)]
    pub remote_mutations: bool,
    /// V73-K1 — the NAMED HTML templates `GET /api/reviews/{id}/doc/render`
    /// may render a `kbc-review/1` document through, `<name> = <path>`.
    ///
    /// A name, never a path, is what the route accepts, so that surface is
    /// structurally unable to be talked into reading an arbitrary file; the
    /// built-in `default` template always exists and cannot be shadowed
    /// away by config. The CLI's `review render --template <file.html>`
    /// does not consult this map at all — it POSTs the operator's own
    /// template bytes to the loopback-only twin.
    #[serde(default)]
    pub doc_templates: std::collections::BTreeMap<String, std::path::PathBuf>,
}

impl ReviewSection {
    fn default_patchset_capture() -> bool {
        true
    }
    fn default_max_patchsets() -> u32 {
        50
    }
}

impl Default for ReviewSection {
    fn default() -> Self {
        Self {
            patchset_capture: Self::default_patchset_capture(),
            max_patchsets: Self::default_max_patchsets(),
            remote_mutations: false,
            doc_templates: std::collections::BTreeMap::new(),
        }
    }
}

/// `[behavioral]` — V3.2-B1 repo-addressed history counters
/// (`crate::behavioral`). Full rebuilds walk `git log --numstat` over a
/// lookback window; incremental head-moved updates add commits between
/// `behavioral_meta.last_commit_sha` and HEAD. ATTENTION signals only —
/// never quality grades.
///
/// ```toml
/// [behavioral]
/// enabled = true
/// window_days = 90
/// max_commit_files = 30
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BehavioralSection {
    /// Master switch for the head-moved incremental worker and optional
    /// boot backfill. Explicit `POST /api/behavioral/backfill` still
    /// works when `enabled = false` (operator-driven rebuild).
    #[serde(default = "BehavioralSection::default_enabled")]
    pub enabled: bool,
    /// Lookback window for a full rebuild (`git log --since`). Default 90.
    #[serde(default = "BehavioralSection::default_window_days")]
    pub window_days: u32,
    /// ROSE transaction cap: commits touching more files than this still
    /// count toward `path_stats`/`author_stats` but are SKIPPED for
    /// `cochange_pairs` (merge/noise filter). Default 30.
    #[serde(default = "BehavioralSection::default_max_commit_files")]
    pub max_commit_files: u32,
}

impl BehavioralSection {
    fn default_enabled() -> bool {
        true
    }
    fn default_window_days() -> u32 {
        90
    }
    fn default_max_commit_files() -> u32 {
        30
    }
}

impl Default for BehavioralSection {
    fn default() -> Self {
        Self {
            enabled: Self::default_enabled(),
            window_days: Self::default_window_days(),
            max_commit_files: Self::default_max_commit_files(),
        }
    }
}

/// `[doclens]` — DCB. OFF by default: `kbs` empty means `/api/doc-lens`
/// refuses every request with `reason = "doclens_disabled"`. kb-code holds a
/// FULL-CORPUS kb token (`[kb_daemon] token_file`), so an unbounded `?kb=`
/// would turn this daemon into an unscoped read-proxy into every corpus that
/// token can reach — the allowlist is the scope, and there is no other one.
///
/// ONE struct for the whole feature (R6): the last three fields are W3.A's
/// background-sync knobs and are INERT in W1.C — defined here so the section
/// is never split across two phases and `[doclens]` never means two
/// different things in two config files.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DoclensSection {
    /// kb corpus names this daemon will pull `coderef/1` for. EMPTY = feature
    /// off. Gates BOTH the routes and W3.A's background sync — the gate lives
    /// inside `doclens::resolve::resolve_lens` (D15), so a kb dropped from
    /// this list stops the corpus's prose flowing into this daemon, not just
    /// the routes.
    #[serde(default)]
    pub kbs: Vec<String>,
    /// EXTRA browser origins allowed to read the doc-lens routes
    /// cross-origin, beyond the always-allowed loopback set
    /// (`kb_server::middleware::is_loopback_web_origin`). Exact
    /// `scheme://host[:port]` strings, no path, no wildcard, compared
    /// case-insensitively after trimming a trailing `/`. EMPTY (the default)
    /// = loopback-only, i.e. kb-server's own SW3 posture.
    #[serde(default)]
    pub origins: Vec<String>,
    /// Override for `doclens::MAX_REFS_PER_LENS`. Clamped to [1, 2000].
    #[serde(default)]
    pub max_refs: Option<usize>,
    /// Per-request wall-clock budget for the blocking resolution work; on
    /// expiry the response is returned with `partial = true`.
    ///
    /// [C1] Bounds ONLY the blocking resolution LOOP inside
    /// `resolve::resolve_refs_blocking` (N refs × file reads) — started
    /// after the `spawn_blocking` hop, per that fn's own doc comment — NOT
    /// end-to-end `GET /api/doc-lens` wall-clock (the kb fetch, the
    /// `list_files`/symbols preamble, and the `spawn_blocking` queue wait
    /// are all outside it and can't be cancelled by it). The SCORECARD route
    /// (`resolve::resolve_scorecard` / `GET /api/doc-lens/repos`) reads this
    /// section but has NO deadline of its own at all: `repo_facts`'s `git
    /// status` per repo (not the ref-resolution loop) dominates its cost,
    /// and there's no comparable unbounded loop this budget could bound.
    #[serde(default = "DoclensSection::default_deadline_ms")]
    pub deadline_ms: u64,

    // --- W3.A sync knobs: parsed here, read only by `doclens::sync` -------
    /// Periodic background-sync interval. `0` (default) disables the timer
    /// entirely — `POST /api/doc-lens/sync` and `sync_on_boot` stay the only
    /// triggers (`[semantic]`'s off-by-default posture for a feature with a
    /// real background cost). Unread in W1.C.
    #[serde(default)]
    pub sync_interval_secs: u64,
    /// Run one sync pass at boot. Unread in W1.C.
    #[serde(default)]
    pub sync_on_boot: bool,
    /// Max (kb, doc) pairs ATTEMPTED — i.e. reaching `resolve_lens`, success
    /// OR failure; see `doclens::sync::SyncStats::docs_attempted` — in ONE
    /// sync pass. The sync-storm guard; the persisted cursor makes a capped
    /// pass resume rather than starve. Read through [`Self::batch_cap`]
    /// (clamped to `>= 1`), never this raw field, so a misconfigured `0`
    /// cannot mean "attempt nothing, forever." Unread in W1.C.
    #[serde(default = "DoclensSection::default_batch_cap")]
    pub batch_cap: usize,
}

/// Hand-written, NOT derived: a derived `Default` would set `deadline_ms = 0`
/// (an instantly-expired budget) and `batch_cap = 0` (a sync that processes
/// nothing) — both silently wrong for a section a `[doclens]`-less config
/// gets by default.
impl Default for DoclensSection {
    fn default() -> Self {
        Self {
            kbs: Vec::new(),
            origins: Vec::new(),
            max_refs: None,
            deadline_ms: Self::DEFAULT_DEADLINE_MS,
            sync_interval_secs: 0,
            sync_on_boot: false,
            batch_cap: Self::default_batch_cap(),
        }
    }
}

impl DoclensSection {
    pub const DEFAULT_DEADLINE_MS: u64 = 3_000;

    fn default_deadline_ms() -> u64 {
        Self::DEFAULT_DEADLINE_MS
    }

    fn default_batch_cap() -> usize {
        200
    }

    /// `false` when `kbs` is empty — the feature is off on this daemon.
    pub fn enabled(&self) -> bool {
        !self.kbs.is_empty()
    }

    pub fn kb_allowed(&self, kb: &str) -> bool {
        self.kbs.iter().any(|k| k == kb)
    }

    pub fn max_refs(&self) -> usize {
        self.max_refs
            .unwrap_or(crate::doclens::MAX_REFS_PER_LENS)
            .clamp(1, 2_000)
    }

    /// The sync-storm budget, clamped to `>= 1` at read (mirrors
    /// [`Self::max_refs`]'s clamp-at-read convention): a raw `batch_cap = 0`
    /// from config would mean "attempt nothing, every pass, forever" rather
    /// than the loud, obviously-broken value it should be.
    pub fn batch_cap(&self) -> usize {
        self.batch_cap.max(1)
    }

    /// Normalised, validated extra origins. A malformed entry is DROPPED with
    /// a `tracing::warn!` (never a boot failure) — `mirror::parse_watch_mode`'s
    /// tolerant-parse convention. Valid = `http://` or `https://` + a
    /// non-empty host, no `/` after the authority, no `*`.
    pub fn normalized_origins(&self) -> Vec<String> {
        self.origins
            .iter()
            .filter_map(|raw| match normalize_origin(raw) {
                Some(o) => Some(o),
                None => {
                    tracing::warn!(
                        origin = %raw,
                        "[doclens] origins: dropping malformed entry (want scheme://host[:port])"
                    );
                    None
                }
            })
            .collect()
    }
}

/// Lowercase + trailing-`/`-trimmed origin, or `None` when the string is not
/// a bare `scheme://host[:port]` web origin. Shared by
/// [`DoclensSection::normalized_origins`] and `doclens::cors::origin_allowed`
/// so a configured entry and an inbound `Origin` header are normalised by the
/// SAME rule (two normalisers would drift on exactly the trailing slash).
///
/// [A3] A userinfo-bearing origin (`https://kb.example.com@evil.test`,
/// `http://user:pass@host`) is NOT specially rejected here — it normalizes
/// to (near enough) itself, same as any other syntactically-valid origin.
/// That's fine, not a gap to close: a real browser's `Origin` header is
/// NEVER userinfo-bearing (the Fetch/URL spec's origin serialization has no
/// userinfo component at all), so a configured allowlist entry that
/// includes one can never `==` an inbound header either way — the rejection
/// happens at `origin_allowed`'s exact-equality check
/// (`origin_allowed_rejects_suffix_prefix_and_wildcard_lookalikes` pins
/// exactly this string), not inside this parser. Don't add userinfo
/// stripping/rejection here thinking it closes a hole; there isn't one.
pub fn normalize_origin(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.contains('*') {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("http://")
        .or_else(|| lower.strip_prefix("https://"))?;
    if rest.is_empty() || rest.contains('/') || rest.contains('?') || rest.contains('#') {
        return None;
    }
    // Reject a bare `http://:4000` (empty host) and `http://host:` forms.
    let host = if let Some(r) = rest.strip_prefix('[') {
        let (h, port) = r.split_once(']')?;
        if h.is_empty() || !(port.is_empty() || port.starts_with(':')) {
            return None;
        }
        h
    } else {
        rest.split(':').next().unwrap_or(rest)
    };
    if host.is_empty() {
        return None;
    }
    Some(lower)
}

/// `~` or `~/rest` → `$HOME` or `$HOME/rest`. No `~user` support (kb-code
/// always runs as the operator's own account — the same scope limit
/// `KbPaths`' env-override precedent accepts for XDG resolution). A
/// `HOME`-less environment (unusual, but not impossible in a stripped-down
/// container) leaves the string untouched rather than failing outright —
/// whatever results still goes through the watcher's own
/// does-this-exist tolerance.
fn expand_tilde(s: &str) -> PathBuf {
    if s == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    } else if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(s)
}

impl KbCodeConfig {
    /// Read + parse `kb-code.toml` at `path`.
    ///
    /// - **Missing file** → `Ok(Self::default())` (empty `[server]` addr
    ///   default, zero repos) plus a `tracing::warn!` boot warning — a
    ///   fresh kb-code install shouldn't refuse to boot over an unwritten
    ///   config file.
    /// - **Malformed TOML** → `Err` carrying `toml`'s own friendly
    ///   line/column message (via `kb_core::Error::Config`, the same
    ///   `From<toml::de::Error>` conversion `KbConfig::load` uses).
    /// - **Present + valid** → each `[[repos]]` entry is canonicalized
    ///   (invariant #27's lesson: store the canonical path, not whatever
    ///   the operator typed) and verified to be a git repository via
    ///   `gix::discover`; an entry that fails either check is dropped with
    ///   a `tracing::warn!` rather than failing the whole load.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(
                    path = %path.display(),
                    "kb-code.toml not found — starting with defaults (0 repos); \
                     see docs for the [server]/[[repos]] schema",
                );
                return Ok(Self::default());
            }
            Err(e) => return Err(Error::Io(e)),
        };
        let mut cfg = Self::from_toml_str(&raw)?;
        cfg.repos = resolve_repos(cfg.repos);
        Ok(cfg)
    }

    /// Parse from a TOML string, no repo resolution (goldens use this to
    /// assert the raw parsed shape before canonicalize/verify runs).
    pub fn from_toml_str(s: &str) -> Result<Self> {
        toml::from_str(s).map_err(Into::into)
    }
}

/// Canonicalize + git-verify every configured repo, dropping (with a
/// warning) any entry whose path doesn't exist or isn't a git repository.
/// Never fails the load — a stale `[[repos]]` entry (a moved/deleted
/// checkout) shouldn't take the whole daemon down.
fn resolve_repos(repos: Vec<RepoEntry>) -> Vec<RepoEntry> {
    let mut resolved = Vec::with_capacity(repos.len());
    for repo in repos {
        let canon = match std::fs::canonicalize(&repo.path) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(
                    name = %repo.name,
                    path = %repo.path.display(),
                    error = %e,
                    "kb-code.toml repo path does not exist — skipping",
                );
                continue;
            }
        };
        // gix::discover walks up from `canon` looking for a `.git` — this
        // accepts both a repo root and a subdirectory of one (mirrors `git
        // -C <path> status`), which is the friendlier behaviour for an
        // operator who points kb-code at a nested project dir.
        if gix::discover(&canon).is_err() {
            tracing::warn!(
                name = %repo.name,
                path = %canon.display(),
                "kb-code.toml repo is not a git repository — skipping",
            );
            continue;
        }
        resolved.push(RepoEntry {
            name: repo.name,
            path: canon,
        });
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_uses_defaults() {
        let cfg = KbCodeConfig::from_toml_str("").unwrap();
        assert_eq!(cfg.server.addr, ServerSection::DEFAULT_ADDR);
        assert!(cfg.repos.is_empty());
        assert_eq!(cfg.watcher.mode, "auto");
        assert!(!cfg.semantic.enabled);
        assert!(cfg.semantic.repos.is_empty());
        assert!(cfg.transcripts.enabled);
        assert_eq!(cfg.transcripts.root, "~/.claude/projects");
        assert!(cfg.transcripts.exclude_projects.is_empty());
        assert!(cfg.transcripts.index_thinking);
        assert!(cfg.occurrences.enabled);
        assert!(cfg.occurrences.disabled_repos.is_empty());
        assert!(cfg.kb_daemon.enabled);
        assert_eq!(cfg.kb_daemon.url, KbDaemonSection::DEFAULT_URL);
        assert_eq!(cfg.backfill.depth, BackfillSection::DEFAULT_DEPTH);
        assert!(!cfg.backfill.on_boot);
        assert!(cfg.github.token_file.is_none());
        assert_eq!(cfg.github.api_base, GithubSection::DEFAULT_API_BASE);
        assert!(cfg.scip.repos.is_empty());
        assert!(!cfg.review.remote_mutations);
    }

    // --- PRR-N12 (N1): `[scip]` -------------------------------------------

    #[test]
    fn scip_section_absent_defaults_to_no_repos() {
        let cfg = KbCodeConfig::from_toml_str("").unwrap();
        assert!(cfg.scip.repos.is_empty());
        assert!(cfg.scip.for_repo("kb").is_none());
    }

    #[test]
    fn scip_section_parses_repo_entries() {
        let cfg = KbCodeConfig::from_toml_str(
            r#"
            [[scip.repos]]
            name = "kb"
            command = ["rust-analyzer", "scip", "."]
            output = "index.scip"
            langs = ["rust"]

            [[scip.repos]]
            name = "kb-code"
            command = ["scip-typescript", "index"]
            output = "index.scip"
            langs = ["typescript", "tsx"]
            "#,
        )
        .unwrap();
        assert_eq!(cfg.scip.repos.len(), 2);

        let kb = cfg.scip.for_repo("kb").expect("kb entry");
        assert_eq!(
            kb.command,
            vec![
                "rust-analyzer".to_string(),
                "scip".to_string(),
                ".".to_string()
            ]
        );
        assert_eq!(kb.output, "index.scip");
        assert_eq!(kb.langs, vec!["rust".to_string()]);

        let kb_code = cfg.scip.for_repo("kb-code").expect("kb-code entry");
        assert_eq!(
            kb_code.command,
            vec!["scip-typescript".to_string(), "index".to_string()]
        );
        assert_eq!(
            kb_code.langs,
            vec!["typescript".to_string(), "tsx".to_string()]
        );

        assert!(cfg.scip.for_repo("does-not-exist").is_none());
    }

    /// `langs` is the ONE field with a default (`[]`) — everything else
    /// (`name`/`command`/`output`) is required, mirroring [`RepoEntry`]'s
    /// own no-default fields.
    #[test]
    fn scip_section_langs_defaults_to_empty_when_omitted() {
        let cfg = KbCodeConfig::from_toml_str(
            r#"
            [[scip.repos]]
            name = "kb"
            command = ["rust-analyzer", "scip", "."]
            output = "index.scip"
            "#,
        )
        .unwrap();
        assert_eq!(
            cfg.scip.for_repo("kb").expect("kb entry").langs,
            Vec::<String>::new()
        );
    }

    #[test]
    fn scip_section_missing_command_is_malformed() {
        let err = KbCodeConfig::from_toml_str(
            r#"
            [[scip.repos]]
            name = "kb"
            output = "index.scip"
            "#,
        )
        .expect_err("missing `command` must fail to parse");
        assert!(matches!(err, Error::Config(_)), "got: {err:?}");
    }

    #[test]
    fn scip_section_missing_output_is_malformed() {
        let err = KbCodeConfig::from_toml_str(
            r#"
            [[scip.repos]]
            name = "kb"
            command = ["rust-analyzer", "scip", "."]
            "#,
        )
        .expect_err("missing `output` must fail to parse");
        assert!(matches!(err, Error::Config(_)), "got: {err:?}");
    }

    #[test]
    fn github_section_parses_and_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let token_path = tmp.path().join("gh-token");
        std::fs::write(&token_path, "ghp_sekrit\n").unwrap();
        let cfg = KbCodeConfig::from_toml_str(&format!(
            "[github]\ntoken_file = \"{}\"\napi_base = \"http://127.0.0.1:9\"\n",
            token_path.display()
        ))
        .unwrap();
        assert_eq!(cfg.github.token_file.as_deref(), Some(token_path.as_path()));
        assert_eq!(cfg.github.api_base, "http://127.0.0.1:9");
        assert_eq!(cfg.github.bearer_token().as_deref(), Some("ghp_sekrit"));

        let default = KbCodeConfig::from_toml_str("").unwrap();
        assert!(default.github.token_file.is_none());
        assert!(default.github.bearer_token().is_none());
        assert_eq!(default.github.api_base, GithubSection::DEFAULT_API_BASE);
    }

    // --- S2-B: `[review] remote_mutations` ---------------------------------

    #[test]
    fn review_section_remote_mutations_defaults_to_false() {
        let cfg = KbCodeConfig::from_toml_str("").unwrap();
        assert!(
            !cfg.review.remote_mutations,
            "fail-closed default — a non-loopback caller must keep 404ing \
             until an operator opts in"
        );
        assert!(!ReviewSection::default().remote_mutations);
    }

    #[test]
    fn review_section_remote_mutations_parses_true() {
        let cfg = KbCodeConfig::from_toml_str(
            r#"
            [review]
            remote_mutations = true
            "#,
        )
        .unwrap();
        assert!(cfg.review.remote_mutations);
        // Untouched sibling defaults — this flag is additive, no
        // cross-field coupling.
        assert!(cfg.review.patchset_capture);
        assert_eq!(cfg.review.max_patchsets, 50);
    }

    #[test]
    fn backfill_section_parses_and_defaults() {
        let cfg = KbCodeConfig::from_toml_str(
            r#"
            [backfill]
            depth = "30days"
            on_boot = true
            "#,
        )
        .unwrap();
        assert_eq!(cfg.backfill.depth, "30days");
        assert!(cfg.backfill.on_boot);
        assert_eq!(
            cfg.backfill.resolved_depth(),
            Some(Duration::from_secs(30 * 86_400))
        );

        let default = KbCodeConfig::from_toml_str("").unwrap();
        assert_eq!(default.backfill.depth, "all");
        assert!(!default.backfill.on_boot);
        assert_eq!(default.backfill.resolved_depth(), None);
    }

    #[test]
    fn backfill_depth_is_case_insensitive_and_falls_back_on_garbage() {
        let all_upper = BackfillSection {
            depth: "ALL".to_string(),
            on_boot: false,
        };
        assert_eq!(all_upper.resolved_depth(), None);

        let mixed_case_days = BackfillSection {
            depth: "7DAYS".to_string(),
            on_boot: false,
        };
        assert_eq!(
            mixed_case_days.resolved_depth(),
            Some(Duration::from_secs(7 * 86_400))
        );

        for garbage in ["", "  ", "banana", "0days", "-5days", "30dayz"] {
            let cfg = BackfillSection {
                depth: garbage.to_string(),
                on_boot: false,
            };
            assert_eq!(
                cfg.resolved_depth(),
                None,
                "{garbage:?} must fall back to \"all\" (None)"
            );
        }
    }

    #[test]
    fn kb_daemon_section_parses_and_defaults() {
        let cfg = KbCodeConfig::from_toml_str(
            r#"
            [kb_daemon]
            enabled = false
            url = "http://127.0.0.1:5999"
            "#,
        )
        .unwrap();
        assert!(!cfg.kb_daemon.enabled);
        assert_eq!(cfg.kb_daemon.url, "http://127.0.0.1:5999");

        let default = KbCodeConfig::from_toml_str("").unwrap();
        assert!(default.kb_daemon.enabled);
        assert_eq!(default.kb_daemon.url, "http://127.0.0.1:4000");
    }

    #[test]
    fn semantic_section_parses_and_gates_on_both_enabled_and_the_allowlist() {
        let cfg = KbCodeConfig::from_toml_str(
            r#"
            [semantic]
            enabled = true
            repos = ["kb"]
            nice = 10
            "#,
        )
        .unwrap();
        assert!(cfg.semantic.enabled);
        assert_eq!(cfg.semantic.repos, vec!["kb".to_string()]);
        assert_eq!(cfg.semantic.resolved_nice(), 10);
        assert!(cfg.semantic.repo_enabled("kb"));
        assert!(!cfg.semantic.repo_enabled("other"));
        assert_eq!(cfg.semantic.enabled_repo_names(), vec!["kb".to_string()]);
    }

    #[test]
    fn semantic_section_disabled_master_switch_gates_out_every_repo() {
        // enabled=false with a non-empty allowlist must still gate
        // everything out — the master switch AND the allowlist both apply.
        let cfg = KbCodeConfig::from_toml_str(
            r#"
            [semantic]
            repos = ["kb"]
            "#,
        )
        .unwrap();
        assert!(!cfg.semantic.enabled);
        assert!(!cfg.semantic.repo_enabled("kb"));
        assert!(cfg.semantic.enabled_repo_names().is_empty());
    }

    #[test]
    fn semantic_section_nice_defaults_and_clamps() {
        // The default (20, mirroring kb-core's DEFAULT_INDEXER_NICE) sits
        // ABOVE the clamp ceiling on purpose — Linux niceness tops out at
        // 19, so "20 = lowest priority" resolves to 19 after the clamp,
        // exactly like `kb_core::config::IndexerSection::
        // resolved_indexer_nice` does for the same constant.
        let cfg = KbCodeConfig::from_toml_str("").unwrap();
        assert_eq!(SemanticSection::DEFAULT_NICE, 20);
        assert_eq!(cfg.semantic.resolved_nice(), 19);

        let negative = KbCodeConfig::from_toml_str("[semantic]\nnice = -5\n").unwrap();
        assert_eq!(negative.semantic.resolved_nice(), 0);

        let too_high = KbCodeConfig::from_toml_str("[semantic]\nnice = 99\n").unwrap();
        assert_eq!(too_high.semantic.resolved_nice(), 19);
    }

    #[test]
    fn occurrences_section_defaults_on_with_an_empty_denylist() {
        let cfg = KbCodeConfig::from_toml_str("").unwrap();
        assert!(cfg.occurrences.enabled);
        assert!(cfg.occurrences.disabled_repos.is_empty());
        assert!(cfg.occurrences.repo_enabled("kb"));
        assert!(cfg.occurrences.repo_enabled("anything"));
    }

    #[test]
    fn occurrences_section_denylist_opts_a_repo_out() {
        let cfg = KbCodeConfig::from_toml_str(
            r#"
            [occurrences]
            disabled_repos = ["huge-monorepo"]
            "#,
        )
        .unwrap();
        assert!(cfg.occurrences.enabled);
        assert_eq!(
            cfg.occurrences.disabled_repos,
            vec!["huge-monorepo".to_string()]
        );
        assert!(!cfg.occurrences.repo_enabled("huge-monorepo"));
        assert!(cfg.occurrences.repo_enabled("kb"), "unlisted repos stay on");
    }

    #[test]
    fn occurrences_section_master_switch_off_gates_every_repo_regardless_of_the_denylist() {
        let cfg = KbCodeConfig::from_toml_str(
            r#"
            [occurrences]
            enabled = false
            "#,
        )
        .unwrap();
        assert!(!cfg.occurrences.enabled);
        assert!(!cfg.occurrences.repo_enabled("kb"));
        assert!(!cfg.occurrences.repo_enabled("anything"));
    }

    #[test]
    fn transcripts_section_parses_every_field() {
        let toml_str = r#"
            [transcripts]
            enabled = false
            root = "/custom/path"
            exclude_projects = ["proj-a", "proj-b"]
            index_thinking = false
        "#;
        let cfg = KbCodeConfig::from_toml_str(toml_str).unwrap();
        assert!(!cfg.transcripts.enabled);
        assert_eq!(cfg.transcripts.root, "/custom/path");
        assert_eq!(
            cfg.transcripts.exclude_projects,
            vec!["proj-a".to_string(), "proj-b".to_string()]
        );
        assert!(!cfg.transcripts.index_thinking);
    }

    #[test]
    fn resolved_root_expands_tilde_against_home() {
        // SAFETY: this test mutates its own process-local HOME env var and
        // restores it before returning; no other test in this module reads
        // HOME, so there's no cross-test race within this crate's test
        // binary.
        let prior = std::env::var_os("HOME");
        std::env::set_var("HOME", "/home/fixture-user");

        let cfg = TranscriptsSection {
            root: "~/.claude/projects".to_string(),
            ..TranscriptsSection::default()
        };
        assert_eq!(
            cfg.resolved_root(),
            PathBuf::from("/home/fixture-user/.claude/projects")
        );

        let bare_tilde = TranscriptsSection {
            root: "~".to_string(),
            ..TranscriptsSection::default()
        };
        assert_eq!(
            bare_tilde.resolved_root(),
            PathBuf::from("/home/fixture-user")
        );

        let absolute = TranscriptsSection {
            root: "/already/absolute".to_string(),
            ..TranscriptsSection::default()
        };
        assert_eq!(absolute.resolved_root(), PathBuf::from("/already/absolute"));

        match prior {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn watcher_section_parses_mode() {
        let cfg = KbCodeConfig::from_toml_str("[watcher]\nmode = \"poll\"\n").unwrap();
        assert_eq!(cfg.watcher.mode, "poll");
    }

    #[test]
    fn full_config_parses_server_and_repos() {
        let toml_str = r#"
            [server]
            addr = "127.0.0.1:5050"

            [[repos]]
            name = "kb"
            path = "/tmp/does-not-matter-for-parsing"

            [[repos]]
            name = "other"
            path = "/tmp/also-unchecked"
        "#;
        let cfg = KbCodeConfig::from_toml_str(toml_str).unwrap();
        assert_eq!(cfg.server.addr, "127.0.0.1:5050");
        assert_eq!(cfg.repos.len(), 2);
        assert_eq!(cfg.repos[0].name, "kb");
        assert_eq!(
            cfg.repos[0].path,
            PathBuf::from("/tmp/does-not-matter-for-parsing")
        );
        assert_eq!(cfg.repos[1].name, "other");
    }

    #[test]
    fn missing_file_falls_back_to_defaults_with_zero_repos() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope-kb-code.toml");
        let cfg = KbCodeConfig::load(&missing).unwrap();
        assert_eq!(cfg, KbCodeConfig::default());
        assert!(cfg.repos.is_empty());
    }

    #[test]
    fn malformed_toml_surfaces_a_friendly_config_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("kb-code.toml");
        std::fs::write(&path, "this is not [ valid toml").unwrap();
        let err = KbCodeConfig::load(&path).expect_err("malformed toml must error");
        // kb_core::Error::Config wraps toml's own Display (line/col info).
        assert!(matches!(err, Error::Config(_)), "got: {err:?}");
    }

    /// `[kb_daemon] token_file` — parsed when present, `None` by default;
    /// `bearer_token()` reads + trims the file and degrades to `None` on a
    /// missing/empty file (fail-open to the token-less request shape).
    #[test]
    fn kb_daemon_token_file_parses_and_reads() {
        let tmp = tempfile::tempdir().unwrap();
        let token_path = tmp.path().join("token");
        std::fs::write(&token_path, "sekrit-token\n").unwrap();
        let path = tmp.path().join("kb-code.toml");
        std::fs::write(
            &path,
            format!("[kb_daemon]\ntoken_file = \"{}\"\n", token_path.display()),
        )
        .unwrap();
        let cfg = KbCodeConfig::load(&path).unwrap();
        assert_eq!(
            cfg.kb_daemon.token_file.as_deref(),
            Some(token_path.as_path())
        );
        assert_eq!(
            cfg.kb_daemon.bearer_token().as_deref(),
            Some("sekrit-token")
        );

        // Default: no token_file, no token.
        let default = KbDaemonSection::default();
        assert!(default.token_file.is_none());
        assert!(default.bearer_token().is_none());

        // Missing file degrades to None, never an error.
        let gone = KbDaemonSection {
            token_file: Some(tmp.path().join("nope")),
            ..KbDaemonSection::default()
        };
        assert!(gone.bearer_token().is_none());
    }

    #[test]
    fn kb_daemon_public_url_parses_and_public_base_prefers_it_over_url() {
        let cfg = KbCodeConfig::from_toml_str(
            r#"
            [kb_daemon]
            url = "http://kb:4000"
            public_url = "https://kb.example.com"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.kb_daemon.url, "http://kb:4000");
        assert_eq!(
            cfg.kb_daemon.public_url.as_deref(),
            Some("https://kb.example.com")
        );
        assert_eq!(cfg.kb_daemon.public_base(), "https://kb.example.com");

        // Default: no public_url set, public_base falls back to url — the
        // native side-by-side install shape.
        let default = KbCodeConfig::from_toml_str("").unwrap();
        assert!(default.kb_daemon.public_url.is_none());
        assert_eq!(
            default.kb_daemon.public_base(),
            KbDaemonSection::DEFAULT_URL
        );
    }

    fn git_init(dir: &Path) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["init", "-q"])
            .status()
            .expect("git runs")
            .success();
        assert!(ok, "git init failed in {}", dir.display());
    }

    #[test]
    fn bad_repo_path_is_skipped_with_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let good = tmp.path().join("good-repo");
        std::fs::create_dir_all(&good).unwrap();
        git_init(&good);
        let path = tmp.path().join("kb-code.toml");
        std::fs::write(
            &path,
            format!(
                r#"
                [[repos]]
                name = "good"
                path = "{}"

                [[repos]]
                name = "missing"
                path = "{}"
                "#,
                good.display(),
                tmp.path().join("does-not-exist").display(),
            ),
        )
        .unwrap();
        let cfg = KbCodeConfig::load(&path).unwrap();
        assert_eq!(cfg.repos.len(), 1, "the missing-path repo must be dropped");
        assert_eq!(cfg.repos[0].name, "good");
    }

    #[test]
    fn non_git_repo_path_is_skipped_with_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let not_git = tmp.path().join("plain-dir");
        std::fs::create_dir_all(&not_git).unwrap();
        let path = tmp.path().join("kb-code.toml");
        std::fs::write(
            &path,
            format!(
                r#"
                [[repos]]
                name = "not-a-repo"
                path = "{}"
                "#,
                not_git.display(),
            ),
        )
        .unwrap();
        let cfg = KbCodeConfig::load(&path).unwrap();
        assert!(
            cfg.repos.is_empty(),
            "a non-git directory must be dropped, not silently kept"
        );
    }

    #[test]
    fn non_canonical_repo_path_is_canonicalized() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git_init(&repo);
        // Non-canonical: a `.` segment + a trailing slash the operator
        // might reasonably type by hand.
        let noncanonical = tmp.path().join(".").join("repo").join("");
        let path = tmp.path().join("kb-code.toml");
        std::fs::write(
            &path,
            format!(
                r#"
                [[repos]]
                name = "repo"
                path = "{}"
                "#,
                noncanonical.display(),
            ),
        )
        .unwrap();
        let cfg = KbCodeConfig::load(&path).unwrap();
        assert_eq!(cfg.repos.len(), 1);
        let want = std::fs::canonicalize(&repo).unwrap();
        assert_eq!(
            cfg.repos[0].path, want,
            "the stored path must be the canonical form, not the operator's literal string"
        );
        assert_ne!(
            cfg.repos[0].path.as_os_str(),
            noncanonical.as_os_str(),
            "canonicalization must actually have changed the path"
        );
    }

    /// DCB-W3.A.R fix 7 — `batch_cap = 0` from config must never be read as
    /// "attempt nothing, forever"; `batch_cap()` clamps at read, the same
    /// convention `max_refs()` already uses for its own field.
    #[test]
    fn batch_cap_clamps_to_at_least_one() {
        let zero = DoclensSection {
            batch_cap: 0,
            ..DoclensSection::default()
        };
        assert_eq!(zero.batch_cap(), 1);
        let five = DoclensSection {
            batch_cap: 5,
            ..DoclensSection::default()
        };
        assert_eq!(five.batch_cap(), 5);
    }

    // --- PRR-L2: `[[intel.providers]]` ------------------------------------

    #[test]
    fn intel_section_absent_defaults_to_no_providers() {
        let cfg = KbCodeConfig::from_toml_str("").unwrap();
        assert!(cfg.intel.providers.is_empty());
        assert!(cfg.intel.provider_for("any-repo", "ruby").is_none());
    }

    #[test]
    fn intel_section_parses_provider_entries() {
        let cfg = KbCodeConfig::from_toml_str(
            r#"
            [[intel.providers]]
            name = "ruby"
            url = "http://127.0.0.1:4841"
            langs = ["ruby"]
            repos = ["acme-shop"]

            [[intel.providers]]
            name = "ts"
            url = "http://127.0.0.1:4842"
            langs = ["typescript", "tsx"]
            repos = ["kb-code"]
            "#,
        )
        .unwrap();
        assert_eq!(cfg.intel.providers.len(), 2);

        let ruby = cfg.intel.provider_for("acme-shop", "ruby").unwrap();
        assert_eq!(ruby.name, "ruby");
        assert_eq!(ruby.url, "http://127.0.0.1:4841");
        assert_eq!(ruby.langs, vec!["ruby".to_string()]);
        assert_eq!(ruby.repos, vec!["acme-shop".to_string()]);

        let ts = cfg.intel.provider_for("kb-code", "tsx").unwrap();
        assert_eq!(ts.name, "ts");

        assert!(cfg.intel.provider_for("kb-code", "ruby").is_none());
        assert!(cfg.intel.provider_for("does-not-exist", "ruby").is_none());
    }

    /// `[semantic]`-style allowlist semantics: an EMPTY `repos` opts in
    /// ZERO repos, never "every repo" — even though `langs` matches.
    #[test]
    fn intel_provider_with_empty_repos_matches_nothing() {
        let cfg = KbCodeConfig::from_toml_str(
            r#"
            [[intel.providers]]
            name = "ruby"
            url = "http://127.0.0.1:4841"
            langs = ["ruby"]
            "#,
        )
        .unwrap();
        assert!(cfg.intel.provider_for("acme-shop", "ruby").is_none());
        assert!(cfg.intel.provider_for("any-repo", "ruby").is_none());
    }

    #[test]
    fn intel_section_missing_url_is_malformed() {
        let err = KbCodeConfig::from_toml_str(
            r#"
            [[intel.providers]]
            name = "ruby"
            langs = ["ruby"]
            repos = ["a"]
            "#,
        )
        .expect_err("missing `url` must fail to parse");
        assert!(matches!(err, Error::Config(_)), "got: {err:?}");
    }

    /// Config-order wins on an ambiguous double-match — mirrors
    /// `ScipSection::for_repo`'s own "first configured entry" convention.
    #[test]
    fn intel_provider_for_picks_the_first_configured_match() {
        let cfg = KbCodeConfig::from_toml_str(
            r#"
            [[intel.providers]]
            name = "first"
            url = "http://127.0.0.1:1"
            langs = ["ruby"]
            repos = ["a"]

            [[intel.providers]]
            name = "second"
            url = "http://127.0.0.1:2"
            langs = ["ruby"]
            repos = ["a"]
            "#,
        )
        .unwrap();
        assert_eq!(cfg.intel.provider_for("a", "ruby").unwrap().name, "first");
    }
}
