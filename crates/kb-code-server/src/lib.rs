//! `kb-code-server` — the read-oriented code-browsing daemon (option B,
//! design v4: "The read-first IDE — session-aware code reading for humans
//! and agents", `research` kb corpus). A sibling daemon to `kb-server`
//! inside this workspace, not a subsystem of it.
//!
//! **W1.1 was scaffolding only** (crate exists, compiles, carries the
//! Wave-1/2 dependencies). **W1.2 adds the real daemon boot**: `kb-code.toml`
//! config (`config` module), an axum router with `GET /api/identity` +
//! `GET /healthz` (`router`/`routes`), and `serve`/`serve_on_random_port`
//! below. Security posture is imported, not re-derived: the invariant-#3/#4
//! loopback + fail-closed-auth predicates and the `auth_bearer` middleware
//! come from `kb_server::middleware` (see the Cargo.toml dependency note) —
//! single-sourced with kb's own daemon so a future change to that logic
//! lands in both at once. **W1.3 adds the `git` module**: read-only gix
//! wrapper (repo handle, refs, tree reads, blob reads). **W1.5 adds the
//! blob-hash index core**: `store` (kb-code's own per-daemon SQLite store —
//! schema in `crates/kb-code-server/migrations/`), `lang`/`extract`/
//! `highlight` (tree-sitter symbol + highlight extraction, Rust/Python/
//! Ruby), and `ingest` (the cache-hit/re-derive decision + the git-tree-
//! walking `index_repo_working_tree`). **W1.4 adds the `mirror` module**:
//! the live-mirror watcher (watch-set, gate-machine, reconcile — see its
//! module doc for ADR-3's "distrust file events, reconcile-don't-replay"
//! design), decoupled from storage via the `mirror::MirrorSink` trait.
//!
//! **W1.6 wires it all into a browsable daemon**: `sink::IndexSink` is the
//! real `MirrorSink` (store + `ingest`, off the watcher's thread via a
//! bounded queue — see that module's doc); `bind_and_spawn` now (a) opens
//! the store and registers every configured repo, (b) spawns a background
//! `HEAD`-tree index of each repo (`sink::initial_index_one`, W1.5's own
//! walk — git-tracked files only, not a full filesystem walk; see that
//! function's call site below for why that's sufficient), (c) starts the
//! W1.4 watcher wired to the real sink (its OWN startup reconcile also
//! walks `HEAD`, redundantly but cheaply thanks to ADR-2's blob-hash
//! cache — from then on, live edits are what keep the store current), and
//! (d) builds the daemon-wide `EventBus` the sink publishes
//! `mirror.updated`/`repo.head_moved` onto (`GET /api/events`,
//! `router`/`routes`). `git`/`store`/`ingest` are also now reachable over
//! HTTP (`GET /api/{repos,tree,file,symbols}`) — `kb-code-cli` gained a
//! `--daemon` mode alongside its existing in-process `--repo <PATH>`
//! fallback. `serve_on_random_port_with_paths` is the test entrypoint
//! (takes an explicit `KbPaths` for isolation, mirroring `kb_server::lib`'s
//! own split).
//!
//! **W2.1 adds the INSTANT search lanes** (`search` module: `files`/
//! `symbols`/`text`, each independently-callable and wired to its own
//! `GET /api/search/{files,symbols,text}` route in `routes.rs`). `AppState`
//! grows two new per-boot singletons — `file_index`/`symbol_index`
//! (`search::{FileIndex, SymbolIndex}`) — constructed here alongside
//! `store`; the text lane needs no persistent state of its own (it reads
//! `store.list_files` fresh on every call). The files lane's frecency
//! signal (`store`'s new `file_opens` table, migration V0002) is bumped by
//! `routes::file` on every successful `GET /api/file` read.
//!
//! **W2.2 completes the six v1 languages**: `lang`/`extract`/`highlight`
//! gain TypeScript/TSX (a hand-written vendored `tags.scm` merge —
//! `queries/typescript-tags.scm` — the official one only tags ambient
//! signatures), JavaScript (the official bundled `tags.scm`, used as-is),
//! Bash (a small vendored `tags.scm`, error-tolerant over partial/broken
//! parses), and YAML (NOT a tags language — the new `yaml` module walks the
//! CST directly into a hierarchical dotted key-path outline, `kind = "key"`
//! rows in the same `symbols` table).
//!
//! **W2.3 adds the SEMANTIC search lane** (`semantic` module): kb-code owns
//! its OWN lance chunk-vector store (`semantic::store::ChunkStore`,
//! `<state>/kb-code/lance/` — a separate dataset from kb's own per-kb lance
//! tables; kb-core is untouched except one edit, `embed::ModelInfo::
//! max_length`), a cAST-style tree-sitter chunker (`semantic::chunk`, reusing
//! `extract::extract_symbols`'s machinery), a background embedding worker
//! (`semantic::indexer::SemanticIndexer`, event-driven off `bus.subscribe()`
//! plus a periodic fallback sweep), and `GET /api/search/semantic`
//! (`routes::search_semantic`). Off by default (`[semantic] enabled =
//! false`), per-repo staged rollout (`[semantic] repos = [...]`) — see
//! `config::SemanticSection`'s doc.
//!
//! **W2.5 adds the RAW TRANSCRIPTS lane** (`transcripts` module): a
//! PULL-ONLY full-text index over the operator's own local Claude Code
//! transcript JSONL, never fused into the code/doc search lanes above,
//! never embedded, and served only over `GET /api/search/transcripts` /
//! `GET /api/transcripts/status` — both LOOPBACK-ONLY (see `transcripts`'
//! module doc). `bind_and_spawn` starts its tail indexer + live watcher
//! (`transcripts::indexer::TranscriptWatcher`) alongside the W1.4 mirror
//! watcher, best-effort (a failure to start is logged, not fatal — unlike
//! the mirror watcher, this lane is a convenience surface, not core
//! browsing).
//!
//! **W2.6 adds the next-tier languages** (operator ruling "plan for it:
//! json, go, toml"): `lang`/`extract`/`highlight` gain Go (a vendored
//! `tags.scm` extending the official one with `const`/`var` coverage — see
//! `queries/go-tags.scm`'s header), and the new `keypath` module adds
//! TOML/JSON as two more CST-walk key-path outlines riding YAML's W2.2
//! model (`kind = "key"` rows, same `symbols` table).
//!
//! **W3.1 adds the BLAME service** (`blame` module, ADR-4): a streamed
//! `git blame --incremental` subprocess (`blame::incremental`) + a lazy
//! `(commit, path)` region cache (`blame::cache`, Gitiles' own shape,
//! `AppState::blame_cache`) + `blame.ignoreRevsFile` support + bounded
//! on-demand line timelines via `git log -L` (`blame::timeline`) — see that
//! module's doc for the full clean/dirty/caching contract. Two new routes,
//! `GET /api/blame` (`routes::blame`) and `GET /api/blame/timeline`
//! (`routes::blame_timeline`), both under the ordinary `auth_bearer`-gated
//! `/api` nest; `kb-code blame`/`kb-code timeline` are the matching CLI
//! verbs.
//!
//! **W3.2 adds the JOIN LADDER** (`join` module): deterministic commit→
//! session resolution (`join::ladder::resolve_commit`), federating to kb's
//! `GET /api/sessions/{by-commit,commit-map}` (`join::kb_client`) over the
//! SAME `[kb_daemon]` config W2.4 already introduced. `AppState` gains
//! `kb_client: Arc<join::kb_client::KbClient>` (a per-boot singleton,
//! constructed alongside `store` — `KbClient::new` is infallible, same
//! reasoning as `file_index`/`symbol_index`). `GET /api/join/commit?repo=
//! &sha=` (`routes::join_commit`) and `kb-code join` (`kb-code-cli`) both
//! call `join::ladder::resolve_commit` directly. New migration V0005
//! (`commit_sessions`) — the ladder's precompute cache.
//!
//! **W3.3 + W3.4 add the `provenance` module** (built entirely on `blame` +
//! `join::ladder`, no new persisted state): W3.3's `provenance::report` is
//! the REAL join-ladder instrument (`GET /api/provenance-report`,
//! `kb-code provenance-report`) — confidence/via/trailer-coverage-by-week
//! counts over a repo's walked commit history, superseding kb-cli's W0.6
//! probe (left in place unchanged). W3.4's `provenance::why` (`GET
//! /api/why`, `kb-code why`) answers line-grade ("which session produced
//! this line") and file-grade ("which sessions dominate this file")
//! queries, including a deliberate narrow read of the W2.5 transcripts
//! store for UNCOMMITTED lines (session ids only, never raw text — see that
//! module's doc); `provenance::story` (`GET /api/story`, `kb-code story`)
//! is a file's (or one symbol's) session timeline, owns-lines vs
//! historical/drive-by. Both new routes sit on the ordinary
//! `auth_bearer`-gated `/api` nest (`router.rs`), not the transcripts
//! lane's stricter `loopback_only`.
//!
//! **W3.5 adds the SESSION DIFF** (`sessiondiff` module): everything a
//! session changed, as one narrative review unit — kb's `session_commits`
//! capture (`join::kb_client::KbClient::session_commits`), each optionally
//! enriched with a REAL local `git show --numstat` diff
//! (`sessiondiff::git_diff`) when it resolved to a configured repo,
//! interleaved with this daemon's own local transcript index
//! (`store::Store::transcript_turns_for_session`) in the session's OWN
//! narrative order — see `sessiondiff`'s module doc for the full assembly.
//! `GET /api/session-diff?session=&repo=` (`routes::session_diff_route`) is
//! mounted on the LOOPBACK-ONLY transcripts sub-router (`router.rs`), NOT
//! the ordinary `auth_bearer` nest — its payload carries raw transcript
//! prompt text, the same "never in any non-loopback response" rule the raw-
//! transcripts lane (W2.5) already enforces. `kb-code session-diff <sid>`
//! (`kb-code-cli`) is the CLI surface.
//!
//! **W3.6 adds the JOIN BACKFILL** (`join::backfill` module, no new
//! persisted state — it warms the SAME `commit_sessions` cache W3.2's
//! ladder already owns): `join::backfill::backfill_repo` proactively runs
//! every commit `[backfill] depth` allows (`config::BackfillSection`,
//! default `"all"`) through the UNCHANGED `join::ladder::resolve_commit`,
//! bounded to `join::backfill::MAX_CONCURRENCY` concurrent daemon-bound
//! resolutions. `AppState` gains `backfill_depth: Option<Duration>`
//! (resolved once at boot, alongside hoisting `kb_client`'s construction
//! earlier so an optional `[backfill] on_boot` background run — off by
//! default — can share the same `Arc<KbClient>`). `POST /api/backfill?repo=`
//! (`routes::backfill_route`) and `kb-code backfill` (`kb-code-cli`) are the
//! explicit, primary entry points.
//!
//! **W4.1 adds the SPA** (`web-code/`, a sibling React 18 + Vite + TS
//! project to kb's own `web/` — same tooling versions, its own `justfile`
//! recipe `ci-code-spa`): `bind_and_spawn` resolves `spa::resolve_spa_dist()`
//! ONCE at boot (`KB_CODE_SPA_DIST` env override, else `web-code/dist`
//! relative to the CWD) into `AppState::spa_dist`; the router's top-level
//! `.fallback(spa::serve)` (`router.rs`) serves it — asset-or-shell, no
//! artifact-subdomain/OG-meta pieces (kb-code has neither concept) — see
//! `spa`'s own module doc for the exact kb-server precedent it mirrors and
//! deliberately trims. Two small routes round out the reader's HTTP needs:
//! `GET /api/refs?repo=` (`routes::refs`, `GitRepo::list_refs` — already a
//! public method since W1.3, just not yet wired to HTTP) for the ref
//! picker, and `GET /api/diff?repo=&path=&from=[&to=]` (`routes::
//! diff_route`, the new `diff` module) for the reader's diff view — a real
//! `git diff` subprocess (ADR-4, same precedent as `sessiondiff::git_diff`),
//! run inside `spawn_blocking` like every other subprocess-backed route.
//! Both are ordinary `auth_bearer`-gated routes (no new sensitivity class:
//! `refs` exposes nothing `tree`/`file` don't already, and `diff` exposes
//! only the same committed/working-tree content those two already serve,
//! just pre-diffed).
//!
//! **W4.6 adds code ANNOTATIONS** (`annotations` module, migration V0006):
//! durable, path-scoped line comments anchored via kb-core's
//! `review::Anchor` (the same tagged-enum + Jaro-Winkler fuzzy-resolve
//! machinery kb's own `.review/*.json` comments use — see that module's
//! doc for the full construction/resolution contract). Own sqlite table
//! (`store::AnnotationRow`/`Store::{insert,list,get,update,delete}_
//! annotation`), no kb corpus involved. `GET`/`POST /api/annotations` +
//! `PATCH`/`DELETE /api/annotations/{id}` (`routes.rs`, ordinary
//! `auth_bearer`-gated `/api` nest — same sensitivity class as the rest of
//! the browsing surface) emit `annotation.changed {repo, path}` on
//! `state.bus` after every mutation. `kb-code annotations <PATH>`/
//! `kb-code annotate <PATH>:<LINE> -m <body>` are the CLI surface.
//!
//! **W4.7 adds confirmed CHECKOUT** (`checkout` module) — the wave-4
//! operator ruling's first sanctioned working-tree mutation (V4.S1
//! suggestion apply is the second):
//! `POST /api/checkout {repo, ref}` refuses (structured 409, listing every
//! dirty path) on a dirty working tree, else shells out to `git switch`
//! (a known local branch) or `git checkout` (anything else — tag/remote-
//! branch/raw sha, which naturally detaches HEAD) exactly as an operator
//! would at a terminal. Deliberately touches NEITHER `Store` nor
//! `MirrorSink` directly — the EXISTING live-mirror watcher already treats
//! a plain `git checkout`'s HEAD move as an ordinary idle `HeadCandidate`
//! (`mirror`'s own module doc, and `tests/mirror_matrix.rs`'s
//! `checkout_produces_head_moved_and_one_reconcile_matching_diff` case
//! already pins exactly this for a raw `git checkout` subprocess), so no
//! special-casing is needed here. Mounted on the LOOPBACK-ONLY sub-router
//! (`router.rs`, the same one `search/transcripts`/`session-diff` use) —
//! stricter than every other mutation in this crate at the time, since it
//! was the only one that touched the operator's actual working tree
//! (V4.S1 later joined it on the same gate). `kb-code checkout
//! <ref>` (`kb-code-cli`) prints a dirty refusal's path list nicely.
//!
//! **W5.1 + W5.2 add the `agentview` module** — the AGENT CONTEXT VERBS,
//! built entirely on existing state (`store`, `search`, `provenance`,
//! `semantic`), no new persisted state: `agentview::map` (`GET /api/map`,
//! `kb-code map`) is a ranked, token-budgeted repo/directory outline
//! (v1-rank: frecency + symbol count + path-depth penalty — graph-rank
//! explicitly deferred, see that module's doc); `agentview::pack`
//! (`GET /api/pack`, `kb-code pack`) is a per-file context pack (map
//! outline + provenance summary + a forward-compatible, currently-empty
//! annotations section + recent story entries, THEN budget-rationed file
//! content, smallest-first) reusing `provenance::why::file_why`/
//! `provenance::story::build_story` directly (both promoted to
//! `pub(crate)` for this); `agentview::xref` (`GET /api/defs`,
//! `GET /api/xrefs`) is a symbols-table exact/fuzzy lookup and a plain
//! word-boundary text-grep respectively — explicitly TAGS-TIER, every
//! result labeled `approximate` honestly (the HTTP path is `/api/xrefs`,
//! NOT `/api/refs` — W4.1's `routes::refs`, added independently on the
//! other side of this cherry-pick, already owns that path for the git
//! ref-picker; `kb-code-cli`'s `xrefs` verb already used this name to dodge
//! the same collision at the CLI layer, see `Cmd::Xrefs`'s doc); `agentview::similar`
//! (`GET /api/similar`) is nearest semantic-lane chunks to a caller-given
//! span (embedded via the NEW `semantic::search::embed_raw` — unprefixed,
//! unlike `embed_query`'s asymmetric `QUERY_PREFIX` — since a code span is
//! content, not a query), excluding the span's own source location; 400s
//! with the same hint `routes::search_semantic` gives when the semantic
//! lane isn't enabled. `agentview::impact` (`GET /api/impact`,
//! `kb-code impact`) is a co-change neighborhood (one `git log
//! --name-only` walk, not one `git show` per commit) plus a cheap textual
//! "mentions" signal — both labeled `approximate`. All six routes sit on
//! the ordinary `auth_bearer`-gated `/api` nest, same sensitivity class as
//! every other browsing/provenance route.
//!
//! **Phase G-server** ("kb-code v2 — The Operable Reader," the
//! review-workflow endpoints) adds five things: `GET /api/merge-check`
//! (`history::merge_check`, `git merge-tree --write-tree --name-only` —
//! never touches the working tree) and `GET /api/range-diff`
//! (`history::range_diff`, a hand-rolled parser of `git range-diff
//! --no-color`'s summary-line grammar) join `history`'s existing four
//! Phase-C routes on the ordinary `auth_bearer` nest, sharing that
//! module's `Resolved`/`resolve_sha`/`merge_base` (hoisted out of
//! `compare.rs` once `merge_check` needed the identical shape) and its
//! `HistoryError`. `GET /api/repo-state` (new top-level `repo_state`
//! module) answers "what git operation is this repo mid-flight on right
//! now" by REUSING `mirror::gate::detect_op` — a richer classifier added
//! beside that gate's existing `MARKER_NAMES`/`markers_present`, never a
//! second copy of the marker list — plus its own two small subprocess
//! calls (unmerged-path listing, dirty check). `GET /api/compare` gains an
//! opt-in `&attribution=true`: each commit then also carries a
//! `join::ladder::Attribution` (the SAME cached ladder `/api/join/commit`
//! already uses), via a `CompareCommitOut` wrapper type so the flag-off
//! response stays byte-for-byte what it always was. Finally, the new
//! `github` module is a READ overlay onto the operator's own GitHub-hosted
//! origin: `GET /api/prs` / `GET /api/prs/{number}/comments` (ordinary
//! `auth_bearer`, honest `unavailable_reason` degrade on any GitHub-side
//! failure — never a 5xx for "GitHub had a bad day") and `POST
//! /api/prs/fetch` (mounted on the SAME loopback-only sub-router as
//! `checkout`/`session-diff` — the one new git ref-write this daemon
//! performs, into the dedicated `refs/kbc/pr/<n>` namespace, never
//! `refs/heads/*`). `AppState` gains `github: Arc<github::GithubClient>`,
//! constructed once in `bind_and_spawn` alongside `kb_client`.
//!
//! **B2 adds TOKEN-LEVEL foundations** ("kb-code v2 — The Operable
//! Reader"): a new `occurrences` module — a second tree-sitter extraction
//! pass over the raw CST (not `tags.scm`), producing EVERY identifier-like
//! token (def/ref/import) for the four `lang::TOKEN_LEVEL_LANG_IDS`
//! languages (Rust/TypeScript/TSX/JavaScript this phase) — migration V0007
//! adds the `occurrences` table (own `(blob_hash, salt)`-keyed cache check,
//! `store::Store::has_occurrences`, independent of `has_symbols`) plus a
//! `symbols.doc` column. `extract.rs`'s existing `symbols.signature` column
//! (previously always `None`) and the new `doc` column are now POPULATED
//! for those same four languages (`extract::build_signature`/`capture_doc`).
//! `ingest::index_file` derives occurrences alongside symbols/highlights,
//! gated on `lang::supports_token_level`. The new `resolve` module answers
//! `GET /api/resolve?repo=&path=&line=&col=` (a 1-based line + 0-based col)
//! with a ranked, honestly-labeled candidate list (`"file-local"` def
//! occurrences in the same file, then same-repo/other-repo `symbols`
//! matches, `"tags-approx"`) — same TAGS-TIER honesty convention as
//! `agentview::xref`'s `defs`/`xrefs`, never real scope/type resolution.
//!
//! **B5a + B5b + S1 close out "The Operable Reader"'s occurrences program**:
//! B5a trims a `ref`-role single-char occurrence (measure-driven — the B2
//! bench found these were +142%/+42% cost for zero resolve value, see
//! `occurrences.rs`'s own doc) and adds `[occurrences]` (`config::
//! OccurrencesSection` — ON by default, a per-repo denylist, mirroring but
//! INVERTING `[semantic]`'s polarity), threaded through `ingest::
//! index_file`/`index_repo_working_tree` and both real callers (`sink.rs`'s
//! live-mirror worker, `lib.rs`'s own boot walk here). B5b widens
//! `lang::TOKEN_LEVEL_LANG_IDS` from four languages to all eight tier-1
//! languages (+ Python/Ruby/Go/Bash) — new per-language tables in
//! `occurrences.rs`/`extract.rs`, plus Python/Go import-following in
//! `imports.rs` (Ruby/Bash have no import syntax to follow at all — an
//! honest, permanent gap, not a B4-vs-B5b asymmetry). S1 adds the OPT-IN
//! exact precision tier: migration V0010 (`occurrences.source`, `'ts'` |
//! `'scip'`), the new `scip` module (`POST /api/scip/ingest`, LOOPBACK-ONLY —
//! parsing itself lives entirely in `kb-code-cli`, keeping this daemon
//! dependency-free of the SCIP/protobuf surface), and a `resolve.rs` tier
//! ranked FIRST (`"scip-exact"`) above `"file-local"` whenever a repo has
//! ever had a `.scip` index ingested.

pub mod actions;
pub mod agentview;
pub mod annotations;
// V70-A8 — GET /api/schemas (D20). Named `api_schemas`, not `schema` — the
// existing `schema` module already owns the SSE event-schema registry
// (`GET /api/events.schema.json`), a different vocabulary this module does
// not touch.
pub mod api_schemas;
pub mod behavioral;
pub mod blame;
pub mod bookmarks;
pub mod canvas;
pub mod checkout;
// S2-C (design-s2.md § S2-C) — LSP code actions as suggestions. Alone
// rather than folded into `lip.rs`: its own request/response wire types,
// distinct from `lip::DiagnosticsOut`. See `code_actions`'s own module doc.
pub mod code_actions;
pub mod config;
pub mod diff;
pub mod doclens;
// V71-G0 — `entities/1`: the entity index (Ruby class/module definition
// sites, keyed by (repo, worktree)), its Zeitwerk reader, and the `?ent=`
// address. See that module's own doc for why the trust class is computed
// per request and never stored.
pub mod entities;
pub mod extract;
// V70-A1 — bounded, order-preserving concurrent fan-out (kb-server
// invariant #28's ethos, ported here). `doclens::resolve::resolve_scorecard`
// is the one real caller today; see this module's own doc for the contract.
pub mod fanout;
// PRR-N5 — the direct rails_edges read (`GET /api/framework/edges`); kept
// as a routes-adjacent sibling of `frameworks` (the pure extraction lane),
// not nested inside it — mirrors `resolve.rs`/`usages.rs`/`hierarchy.rs`'s
// own top-level placement next to the tables/extractors they read.
pub mod framework_edges;
pub mod frameworks;
pub mod git;
pub mod git_status;
pub mod github;
pub mod haml;
pub mod hierarchy;
pub mod highlight;
pub mod history;
// PRR-N5 — `GET /api/hover`; see that module's own doc.
pub mod hover;
pub mod impact_analysis;
pub mod import_graph;
pub mod imports;
pub mod ingest;
pub mod intel;
pub mod join;
pub mod keypath;
pub mod lang;
pub mod lenses;
/// PRR-L2 — the lip/1 provider client + ladder overlay (design-lip.md +
/// design-addendum-2.md §D). See that module's doc.
pub mod lip;
pub mod locals;
pub mod mirror;
pub mod numstat;
pub mod occurrences;
pub mod provenance;
pub mod reading_sets;
pub mod recipes;
pub mod repo_state;
pub mod resolve;
pub mod review_analytics;
pub mod review_comments;
pub mod review_distill;
pub mod review_findings;
pub mod review_gate;
pub mod review_github_export;
pub mod review_github_threads;
pub mod review_impact;
pub mod review_inbox;
pub mod review_map;
pub mod review_sweep;
pub mod review_timeline;
pub mod reviews;
pub mod router;
pub mod routes;
pub mod schema;
pub mod scip;
pub mod scopes;
pub mod search;
/// V70-A2 — local-daemon hardening: the Origin/Host allowlist, the
/// mutation header, path containment, the secret denylist, and the
/// mutations audit ledger. See that module's own doc for the threat frame
/// ("loopback is not an authorization boundary").
pub mod security;
pub mod semantic;
// V71-G0 — kbc-seq/1: the sequence PROJECTION layer over reading_sets /
// canvas_sets. A layer, not a physical merge (design §P6).
pub mod seq;
pub mod sessiondiff;
pub mod sink;
pub mod spa;
pub mod state;
pub mod store;
pub mod suggestions;
// PRR-N5 — `GET /api/resolve-symbol`; see that module's own doc.
pub mod symbol_addr;
// V72-H1 (D7) — `syntax/1`: the ONE file-type registry (grammar, extraction
// tier, injection host, extensions/stems/shebangs) plus the Parity Grid
// derived from it. `lang::detect` is a thin façade over this table.
pub mod syntax;
pub mod transcripts;
/// V71-F1 — kbc-tree/1: the projected, decorated file tree (`GET
/// /api/tree/2`) and kbc-scope/1, its path-set algebra.
pub mod tree;
// S2-A ("One Inbox," kb-code v6.0) — `GET /api/inbox`; see that module's
// own doc for the three-lane composition.
pub mod unified_inbox;
pub mod usages;
pub mod usages2;
pub mod yaml;

use anyhow::{Context, Result};
use config::KbCodeConfig;
use kb_core::paths::KbPaths;
use state::AppState;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tree_sitter::Language;

/// Crate version (from `Cargo.toml`, workspace-pinned).
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The linked language set: one `(id, tree_sitter::Language)` pair per
/// linked grammar CRATE (originally "Wave-1" scaffolding proving every
/// grammar crate compiles and its `LanguageFn` converts cleanly against
/// this workspace's pinned `tree-sitter` core; kept up to date as later
/// waves add grammars — W2.2 added `yaml`, W2.6 added `go`/`toml`/`json`).
/// The REAL parser registry `lang.rs` builds on top of this per-id, not
/// per-crate (TypeScript/TSX share one crate here but are two `lang::
/// LangInfo` ids there).
pub fn languages() -> Vec<(&'static str, Language)> {
    vec![
        ("rust", tree_sitter_rust::LANGUAGE.into()),
        (
            "typescript",
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        ),
        ("javascript", tree_sitter_javascript::LANGUAGE.into()),
        ("python", tree_sitter_python::LANGUAGE.into()),
        ("ruby", tree_sitter_ruby::LANGUAGE.into()),
        ("bash", tree_sitter_bash::LANGUAGE.into()),
        ("yaml", tree_sitter_yaml::LANGUAGE.into()),
        ("go", tree_sitter_go::LANGUAGE.into()),
        ("toml", tree_sitter_toml_ng::LANGUAGE.into()),
        ("json", tree_sitter_json::LANGUAGE.into()),
    ]
}

/// Touches grep-searcher + grep-regex: builds a real regex matcher and a
/// real searcher (not a no-op construction) so both crates are proven
/// live. The ripgrep-style code-search surface itself lands later in
/// Wave 1.
pub fn search_version() -> &'static str {
    let _matcher = grep_regex::RegexMatcher::new(r"\bkb-code\b").expect("static pattern is valid");
    let _searcher = grep_searcher::Searcher::new();
    "grep-searcher 0.1.17 / grep-regex 0.1.14"
}

/// Touches gix: worktree-aware repository discovery, run against this
/// crate's own source tree (which is always inside a git checkout — the
/// kb workspace repo, including linked worktrees). Proves the
/// object-DB-reads + worktree-discovery + blame feature set this
/// scaffold pins actually links and runs; the real git-backed browsing API
/// is the `git` module (W1.3).
pub fn git_backend_ready() -> bool {
    gix::discover(env!("CARGO_MANIFEST_DIR")).is_ok()
}

/// Touches nucleo: constructs a real fuzzy matcher with the default
/// config and runs one match, rather than a no-op construction.
pub fn fuzzy_ready() -> bool {
    let mut matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
    let mut haystack_buf = Vec::new();
    let mut needle_buf = Vec::new();
    let haystack = nucleo::Utf32Str::new("kb-code", &mut haystack_buf);
    let needle = nucleo::Utf32Str::new("kbc", &mut needle_buf);
    matcher.fuzzy_match(haystack, needle).is_some()
}

// --- W1.2 — daemon boot -----------------------------------------------

/// Run the daemon to completion — binds `config.server.addr`, serves until
/// SIGINT/SIGTERM, then returns. The binary entrypoint (`main.rs`) calls
/// this; resolves REAL XDG paths (`KbPaths::new("kb-code")`) for the
/// store, mirroring `kb_server::lib::serve`/`build_xdg_paths`.
/// `serve_on_random_port_with_paths` is the test entrypoint.
pub async fn serve(config: KbCodeConfig) -> Result<()> {
    let paths = KbPaths::new("kb-code").context("resolve kb-code XDG paths")?;
    let (_, task) = bind_and_spawn(config, paths).await?;
    task.await.context("kb-code-server task panicked")?
}

/// Same as `serve`, but binds to a kernel-assigned port and resolves REAL
/// XDG paths (`KbPaths::new("kb-code")`) — convenience wrapper mirroring
/// `kb_server::lib::serve_on_random_port`. NOT test-isolated: two calls in
/// the same process (or two processes) share the same `<state>/kb-code/
/// index.db`. Tests use `serve_on_random_port_with_paths` instead, exactly
/// as `kb_server`'s own boot tests use `serve_on_random_port_with_paths`
/// rather than the bare `serve_on_random_port` (see that crate's
/// `build_xdg_paths` doc for the rationale — parallel `cargo test` would
/// otherwise race on process-global `XDG_*`/`KB_HOME` state).
pub async fn serve_on_random_port(
    mut config: KbCodeConfig,
) -> Result<(SocketAddr, tokio::task::JoinHandle<Result<()>>)> {
    config.server.addr = "127.0.0.1:0".to_string();
    let paths = KbPaths::new("kb-code").context("resolve kb-code XDG paths")?;
    bind_and_spawn(config, paths).await
}

/// Test entrypoint: forces `config.server.addr` to `127.0.0.1:0`
/// (kernel-assigned port) and takes an explicit `KbPaths` — tests pass
/// `KbPaths::rooted_at(tmpdir, "kb-code")` for isolation (mirrors
/// `kb_server::lib::serve_on_random_port_with_paths`).
pub async fn serve_on_random_port_with_paths(
    mut config: KbCodeConfig,
    paths: KbPaths,
) -> Result<(SocketAddr, tokio::task::JoinHandle<Result<()>>)> {
    config.server.addr = "127.0.0.1:0".to_string();
    bind_and_spawn(config, paths).await
}

/// Bind `config.server.addr` exactly as given — tests exercise the
/// fail-closed startup guard directly by passing a non-loopback addr like
/// `0.0.0.0:0` (a real unspecified-address bind, same trick
/// `kb_server`'s own `daemon_refuses_public_bind_without_token` test
/// uses). Opens the W1.5 store (`<paths.state>/index.db`) and registers
/// every configured repo BEFORE the fail-closed auth check — cheap, and
/// keeps the store-open failure mode (e.g. a permissions error) reported
/// the same way regardless of bind address. Applies the invariant-#4
/// fail-closed check, builds the router, and spawns the serve task;
/// returns before any request is handled.
pub async fn bind_and_spawn(
    config: KbCodeConfig,
    paths: KbPaths,
) -> Result<(SocketAddr, tokio::task::JoinHandle<Result<()>>)> {
    // W1.5 — `KbPaths::new("kb-code")`'s own `state` field is ALREADY
    // namespaced to `<state-root>/kb-code` (see the `config` module doc),
    // so this is `<state-root>/kb-code/index.db` — one file for every repo
    // this daemon browses, distinct from any of kb's own per-kb
    // `index.db` files.
    let db_path = paths.state.join("index.db");
    let store = Arc::new(
        store::Store::open(&db_path)
            .with_context(|| format!("open kb-code store {}", db_path.display()))?,
    );
    let mut repo_ids = std::collections::HashMap::with_capacity(config.repos.len());
    for repo in &config.repos {
        let id = store
            .upsert_repo(&repo.name, &repo.path.to_string_lossy())
            .with_context(|| format!("register repo {:?} in the kb-code store", repo.name))?;
        repo_ids.insert(repo.name.clone(), id);
    }

    // DCB W2.A (amendment 11 / E7) — prune doc-lens pins whose repo is no
    // longer configured, or whose recorded `repo_root` is no longer what that
    // NAME resolves to. Here, immediately after the `upsert_repo` loop and
    // BEFORE the initial index spawn: `upsert_repo` is
    // `ON CONFLICT(name) DO UPDATE SET root`, so a re-pointed path silently
    // re-uses the repo id, and boot is the only moment a configured root can
    // change (`KbCodeConfig` has no live reload — see `AppState`'s own doc).
    // A stale pin is strictly worse than no pin: it pre-selects a tree it was
    // never chosen against, while no pin merely shows the scorecard. Never
    // fatal to boot — a pin ledger this daemon can't read is a degraded
    // preference, not a broken index.
    match doclens::pins::prune_stale_pins(&store, &config.repos) {
        Ok(0) => {}
        Ok(n) => tracing::info!(pruned = n, "doc-lens: pruned stale pins at boot"),
        Err(e) => tracing::warn!(error = %e, "doc-lens: boot pin prune failed"),
    }

    // V70-A3X — sweep symbols/highlights/occurrences rows left stranded
    // under a STALE salt (a grammar/query version bump in `lang.rs` mints a
    // new salt string that the old delete-then-insert writers never purged
    // the old one for) — see `store::Store::sweep_stale_salt_derived`'s doc
    // for the exact (blob-live AND genuinely-superseded) condition. Never
    // fatal to boot, same posture as the pin prune above.
    match store.sweep_stale_salt_derived() {
        Ok(counts) if counts.is_empty() => {}
        Ok(counts) => tracing::info!(
            symbols = counts.symbols,
            highlights = counts.highlights,
            occurrences = counts.occurrences,
            total = counts.total(),
            "kb-code: swept stale-salt derived rows at boot"
        ),
        Err(e) => tracing::warn!(error = %e, "kb-code: boot stale-salt sweep failed"),
    }

    // W1.6 (a) — initial background index: a HEAD-tree walk per repo
    // (W1.5's `ingest::index_repo_working_tree`), spawned so it never delays
    // this fn's return. Deliberately HEAD-tree only, not a full filesystem
    // walk — `index_repo_working_tree` (like `git ls-tree`) only ever sees
    // git-TRACKED paths, so an untracked-but-present file is invisible to
    // it. That's an accepted Wave-1 scope limit, not a gap the watcher's own
    // startup reconcile closes either: its `committed_delta` is the SAME
    // HEAD-tree walk (see `mirror::startup_reconcile`) — a deliberate,
    // cheap redundancy thanks to ADR-2's blob-hash cache (the second walk is
    // pure cache hits), not a second source of untracked-file coverage.
    // From the moment the watcher below is armed, LIVE `notify` events are
    // what pick up new files going forward — an untracked file already on
    // disk before boot stays unindexed until it's next touched or `git
    // add`ed.
    // PRR-N3 — Rails-lens detection: a filesystem scan (`config/routes.rb`
    // existence + a `Gemfile` grep, `frameworks::rails::detect_is_rails`)
    // resolved ONCE PER REPO here at boot, never re-run per file or per
    // live-watcher event — see that fn's doc + `ingest::index_file`'s doc
    // for why. `[rails_lens]` lets an operator override auto-detection per
    // repo (`config::RailsLensSection::repo_enabled`). Threaded through
    // BOTH the initial-index walk below and the live sink's per-repo map
    // (`sink::spawn`).
    let is_rails_by_repo: std::collections::HashMap<String, bool> = config
        .repos
        .iter()
        .map(|repo| {
            let auto_detected = frameworks::rails::detect_is_rails(&repo.path);
            let enabled = config.rails_lens.repo_enabled(&repo.name, auto_detected);
            if enabled {
                tracing::info!(repo = %repo.name, "kb-code: Rails lens enabled for this repo");
            }
            (repo.name.clone(), enabled)
        })
        .collect();

    // V71-D1b — hoisted so the boot walk below can warm THIS instance's
    // cache (not a throwaway one) — `AppState` reuses the SAME `Arc` further
    // down rather than constructing a second, cold `SymbolIndex`.
    let symbol_index = Arc::new(search::SymbolIndex::new());

    {
        let store_for_walk = store.clone();
        let repo_ids_for_walk = repo_ids.clone();
        let repos_for_walk = config.repos.clone();
        let occurrences_for_walk = config.occurrences.clone();
        let is_rails_for_walk = is_rails_by_repo.clone();
        let symbol_index_for_walk = symbol_index.clone();
        tokio::task::spawn_blocking(move || {
            for repo in &repos_for_walk {
                let Some(&repo_id) = repo_ids_for_walk.get(&repo.name) else {
                    continue;
                };
                let occurrences_enabled = occurrences_for_walk.repo_enabled(&repo.name);
                let is_rails = is_rails_for_walk.get(&repo.name).copied().unwrap_or(false);
                sink::initial_index_one(
                    &store_for_walk,
                    repo_id,
                    &repo.name,
                    &repo.path,
                    occurrences_enabled,
                    is_rails,
                );
                // V71-D1b — warm `search::symbols::SymbolIndex`'s cache for
                // this repo at the END of its own boot-walk entry, still
                // inside this SAME `spawn_blocking` task (off the request
                // path — the daemon has already bound its listener and is
                // serving by the time this runs). `store::symbols_for_repo`
                // materialising the whole current-salt symbol set (26,820
                // rows with signature/doc strings, on the client-repo
                // fixture) is what D1's bench measured as a 29.6s FIRST
                // unified-search cliff — longer than kb-code-cli's default
                // HTTP timeout, so the first `kb-code search` after a
                // restart failed with a misleading "is kb-code-server
                // running?" rather than a true "still warming". Paying that
                // cost here, once, before any real request needs it, is
                // strictly better than paying it inline on whichever
                // request happens to be first; a failure is logged and
                // never fatal to boot, same posture as `initial_index_one`
                // itself.
                if let Err(e) = symbol_index_for_walk.warm(&store_for_walk, repo_id) {
                    tracing::warn!(
                        repo = %repo.name, error = %e,
                        "kb-code: boot-time symbol cache warm failed",
                    );
                }
            }
        });
    }

    // W1.6 (d) — the daemon-wide SSE event bus (`GET /api/events`), then the
    // real store-backed sink (`sink::spawn`), then (b) the W1.4 watcher
    // wired to it. `[watcher] mode` comes from `kb-code.toml` (`crate::
    // mirror::parse_watch_mode`); a repo that fails to open is skipped by
    // `MirrorWatcher::start` itself (logged, not fatal — mirrors kb-core's
    // own tolerate-and-continue boot posture), so `bind_and_spawn` doesn't
    // duplicate that check. Unlike a per-repo open failure, a failure to
    // arm the watcher AT ALL (a `notify` debouncer init error — e.g. the
    // host's inotify watch-limit is exhausted) IS treated as a fatal boot
    // error: a code-browsing daemon that can never learn about a live edit
    // isn't meeting Wave-1's "genuinely preferable to grep" bar, so this
    // fails the same way a store-open failure does, rather than silently
    // degrading to a read-only snapshot of whatever the initial walk saw.
    // W3.2 — the join ladder's federation handle, hoisted here (rather than
    // inline in `AppState`'s construction below) so W3.6's optional
    // `[backfill] on_boot` background run (further down) can share this
    // SAME `Arc<KbClient>` instance — its cached, TTL'd commit-map snapshot
    // (`KbClient::commit_map_snapshot`) is then warmed exactly once for
    // whichever of {boot backfill, a live `join/commit`/`why`/`story`
    // request} touches it first, rather than each holding its own
    // independent (and independently cold) client. `KbClient::new` is
    // infallible (it builds its short-lived reqwest clients per-call,
    // mirroring `search::sessions::search`'s own pattern — see that fn's
    // doc).
    let kb_client = Arc::new(join::kb_client::KbClient::new(config.kb_daemon.clone()));

    // Phase G-server — the GitHub read overlay's federation handle
    // (`GET /api/prs`, `GET /api/prs/{n}/comments`), same per-boot-
    // singleton convention as `kb_client` above (a pooled `reqwest::Client`
    // built once, reused by every request rather than a fresh client per
    // call).
    let github_client = Arc::new(github::GithubClient::new(&config.github));

    // PRR-L2 — the lip/1 provider registry (`[[intel.providers]]`), built
    // once at boot from config (same per-boot-singleton, no-live-reload
    // convention as `kb_client`/`github_client` above). `LipRegistry::new`
    // is infallible — each `LipClient`'s own handshake is lazy, performed
    // on first real use, never at boot (see `lip`'s module doc).
    let lip_registry = Arc::new(lip::LipRegistry::new(config.intel.clone()));

    // W3.6 — the join ladder's PRECOMPUTE lookback, resolved ONCE here so
    // both the optional on-boot background run below and `AppState::
    // backfill_depth` (read by `routes::backfill_route`) agree on the same
    // value for this boot's lifetime (no live-reload — see `AppState`'s own
    // doc).
    let backfill_depth = config.backfill.resolved_depth();

    // W3.6 — the OPTIONAL background backfill (`[backfill] on_boot`, off by
    // default — `kb-code backfill` / `POST /api/backfill` is the primary,
    // explicit path; see `config::BackfillSection`'s doc). Spawned
    // fire-and-forget: it never delays this fn's return, and a per-repo
    // failure is logged, never fatal to daemon boot — same posture as the
    // W1.6(a) initial-index spawn above.
    if config.backfill.on_boot {
        let store_bg = store.clone();
        let kb_client_bg = kb_client.clone();
        let repos_bg = config.repos.clone();
        let repo_ids_bg = repo_ids.clone();
        tokio::spawn(async move {
            for repo in &repos_bg {
                let Some(&repo_id) = repo_ids_bg.get(&repo.name) else {
                    continue;
                };
                match join::backfill::backfill_repo(
                    repo,
                    repo_id,
                    backfill_depth,
                    &store_bg,
                    &kb_client_bg,
                )
                .await
                {
                    Ok(stats) => tracing::info!(
                        repo = %repo.name,
                        total = stats.total,
                        newly_cached = stats.newly_cached,
                        upgraded = stats.upgraded,
                        degraded = stats.degraded,
                        duration_ms = stats.duration_ms,
                        "kb-code: on-boot backfill complete",
                    ),
                    Err(e) => tracing::warn!(
                        repo = %repo.name, error = %e,
                        "kb-code: on-boot backfill failed",
                    ),
                }
            }
        });
    }

    let bus = Arc::new(kb_core::events::EventBus::from_env());
    let (index_sink, _sink_worker) = sink::spawn(
        store.clone(),
        repo_ids.clone(),
        bus.clone(),
        config.occurrences.clone(),
        is_rails_by_repo,
    );
    let watch_mode = mirror::parse_watch_mode(&config.watcher.mode);
    let watch_mode_label: &'static str = if watch_mode == mirror::WatchMode::Poll {
        "polling"
    } else {
        "watching"
    };
    let mirror_config = mirror::MirrorConfig::new(
        config
            .repos
            .iter()
            .map(|r| mirror::RepoWatchConfig {
                name: r.name.clone(),
                root: r.path.clone(),
            })
            .collect(),
    )
    .with_mode(watch_mode);
    let watcher = mirror::MirrorWatcher::start(mirror_config, Arc::new(index_sink))
        .context("start kb-code live-mirror watcher")?;

    // W2.5 — the raw-transcripts lane's own tail indexer + live watcher,
    // over a COMPLETELY separate directory tree (`[transcripts] root`, not
    // any configured repo) and its own dedicated `notify` debouncer
    // instance (never shared with the mirror watcher above). Best-effort:
    // unlike the mirror watcher, a failure here does not fail the daemon
    // boot — see `transcripts`' module doc.
    let transcripts_root = config.transcripts.resolved_root();
    let transcripts_watcher = if config.transcripts.enabled {
        match transcripts::indexer::TranscriptWatcher::start(
            store.clone(),
            transcripts_root.clone(),
            config.transcripts.exclude_projects.clone(),
            config.transcripts.index_thinking,
        ) {
            Ok(w) => Some(Arc::new(w)),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "kb-code: transcripts watcher failed to start — \
                     GET /api/search/transcripts will report an empty index until a restart",
                );
                None
            }
        }
    } else {
        tracing::info!(
            "kb-code: [transcripts] enabled = false — the raw-transcripts search lane is idle"
        );
        None
    };

    let listener = TcpListener::bind(config.server.addr.as_str())
        .await
        .with_context(|| format!("bind {}", config.server.addr))?;
    let local_addr = listener.local_addr()?;

    // invariant #21 — best-effort IPv6 loopback companion, same rule as
    // `kb_server::serve_with_paths` (bound up-front, loopback-only, never a
    // `[::]` dual-stack bind). Not importable as a helper — kb-server
    // inlines this in `serve_with_paths` rather than exposing a free fn —
    // so this is a small local fn mirroring the same conditions/logging
    // shape rather than a copy of kb-server's implementation.
    let listener6 = bind_ipv6_loopback_companion(local_addr).await;

    // invariants #3/#4 — fail closed on a token-less non-loopback bind.
    // Imports kb-server's own predicates rather than re-deriving them: see
    // the doc comments on `refuse_public_bind_without_auth`/`allow_no_auth`
    // in crates/kb-server/src/middleware.rs.
    let token = std::env::var("KB_CODE_TOKEN")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if kb_server::middleware::refuse_public_bind_without_auth(
        local_addr.ip().is_loopback(),
        token.is_some(),
        kb_server::middleware::allow_no_auth(),
    ) {
        anyhow::bail!(
            "refusing to serve on non-loopback address {local_addr} without a bearer token — \
             a token-less public bind would expose /api/identity (and everything kb-code adds \
             later) unauthenticated. Set KB_CODE_TOKEN, or KB_ALLOW_NO_AUTH=1 to acknowledge \
             that an upstream proxy provides authentication (mirrors kb-server's fail-closed \
             rule, invariant #4)."
        );
    }
    if token.is_some() {
        tracing::info!("KB_CODE_TOKEN set — bearer auth enforced on non-loopback requests");
    } else if !local_addr.ip().is_loopback() {
        tracing::warn!(
            addr = %local_addr,
            "serving on a non-loopback address via KB_ALLOW_NO_AUTH=1 with no token — \
             every request is unauthenticated",
        );
    }
    let auth = Arc::new(kb_server::state::AuthConfig {
        token,
        trusted_proxies: Arc::new(Vec::new()),
        // kb-users/1 is OUT OF SCOPE for kb-code (recorded, v0.34): no
        // per-user token registry, no identity-header consumption — every
        // kbc request attributes as the operator via the legacy/loopback
        // rungs. The shared auth_bearer still inserts an Identity
        // extension; no kbc route reads it.
        tokens: Vec::new(),
        operator: "operator".to_string(),
        identity_header: "remote-user".to_string(),
    });

    // W2.3 — the semantic search lane: off by default
    // (`config.semantic.enabled == false`), a per-repo staged rollout when
    // on (see `config::SemanticSection`'s doc). A configured-but-failing
    // embedder spawn is treated as a FATAL boot error here, mirroring
    // kb-server's own precedent for its PRIMARY embedder (`lib.rs`'s
    // `Embedder::spawn_ipc(...).with_context(...)?` — only the OPT-IN
    // reranker degrades gracefully there); an operator who explicitly
    // turned this on gets a loud failure, not a silently-disabled feature.
    // Captured/cloned before `config.repos` moves into `AppState` below.
    let semantic_config = config.semantic.clone();
    // V71-D1 — resolved once at boot (there is no live config reload; see
    // `AppState`'s own doc), so every lane of every request agrees about
    // which ranking factors are on.
    let search_factors_config = config.search.factors();
    let (semantic_chunk_store, semantic_embedder, semantic_indexer) = if semantic_config.enabled {
        let lance_dir = paths.state.join("lance");
        let chunk_store = Arc::new(semantic::ChunkStore::open(&lance_dir).await.with_context(
            || format!("open kb-code semantic chunk store {}", lance_dir.display()),
        )?);
        let cache_dir = kb_core::embed::models_cache_dir(&paths.cache);
        let nice = semantic_config.resolved_nice();
        let model_name = semantic::MODEL_NAME;
        // `Embedder::spawn_ipc` is genuinely blocking (subprocess spawn +
        // handshake read) — run it off the async runtime, same discipline
        // `embed_ipc`'s own doc calls for.
        let embedder = tokio::task::spawn_blocking(move || {
            kb_core::embed::Embedder::spawn_ipc(model_name, cache_dir, nice)
        })
        .await
        .context("kb-code semantic embedder spawn task panicked")?
        .context("spawn kb-code semantic embedder subprocess")?;
        let embedder = Arc::new(std::sync::Mutex::new(embedder));
        let enabled_repo_names = semantic_config.enabled_repo_names();
        let indexer = semantic::SemanticIndexer::spawn(
            store.clone(),
            chunk_store.clone(),
            embedder.clone(),
            config.repos.clone(),
            repo_ids.clone(),
            enabled_repo_names,
            bus.clone(),
        );
        (Some(chunk_store), Some(embedder), Some(Arc::new(indexer)))
    } else {
        (None, None, None)
    };

    let started_at = chrono::Utc::now();
    // Phase N — scopes are read-only after boot; clone before `config.repos`
    // moves into AppState.
    let scopes = config.scopes.clone();
    // PRR-N12 (N1) — the SCIP config is read-only after boot too (no
    // live-reload — see `AppState`'s own doc); clone before `config.repos`
    // moves into AppState below.
    let scip_cfg = config.scip.clone();
    // V3.R1 — review config + auto-capture worker (subscribes to
    // `repo.head_moved` on the bus; capture work is spawn_blocking and
    // deliberately OUT of the mirror/sink hot loop).
    let review_cfg = config.review.clone();
    let _auto_capture = reviews::spawn_auto_capture_worker(
        store.clone(),
        bus.clone(),
        config.repos.clone(),
        review_cfg.max_patchsets,
        review_cfg.patchset_capture,
    );
    // V3.2-B1 — behavioral incremental worker (subscribes to
    // `repo.head_moved`; spawn_blocking off the mirror hot loop).
    let doclens_cfg = config.doclens.clone();
    let behavioral_cfg = config.behavioral.clone();
    let _behavioral_worker = behavioral::spawn_behavioral_worker(
        store.clone(),
        bus.clone(),
        config.repos.clone(),
        repo_ids.clone(),
        behavioral_cfg.clone(),
    );

    // --- V70-A2 — local-daemon hardening state (`crate::security`) ------
    //
    // Built here, once, from config: the Origin/Host allowlist the two
    // guard middlewares read, the secret denylist every content-returning
    // read consults, the git fan-out semaphore, and the scratch-ODB root.
    let host_policy = security::origin::HostPolicy::from_config(&config);
    host_policy.warn_if_unconfigured(local_addr.ip().is_loopback());
    // The FLOOR (`security::secrets::builtin_policy`) is enforced inside
    // every read site with no wiring; this carries the operator's ADDITIONS
    // for the two content-returning routes that hold state — see
    // `security::secrets`' "two-level enforcement split".
    let secret_policy = security::secrets::SecretPolicy::new(&config.security.secret_globs);
    let git_fanout = Arc::new(tokio::sync::Semaphore::new(
        config.server.resolved_git_fanout(),
    ));
    let scratch_root = paths.state.join(history::scratch::SCRATCH_DIR_NAME);
    if let Err(e) = std::fs::create_dir_all(&scratch_root) {
        // Not fatal: merge-check is the only consumer and it reports its
        // own error; a daemon that refuses to boot because one derived
        // directory could not be made would be worse.
        tracing::warn!(
            dir = %scratch_root.display(), error = %e,
            "kb-code: could not create the git scratch root"
        );
    }
    // SEC-15 — collect scratch dirs a previous process was killed holding.
    // Unconditional: THIS process has created none yet.
    match history::scratch::sweep_orphans(&scratch_root) {
        0 => {}
        n => tracing::info!(swept = n, "kb-code: removed orphaned git scratch dirs"),
    }

    let state = Arc::new(AppState {
        version: version(),
        started_at,
        repos: config.repos,
        store,
        repo_ids,
        bus,
        watch_mode: watch_mode_label,
        watcher: Arc::new(watcher),
        // W2.1 — per-boot singletons for the files/symbols search lanes'
        // in-memory caches (see `search`'s module doc). `file_index` starts
        // empty; its first `search`/`recent` call lazily populates it from
        // `state.store`. `symbol_index` is the SAME `Arc` the boot walk
        // above is warming in the background (V71-D1b) — constructing a
        // second instance here would leave that warm work orphaned in a
        // cache nothing ever reads from again.
        file_index: Arc::new(search::FileIndex::new()),
        symbol_index: symbol_index.clone(),
        search_factors: search_factors_config,
        status_index: Arc::new(git_status::StatusIndex::new()),
        semantic: semantic_config,
        semantic_chunk_store,
        semantic_embedder,
        semantic_indexer,
        // W2.5 — the raw-transcripts lane's resolved root (always set,
        // even when the watcher failed/is disabled — the search route's
        // snippet re-read and `transcripts_status`'s reported `root` both
        // need it regardless) and the watcher's RAII handle (`None` when
        // disabled or failed to start).
        transcripts_root,
        transcripts_watcher,
        // W2.4 — the unified box's sessions-lane federation target and (a
        // clone of) the SAME `auth` the router layers `auth_bearer` with,
        // so `routes::search_unified` can re-derive loopback-ness itself
        // for its transcripts section (see `state.rs`'s doc on this field).
        kb_daemon: config.kb_daemon.clone(),
        auth: auth.clone(),
        // W3.1 — the blame service's region cache; see `blame::cache`'s
        // module doc for the capacity/eviction policy.
        blame_cache: Arc::new(blame::BlameCache::default()),
        // W3.2 — the join ladder's federation handle (`GET /api/sessions/
        // {by-commit,commit-map}`), same `[kb_daemon]` target as the
        // sessions search lane above — hoisted earlier (see the comment at
        // its construction site) so W3.6's on-boot backfill can share this
        // exact `Arc` instance.
        kb_client,
        // W3.6 — the join ladder's precompute lookback, resolved once
        // above.
        backfill_depth,
        // W4.1 — resolved once at boot; see `spa::resolve_spa_dist`'s doc.
        spa_dist: spa::resolve_spa_dist(),
        // Phase G-server — the GitHub read overlay's federation handle.
        github: github_client,
        // Phase N — named path-set globs (`[scopes]`).
        scopes,
        // V3.R1 — local review sessions (`[review]`).
        review: review_cfg,
        // V3.2-B1 — behavioral store (`[behavioral]`).
        behavioral: behavioral_cfg,
        // DCB W1.C — the doc-lens config (`[doclens]`); empty `kbs` means
        // the whole feature is off (`doclens::resolve::resolve_lens` refuses
        // with `reason = "doclens_disabled"`).
        doclens: doclens_cfg,
        // DCB-W3.A.R fix 5 — the sync re-entrancy guard; see `state.rs`'s
        // doc on this field.
        doclens_sync_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        // PRR-N12 (N1) — the SCIP per-repo config (`[scip]`); see
        // `state.rs`'s doc on this field.
        scip: scip_cfg,
        // PRR-L2 — the lip/1 provider registry, constructed above.
        lip: lip_registry,
        // V70-A2 — the local-daemon hardening state (see `crate::security`).
        host_policy,
        secret_policy,
        git_fanout,
        scratch_root,
    });

    // DCB W3.A — the doc_refs reverse-index sync. Spawned UNCONDITIONALLY,
    // deciding internally whether to idle out (`sync_interval_secs == 0`),
    // the same shape as `spawn_auto_capture_worker`'s own
    // `patchset_capture` check above — a config branch at the call site
    // would make the two sibling workers read differently for no gain.
    let _doclens_sync = doclens::sync::spawn_doclens_sync_worker(state.clone());
    // `[doclens] sync_on_boot` is a SEPARATE, one-shot run, mirroring
    // `[backfill] on_boot`'s shape exactly: fire-and-forget, never delays
    // boot, and a per-kb failure inside `run_doclens_sync` is already
    // non-fatal by construction.
    if state.doclens.sync_on_boot {
        let state_bg = state.clone();
        tokio::spawn(async move {
            // `run_doclens_sync` logs its own pass summary.
            let _ = doclens::sync::run_doclens_sync(&state_bg, false).await;
        });
    }

    let app = router::build_router(state, auth);

    tracing::info!(addr = %local_addr, "kb-code-server listening");

    // invariant #3 — connect-info MUST be wired via
    // `into_make_service_with_connect_info`, not a bare `into_make_service`;
    // otherwise `auth_bearer`'s `ConnectInfo` extraction sees `None` and
    // fails closed for every request, including loopback ones.
    let server = axum::serve(
        listener,
        app.clone()
            .into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal());

    let server6 = listener6.map(|l6| {
        axum::serve(
            l6,
            app.clone()
                .into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown_signal())
    });

    let task = tokio::spawn(async move {
        match server6 {
            Some(s6) => {
                tokio::try_join!(
                    async { server.await.context("axum::serve (IPv4 loopback)") },
                    async { s6.await.context("axum::serve (IPv6 loopback)") },
                )?;
                Ok(())
            }
            None => server.await.context("axum::serve"),
        }
    });

    Ok((local_addr, task))
}

/// TEST-ONLY: builds a full [`state::SharedState`] the way [`bind_and_spawn`]
/// does, minus the TCP bind + invariants #3/#4 fail-closed dance (which
/// needs a real `local_addr` this fn never acquires — `auth` is a bare
/// [`Default`] instead, since none of the `pub(crate)` fns this fixture
/// exists to call directly touch auth). DCB-W1.C.R's G1 fix wants an in-crate
/// test that calls a resolution entry point (`doclens::resolve::
/// resolve_lens`) directly, bypassing the HTTP handler in front of it, to
/// pin that a gate lives INSIDE the fn rather than only in the wire layer —
/// that requires a real, fully-wired `SharedState`, not a mock. `[semantic]
/// enabled = true` is refused: spawning the ONNX embedder subprocess is out
/// of scope for a lightweight fixture.
#[cfg(test)]
pub(crate) async fn build_state_for_test(
    config: KbCodeConfig,
    paths: KbPaths,
) -> Result<state::SharedState> {
    anyhow::ensure!(
        !config.semantic.enabled,
        "build_state_for_test does not support [semantic] enabled = true"
    );
    // V70-A2 — `config` is partially moved into `AppState` below; the
    // hardening state is derived from a clone taken up front so the
    // fixture matches `bind_and_spawn`'s wiring exactly.
    let config_for_security = config.clone();

    let db_path = paths.state.join("index.db");
    let store = Arc::new(
        store::Store::open(&db_path)
            .with_context(|| format!("open kb-code store {}", db_path.display()))?,
    );
    let mut repo_ids = std::collections::HashMap::with_capacity(config.repos.len());
    for repo in &config.repos {
        let id = store
            .upsert_repo(&repo.name, &repo.path.to_string_lossy())
            .with_context(|| format!("register repo {:?} in the kb-code store", repo.name))?;
        repo_ids.insert(repo.name.clone(), id);
    }
    // Same boot-time pin prune `bind_and_spawn` runs (DCB-W2.A), kept in
    // lock-step so an in-crate test sees the daemon's real pin posture.
    if let Err(e) = doclens::pins::prune_stale_pins(&store, &config.repos) {
        tracing::warn!(error = %e, "doc-lens: boot pin prune failed");
    }
    // Same boot-time stale-salt sweep `bind_and_spawn` runs (V70-A3X).
    if let Err(e) = store.sweep_stale_salt_derived() {
        tracing::warn!(error = %e, "kb-code: boot stale-salt sweep failed");
    }

    let kb_client = Arc::new(join::kb_client::KbClient::new(config.kb_daemon.clone()));
    let github_client = Arc::new(github::GithubClient::new(&config.github));
    // PRR-L2 — same per-boot construction as `bind_and_spawn`'s own.
    let lip_registry = Arc::new(lip::LipRegistry::new(config.intel.clone()));
    let backfill_depth = config.backfill.resolved_depth();

    // PRR-N3 — same per-repo Rails-lens resolution `bind_and_spawn` does
    // (see that fn's own comment for the rationale).
    let is_rails_by_repo: std::collections::HashMap<String, bool> = config
        .repos
        .iter()
        .map(|repo| {
            let auto_detected = frameworks::rails::detect_is_rails(&repo.path);
            let enabled = config.rails_lens.repo_enabled(&repo.name, auto_detected);
            (repo.name.clone(), enabled)
        })
        .collect();

    let bus = Arc::new(kb_core::events::EventBus::from_env());
    let (index_sink, _sink_worker) = sink::spawn(
        store.clone(),
        repo_ids.clone(),
        bus.clone(),
        config.occurrences.clone(),
        is_rails_by_repo,
    );
    let watch_mode = mirror::parse_watch_mode(&config.watcher.mode);
    let watch_mode_label: &'static str = if watch_mode == mirror::WatchMode::Poll {
        "polling"
    } else {
        "watching"
    };
    let mirror_config = mirror::MirrorConfig::new(
        config
            .repos
            .iter()
            .map(|r| mirror::RepoWatchConfig {
                name: r.name.clone(),
                root: r.path.clone(),
            })
            .collect(),
    )
    .with_mode(watch_mode);
    let watcher = mirror::MirrorWatcher::start(mirror_config, Arc::new(index_sink))
        .context("start kb-code live-mirror watcher")?;

    let transcripts_root = config.transcripts.resolved_root();
    let transcripts_watcher = if config.transcripts.enabled {
        transcripts::indexer::TranscriptWatcher::start(
            store.clone(),
            transcripts_root.clone(),
            config.transcripts.exclude_projects.clone(),
            config.transcripts.index_thinking,
        )
        .ok()
        .map(Arc::new)
    } else {
        None
    };

    let auth = Arc::new(kb_server::state::AuthConfig::default());

    let started_at = chrono::Utc::now();
    let scopes = config.scopes.clone();
    let scip_cfg = config.scip.clone();
    let review_cfg = config.review.clone();
    let _auto_capture = reviews::spawn_auto_capture_worker(
        store.clone(),
        bus.clone(),
        config.repos.clone(),
        review_cfg.max_patchsets,
        review_cfg.patchset_capture,
    );
    let doclens_cfg = config.doclens.clone();
    let behavioral_cfg = config.behavioral.clone();
    let _behavioral_worker = behavioral::spawn_behavioral_worker(
        store.clone(),
        bus.clone(),
        config.repos.clone(),
        repo_ids.clone(),
        behavioral_cfg.clone(),
    );
    Ok(Arc::new(AppState {
        version: version(),
        started_at,
        repos: config.repos,
        store,
        repo_ids,
        bus,
        watch_mode: watch_mode_label,
        watcher: Arc::new(watcher),
        file_index: Arc::new(search::FileIndex::new()),
        symbol_index: Arc::new(search::SymbolIndex::new()),
        search_factors: config.search.factors(),
        status_index: Arc::new(git_status::StatusIndex::new()),
        semantic: config.semantic,
        semantic_chunk_store: None,
        semantic_embedder: None,
        semantic_indexer: None,
        transcripts_root,
        transcripts_watcher,
        kb_daemon: config.kb_daemon,
        auth,
        blame_cache: Arc::new(blame::BlameCache::default()),
        kb_client,
        backfill_depth,
        spa_dist: None,
        github: github_client,
        scopes,
        review: review_cfg,
        behavioral: behavioral_cfg,
        doclens: doclens_cfg,
        doclens_sync_running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        scip: scip_cfg,
        lip: lip_registry,
        // V70-A2 — the same hardening state `bind_and_spawn` builds. The
        // fixture uses a real scratch root under the test's own `paths` so
        // a fn that runs merge-check through this state never writes into
        // a browsed repo either.
        host_policy: security::origin::HostPolicy::from_config(&config_for_security),
        secret_policy: security::secrets::SecretPolicy::new(
            &config_for_security.security.secret_globs,
        ),
        git_fanout: Arc::new(tokio::sync::Semaphore::new(
            config_for_security.server.resolved_git_fanout(),
        )),
        scratch_root: paths.state.join(history::scratch::SCRATCH_DIR_NAME),
    }))
}

/// invariant #21 — mirrors `kb_server::serve_with_paths`'s IPv6 loopback
/// companion bind. Best-effort: an IPv6-disabled host (or a transient
/// rebind clash) just keeps the IPv4 listener. Loopback-only — never a
/// `[::]` dual-stack bind — and skipped when the primary bind is already
/// IPv6 or non-loopback.
async fn bind_ipv6_loopback_companion(local_addr: SocketAddr) -> Option<TcpListener> {
    if !(local_addr.is_ipv4() && local_addr.ip().is_loopback()) {
        return None;
    }
    let addr6 = format!("[::1]:{}", local_addr.port());
    match TcpListener::bind(addr6.as_str()).await {
        Ok(l) => {
            tracing::info!(addr = %addr6, "bound IPv6 loopback companion listener");
            Some(l)
        }
        Err(e) => {
            tracing::warn!(
                addr = %addr6, error = %e,
                "could not bind IPv6 loopback companion; serving IPv4 loopback only"
            );
            None
        }
    }
}

/// Resolves on SIGINT or SIGTERM (Ctrl-C only on non-unix). axum's
/// `with_graceful_shutdown` awaits this future. kb-code has no CE-style
/// restart loop yet (unlike kb-server), so this is simpler than
/// `kb_server::lib`'s own `shutdown_signal` — no watch channel to flip, no
/// SSE streams to close, no drain-deadline backstop (nothing kb-code
/// serves today can hold a connection open indefinitely).
///
/// `pub(crate)` — DCB W3.A's `doclens::sync` worker selects on this beside
/// its own ticker so a periodic background pass stops with the daemon rather
/// than only when the runtime is torn down under it.
pub(crate) async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    tracing::info!("shutdown signal received; draining");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_non_empty() {
        assert!(!version().is_empty());
    }

    #[test]
    fn languages_lists_every_linked_grammar() {
        let langs = languages();
        assert_eq!(
            langs.len(),
            10,
            "expected 10 linked grammars, got {langs:?}"
        );
        let ids: Vec<_> = langs.iter().map(|(id, _)| *id).collect();
        for expected in [
            "rust",
            "typescript",
            "javascript",
            "python",
            "ruby",
            "bash",
            "yaml",
            "go",
            "toml",
            "json",
        ] {
            assert!(ids.contains(&expected), "missing language: {expected}");
        }
    }

    #[test]
    fn search_version_is_non_empty() {
        assert!(!search_version().is_empty());
    }

    #[test]
    fn git_backend_probe_does_not_panic() {
        // This crate's source tree is always inside a git checkout (the
        // kb workspace repo, or a linked worktree of it) in every
        // environment this test runs — CI checkout, local clone, or an
        // agent worktree — so discovery is expected to succeed. We don't
        // hard-fail the build on an exotic packaging context, only prove
        // the call doesn't panic.
        let _ = git_backend_ready();
    }

    #[test]
    fn fuzzy_match_finds_subsequence() {
        assert!(fuzzy_ready());
    }
}
