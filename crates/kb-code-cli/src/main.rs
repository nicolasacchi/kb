//! `kb-code` — the kb-code CLI. Mirrors kb-cli's shape: a clap root +
//! subcommands. W1.2 made `identity` a real HTTP client against a running
//! `kb-code-server` daemon (mirroring `kb fleet status`'s
//! `GET /api/identity` pattern — see `crates/kb-cli/src/commands/fleet.rs`).
//! W1.3 added `refs`/`tree`/`cat`: DIRECT library calls against
//! `kb_code_server::git` (opened in-process against `--repo <PATH>`, no
//! daemon involved).
//!
//! **W1.6 wires the daemon HTTP surface (`GET /api/{repos,tree,file,
//! symbols,events}`)** — `tree`/`cat` gain an OPTIONAL `--daemon <url>`
//! flag: when given, `--repo` is reinterpreted as a repo NAME (a
//! `kb-code.toml`-configured repo the daemon knows about) and the command
//! goes over HTTP; when absent (the default, UNCHANGED from W1.3), `--repo`
//! stays a filesystem PATH and the command runs fully in-process — this is
//! the documented offline fallback, kept working byte-for-byte for every
//! existing invocation. `repos`/`symbols`/`events` are brand new in W1.6 and
//! are daemon-ONLY (no in-process form existed for them to preserve);
//! `--daemon` on those three DOES default to `http://127.0.0.1:4747` (the
//! daemon's own default listen address, `config::ServerSection::
//! DEFAULT_ADDR`), matching `identity`'s existing convention.
//!
//! **W2.1 adds `search files|symbols|text`** (`kb_code_server::search`'s
//! `GET /api/search/{files,symbols,text}` routes) — daemon-only, same
//! `--daemon` default convention as `repos`/`symbols`/`events`. **W2.3 adds
//! `search semantic`** (`GET /api/search/semantic`) — same shape, 400s if
//! the semantic lane is off (daemon-wide or for the requested `--repo`).
//!
//! **W2.5 adds `search transcripts` + `transcripts status`**
//! (`kb_code_server::transcripts::search`'s `GET /api/search/transcripts` /
//! `GET /api/transcripts/status`) — daemon-only, LOOPBACK-ONLY (the daemon
//! 404s a non-loopback caller regardless of `--daemon`'s host — see that
//! module's doc).
//!
//! **W3.1 adds `blame`/`timeline`** (`kb_code_server::blame`'s
//! `GET /api/blame` / `GET /api/blame/timeline`) — daemon-only, same
//! `--daemon` default convention as `repos`/`symbols`/`events`.
//!
//! **W3.2 adds `join <sha> --repo <name>`** (`kb_code_server::join::
//! ladder`'s `GET /api/join/commit`) — the join ladder's CLI surface:
//! which session (if any) produced a given commit. Daemon-only.
//!
//! **W3.3 adds `provenance-report`** (`kb_code_server::provenance::report`'s
//! `GET /api/provenance-report`) — the join-ladder confidence/via/
//! trailer-coverage instrument over a repo's commit history. Daemon-only.
//!
//! **W3.4 adds `why <PATH>[:<LINE>]` and `story <PATH>`**
//! (`kb_code_server::provenance::{why,story}`'s `GET /api/why` /
//! `GET /api/story`) — which session (if any) produced a line/file, and a
//! file's session timeline. Daemon-only.
//!
//! **W3.5 adds `session-diff <sid>`** (`kb_code_server::sessiondiff`'s
//! `GET /api/session-diff`) — the session diff: everything a session
//! changed, as one narrative review unit. Daemon-only, LOOPBACK-ONLY (same
//! gate as `search transcripts` — its payload carries raw transcript
//! prompt text).
//!
//! **W3.6 adds `backfill [--repo NAME]`** (`kb_code_server::join::backfill`'s
//! `POST /api/backfill?repo=`) — the join ladder's precompute: proactively
//! resolves a repo's commit history (bounded by `[backfill] depth`) through
//! the same six-arm ladder `join`/`why`/`story` already use, warming the
//! cache ahead of a live query. Daemon-only, ordinary `auth_bearer` gate
//! (not loopback-only — see that route's doc). Omitting `--repo` runs the
//! precompute over every repo the daemon is configured to browse.
//!
//! **W4.6 adds `annotations <PATH>` / `annotate <PATH>:<LINE> -m <body>`**
//! (`kb_code_server::annotations`'s `GET`/`POST /api/annotations`) —
//! durable, path-scoped line comments anchored via kb-core's own
//! `review::Anchor` fuzzy-resolve machinery. Daemon-only, ordinary
//! `auth_bearer` gate.
//!
//! **W4.7 adds `checkout <ref>`** (`kb_code_server::checkout::switch_repo`'s
//! `POST /api/checkout`) — the daemon's confirmed, ONLY working-tree
//! mutation: refuses (printing every dirty path) on a dirty working tree,
//! else `git switch`/`git checkout`s a clean one. Daemon-only,
//! LOOPBACK-ONLY (same gate as `session-diff`/`search transcripts`).
//!
//! **B3 ("kb-code v2 — The Operable Reader") adds `resolve
//! <PATH>:<LINE>:<COL>`** (`kb_code_server::resolve`'s `GET /api/resolve`) —
//! CLI parity for the SPA peek panel's new PRIMARY `gd`/`K` path: a
//! position-based (1-based line, 0-based col — tree-sitter's own `Point`
//! convention), occurrence-aware identifier lookup, ranked file-local /
//! same-repo / other-repo, each candidate carrying its own `precision` tier.
//! Daemon-only.
//!
//! **D3 ("kb-code v2 — The Operable Reader") gives annotations v2 full CLI
//! parity** — the agent's half of the human↔agent annotation dialogue
//! (`kb_code_server::annotations`/`routes`, wire landed in bee9b97e).
//! `annotate <PATH>:<LINE>` gains kind flags: `--to END` (→ `anchor_kind:
//! "range"`), `--symbol` (→ `"symbol"`, server-derived enclosing symbol —
//! a 404 "no enclosing symbol" is rendered as a friendly nudge to drop the
//! flag), `--sha REVSPEC` (→ `"diff"`, pinned to that commit's blob,
//! NEVER re-resolved), and `--intent` (note (default) | question | todo |
//! flag-for-agent | tour-stop) — the three kind flags are mutually
//! exclusive, gated client-side via `clap`'s `conflicts_with_all` (a `400`
//! from the daemon would say the same thing, slower). `annotate` ALSO
//! grows six thread/lifecycle subcommands acting on an existing annotation
//! by id — `reply <ID> -m BODY --repo --path [--intent]` (a reply is
//! itself a `POST /api/annotations` with `parent_id` set, so it still needs
//! `repo`/`path` to satisfy the wire's mandatory create fields; every OTHER
//! lifecycle verb below needs only the `id`, since `PATCH`/`DELETE
//! /api/annotations/{id}` require nothing else), `resolve <ID>`
//! (`PATCH {resolved:true}`), `reopen <ID>` (`{resolved:false}`), `edit <ID>
//! -m BODY` (`{body}`), `set-intent <ID> <INTENT>` (`{intent}`, vocab-gated
//! client-side against `kb_code_server::annotations::is_valid_intent` — the
//! same fn the daemon itself uses), and `delete <ID> [--yes]` (`DELETE`,
//! cascades to any replies — prompts for confirmation unless `--yes`,
//! mirroring `kb reset`'s own confirm convention). `annotate <PATH>:<LINE>`
//! (create) and `annotate <subcommand> <ID>` (lifecycle) coexist on ONE
//! verb via an `Option<String>` positional beside an `Option<AnnotateCmd>`
//! `#[command(subcommand)]` field — the SAME mixed pattern `Cmd::Search`
//! already uses for its bare `query` vs. `files|symbols|text|…` lanes.
//! `annotations <PATH>` (reading) gains an intent chip + anchor-kind column
//! (range → `L10–24`; diff → the short sha; symbol has NO name in the wire
//! `AnnotationView` — see that struct's doc — so it degrades to `L{line}`),
//! threaded (indented) reply display, and resolved rows hidden unless
//! `--all`. `annotations open --repo R [--intent] [--path-prefix]` is NEW —
//! `GET /api/annotations/open` verbatim, the D4 hook's future query surface.
//! Daemon-only throughout.
//!
//! **S1 adds `scip ingest <INDEX.SCIP> --repo NAME`** (`POST
//! /api/scip/ingest`, `kb_code_server::scip`) — the SCIP precision tier's
//! ingest surface. Parsing/protobuf lives ENTIRELY here (the `scip` and
//! `protobuf` crates are kb-code-cli-only dependencies — `kb-code-server`
//! stays dependency-free of both): `scip_map` maps a parsed `.scip` index's
//! occurrences to `{name, role, line, col_start, col_end}` rows, this
//! module reads each document's CURRENT bytes off the working tree
//! (resolved via the index's own `Metadata.project_root`) to compute a
//! `blob_hash` per document, then POSTs batches to the daemon — which
//! resolves each path to its OWN current blob_hash/salt and honestly skips
//! (never silently ingests against drifted content) any document whose
//! file has changed since. Daemon-only, LOOPBACK-ONLY (same gate as
//! `checkout`/`session-diff`).
//!
//! **PRR-R2** ("The PR Room," kb v0.39 T2, Phase 2) adds a NEW `pr` parent
//! family (`kb-code pr {list,show,checks,comments,fetch}`) — `list`/
//! `comments`/`fetch` drive EXISTING routes that had zero CLI coverage
//! before this unit (`GET /api/prs`, `GET /api/prs/{n}/comments`, `POST
//! /api/prs/fetch` [LOOPBACK]); `show`/`checks` drive this unit's NEW
//! routes (`GET /api/prs/{n}`, `GET /api/prs/{n}/checks`). `pr` is kept
//! deliberately separate from `review` (which already means kb-code's own
//! local review session) — widening "review" to also mean a bare GitHub PR
//! is a collision risk the design doc explicitly flags. `ReviewCmd` gains
//! `start-pr` (`POST /api/reviews/pr` [LOOPBACK]), `report` (`GET`/`PUT
//! /api/reviews/{id}/report`, `--set --from-file` for the PUT half
//! [LOOPBACK]), and `artifact` (`GET /api/reviews/{id}/artifact`).
//!
//! **PRR-R4** ("The PR Room," kb v0.39 T2, Phase 4) adds `review pr-status
//! ID` (`GET /api/reviews/{id}/pr-status`, design doc §2 row 12 — the
//! staleness probe), `review inbox [--repo R|--all-repos]` (`GET
//! /api/reviews/inbox`, design doc §2 row 13 — the cross-repo attention
//! queue; the two flags are mutually exclusive and one is REQUIRED, a
//! CLI-side guard against silently scanning every configured repo when the
//! caller meant to scope to one), and `review timeline ID` (`GET
//! /api/reviews/{id}/timeline`, milestone plan arbitration #7). All three
//! are ordinary bearer reads.

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use kb_code_server::code_actions::{CodeAction, CodeActionEdit}; // S2-B1 (design-s2.md § S2-C)
use kb_code_server::git::{GitRepo, DEFAULT_BLOB_SIZE_CAP};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

mod bench;
// Named `scip_map`, NOT `scip` — a local module named `scip` would collide
// with the `scip` crate itself in this binary's root namespace (a local
// item shadows an extern-prelude crate of the same name, per RFC 2126 —
// harmless from WITHIN `scip_map.rs` itself, since a bare `scip::` path
// there has no competing local module to resolve against, but it would
// make the external crate unreachable via a plain `scip::` path from HERE,
// in `main.rs`, without the `::scip::` global-path workaround). Simplest
// fix: don't shadow it at all.
mod scip_map;
mod sse;
mod watch;
// ── S2-B2: `kb-code inbox` (unified-inbox/1 one-shot + --watch poll) ──
mod inbox;
// ── V70-A5: `kb-code commands` (kbc-cmd/1 — the command/key registry) ──
// The ONLY module here that reads a build-time artefact rather than talking
// to a daemon: it `include_str!`s the SAME `registry.json` the server embeds,
// which is what lets `doctor` cross-join registry ids against THIS binary's
// real clap tree.
mod commands;
// ── V70-A8 (D20 CLI hygiene): envelope/exit-codes, bearer token, `tools` ──
mod envelope;
mod redact;
mod token;
mod tools;

#[derive(Parser, Debug)]
#[command(
    name = "kb-code",
    version = env!("CARGO_PKG_VERSION"),
    about = "kb-code — read-oriented code-browsing CLI"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Query a running kb-code daemon's `GET /api/identity`.
    Identity {
        /// Base URL of the kb-code daemon.
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        /// Print the raw JSON response instead of the human summary.
        #[arg(long)]
        json: bool,
    },
    /// List branches and tags. Direct library call against `--repo` — no
    /// daemon involved.
    Refs {
        /// Path to (or inside) the git repository.
        #[arg(long)]
        repo: PathBuf,
    },
    /// List a directory at a ref (default: repo root at HEAD).
    ///
    /// Offline (default): `--repo` is a filesystem PATH, read in-process.
    /// Daemon mode (`--daemon <url>`): `--repo` is a NAME configured on
    /// that daemon, read over `GET /api/tree`.
    Tree {
        /// Path within the repo (defaults to the root).
        path: Option<String>,
        /// Filesystem PATH (offline) or configured repo NAME (`--daemon`).
        #[arg(long)]
        repo: String,
        /// Revspec to read at: full/short sha, branch, tag, `HEAD~n`, ...
        #[arg(long = "ref", default_value = "HEAD")]
        rev: String,
        /// Print JSON instead of a human-readable table.
        #[arg(long)]
        json: bool,
        /// Read via a running kb-code daemon instead of in-process.
        #[arg(long)]
        daemon: Option<String>,

        // ── V71-F1 — kbc-tree/1. Passing ANY of these routes the verb to
        // `GET /api/tree/2` (the PROJECTED tree) instead of the legacy
        // per-directory ODB listing above, which is otherwise untouched.
        // Every one of them REQUIRES `--daemon`: the projection is
        // computed once server-side on purpose (the evidence report's
        // "two renderers of one projection will diverge" risk), so the
        // offline path refuses by name rather than growing a second
        // implementation.
        /// `physical` (default) · `role` · `namespace` · `change`.
        #[arg(long)]
        view: Option<String>,
        /// A kbc-scope/1 expression (`role:model && !path:spec//*`,
        /// `$generated`). A refused scope prints WHY and shows the
        /// unscoped tree — never a silently different set.
        #[arg(long)]
        scope: Option<String>,
        /// Fuzzy filter over the row labels, ranked by the daemon's ONE
        /// matcher (nucleo, exact tier first).
        #[arg(long)]
        filter: Option<String>,
        /// `filter` (default — prune non-matches, keep ancestry) or
        /// `highlight` (keep every row, badge the ancestors).
        #[arg(long)]
        mode: Option<String>,
        /// Up to three of `git,review,findings,annot,todo,bookmark`. A
        /// fourth is DROPPED and named.
        #[arg(long)]
        decorate: Option<String>,
        /// Base ref for the git lane and the `change` view (default: the
        /// repo's own default branch).
        #[arg(long)]
        base: Option<String>,
        /// Review id for the `review` / `findings` decoration lanes.
        #[arg(long)]
        review: Option<i64>,
        /// Levels to expand; `0` = the whole tree (default `0` for the CLI,
        /// which has no click-to-expand).
        #[arg(long)]
        depth: Option<u32>,
        /// Rows to return before `truncated` fires.
        #[arg(long)]
        limit: Option<usize>,
        /// `tree` (default) · `paths` (one repo-relative path per line,
        /// for `xargs`) · `json`.
        #[arg(long)]
        format: Option<String>,
    },
    /// Print the raw bytes of a file at a ref to stdout.
    ///
    /// Offline (default): `--repo` is a filesystem PATH, read in-process.
    /// Daemon mode (`--daemon <url>`): `--repo` is a NAME configured on
    /// that daemon, read over `GET /api/file`.
    Cat {
        /// Path to the file within the repo.
        path: String,
        /// Filesystem PATH (offline) or configured repo NAME (`--daemon`).
        #[arg(long)]
        repo: String,
        /// Revspec to read at: full/short sha, branch, tag, `HEAD~n`, ...
        #[arg(long = "ref", default_value = "HEAD")]
        rev: String,
        /// Read via a running kb-code daemon instead of in-process.
        #[arg(long)]
        daemon: Option<String>,
    },
    /// List every repo a kb-code daemon is configured to browse, with
    /// counts, HEAD, and watcher state (`GET /api/repos`). Daemon-only.
    Repos {
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// The `syntax/1` file-type registry (`GET /api/syntax`): every file
    /// type this daemon knows, its tree-sitter grammar (or the honest
    /// absence of one), its extraction tier, and the extensions, exact
    /// filenames and `#!` interpreters that address it. Daemon-only —
    /// the answer describes the DAEMON's build, not this CLI's.
    Syntax {
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        /// Print the raw `syntax/1` JSON instead of the table.
        #[arg(long)]
        json: bool,
    },
    /// The Parity Grid (`GET /api/parity`): every registry language
    /// against every capability (highlight, symbols, outline, usages,
    /// hover, lens), each cell derived from what the daemon can actually
    /// do — an honest map of where the instrument is weak, never a
    /// hand-written claim. Daemon-only.
    Parity {
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        /// Print the raw `parity/1` JSON instead of the grid.
        #[arg(long)]
        json: bool,
    },
    /// Per-file symbol list (PATH), or a repo-wide substring search
    /// (`--query`) — exactly one of the two. Daemon-only
    /// (`GET /api/symbols`); the real fuzzy-match lane is W2.1's job, this
    /// is a plain case-insensitive substring scan.
    Symbols {
        /// File path within the repo (mutually exclusive with `--query`).
        path: Option<String>,
        /// The daemon's configured repo NAME.
        #[arg(long)]
        repo: String,
        /// Revspec for the per-file form (ignored with `--query`).
        #[arg(long = "ref", default_value = "HEAD")]
        rev: String,
        /// Repo-wide substring search (mutually exclusive with PATH).
        #[arg(long)]
        query: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// Tail `GET /api/events` — the daemon's live-mirror SSE bus
    /// (`mirror.updated`/`repo.head_moved`). Daemon-only.
    Events {
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        /// Accepted for parity with `kb events --follow`; this command has
        /// no non-follow mode, so it always tails.
        #[arg(long)]
        follow: bool,
        /// NDJSON envelopes instead of human `id  type  payload` lines.
        #[arg(long)]
        json: bool,
    },
    /// `kb-code search <q>` — the unified Search-Everywhere box
    /// (`GET /api/search`, W2.4): one query, grammar-routed to one or more
    /// of the six lanes (`@sym`/`#file`/`/regex/`/`?nl semantic`/`~session`/
    /// `~~transcript`, or every lane with no prefix), rendered as headered
    /// sections. Pass a lane SUBCOMMAND instead (`files|symbols|text|
    /// semantic|transcripts`) to hit exactly that one lane directly — same
    /// as before W2.4, unchanged. Daemon-only.
    Search {
        /// Bare query for the unified box. Omit this and use a lane
        /// subcommand below instead to search exactly one lane directly.
        query: Option<String>,
        /// Scope to one configured repo (omit to search every repo the
        /// unified box's lanes support scoping across).
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
        /// V71-D1 — ask for the ranking decomposition. Sugar for appending
        /// `explain:1` to QUERY: the kbcq/1 grammar IS the protocol, so
        /// there is no separate wire parameter to drift from it.
        #[arg(long)]
        explain: bool,
        /// V71-D1 — cap the `--json` payload at roughly this many TOKENS
        /// (~4 bytes each), dropping whole hits from the END of each lane
        /// and reporting what was dropped in `truncated`. The probe-then-
        /// fetch shape an agent wants; never a silent cut.
        #[arg(long)]
        budget: Option<usize>,
        /// V71-D1 — the cheap probe: per-lane counts only, no hit bodies.
        /// Answers "how big is this result set" for ~50 tokens.
        #[arg(long)]
        count_only: bool,
        /// V71-D2 — the facet census over the page this call returns
        /// (`basis: "page"`, never a corpus estimate). Sugar for appending
        /// `facets:1` to QUERY — same reason `--explain` is sugar: the
        /// kbcq/1 grammar IS the protocol, so the SPA's facet rail, a saved
        /// search and this flag all express one fact one way.
        #[arg(long)]
        facets: bool,
        /// V71-D2 — group the returned page (`file|kind|lane|dir|none`).
        /// Sugar for appending `group:<key>` to QUERY. Grouping partitions
        /// the page; it never re-ranks or drops a hit.
        #[arg(long, value_name = "KEY")]
        group: Option<String>,
        #[command(subcommand)]
        cmd: Option<SearchCmd>,
    },
    /// The raw-transcripts lane (W2.5) — a PULL-ONLY full-text index over
    /// the operator's own local Claude Code transcript JSONL. Daemon-only,
    /// LOOPBACK-ONLY (the daemon 404s a non-loopback request regardless of
    /// `--daemon`'s host — this only ever works pointed at a daemon this
    /// CLI can reach as a loopback peer).
    Transcripts {
        #[command(subcommand)]
        cmd: TranscriptsCmd,
    },
    /// `kb-code blame <PATH>` — `GET /api/blame` (W3.1): per-region `git
    /// blame`-style attribution for a file. Daemon-only (the daemon owns
    /// the region cache + dirty-working-tree detection).
    Blame {
        /// File path within the repo.
        path: String,
        /// The daemon's configured repo NAME.
        #[arg(long)]
        repo: String,
        /// Revspec to blame at (default: the repo's current state — HEAD,
        /// or the live working-tree bytes if `path` has an uncommitted
        /// edit).
        #[arg(long = "ref")]
        rev: Option<String>,
        /// Narrow to a 1-based inclusive line range `START:END` (default:
        /// the whole file).
        #[arg(long)]
        lines: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code timeline <PATH>:<LINE>` — `GET /api/blame/timeline` (W3.1):
    /// the bounded, newest-first set of commits that have ever touched
    /// exactly that line. Daemon-only.
    Timeline {
        /// `PATH:LINE`, e.g. `src/lib.rs:42`.
        target: String,
        #[arg(long)]
        repo: String,
        #[arg(long)]
        max: Option<usize>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code join <sha>` — the join ladder (W3.2): which session (if
    /// any) produced this commit, via a fixed trailer/exact/fuzzy/none
    /// arm sequence (`GET /api/join/commit?repo=&sha=`,
    /// `kb_code_server::join::ladder`). Daemon-only; probe-grade CLI
    /// naming (this is an inspection surface, not a stable long-term
    /// contract).
    Join {
        /// Full or short (>=4 hex chars) git sha — local `git`/gix
        /// resolution disambiguates a short prefix before kb is ever
        /// queried.
        sha: String,
        /// The daemon's configured repo NAME.
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code why <PATH>[:<LINE>]` — W3.4: line-grade ("which session
    /// produced this line") or (no `:LINE`) file-grade ("which sessions
    /// dominate this file") attribution (`GET /api/why`,
    /// `kb_code_server::provenance::why`). Daemon-only.
    Why {
        /// `PATH` or `PATH:LINE`.
        target: String,
        /// The daemon's configured repo NAME.
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code story <PATH>` — W3.4: the file's (or `--symbol`'s) session
    /// timeline (`GET /api/story`, `kb_code_server::provenance::story`).
    /// Daemon-only.
    Story {
        path: String,
        /// The daemon's configured repo NAME.
        #[arg(long)]
        repo: String,
        /// Restrict to one symbol's current line range.
        #[arg(long)]
        symbol: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code provenance-report` — W3.3: the join-ladder confidence/via/
    /// trailer-coverage instrument over a repo's commit history
    /// (`GET /api/provenance-report`,
    /// `kb_code_server::provenance::report`) — supersedes kb-cli's W0.6
    /// probe (`kb sessions provenance-report`), which stays in place
    /// unchanged. Daemon-only.
    ProvenanceReport {
        /// The daemon's configured repo NAME.
        #[arg(long)]
        repo: String,
        /// Bound on how many commits `git log HEAD` walks (default 2000).
        #[arg(long)]
        max_count: Option<usize>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code session-diff <sid>` — the session diff (W3.5):
    /// everything a session changed, as one narrative review unit
    /// (`GET /api/session-diff`, `kb_code_server::sessiondiff`). Daemon-
    /// only, LOOPBACK-ONLY (same gate as `search transcripts` — the
    /// payload carries raw transcript prompt text; only works pointed at a
    /// daemon this CLI can reach as a loopback peer).
    SessionDiff {
        /// The session id (`sessionId` from the transcript JSONL —
        /// mirrors `kb why`/`kb recollect`'s own identifier).
        session: String,
        /// Narrow to one configured repo (omit to consider every repo).
        #[arg(long)]
        repo: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code backfill` — W3.6: the join ladder's PRECOMPUTE
    /// (`POST /api/backfill?repo=`, `kb_code_server::join::backfill`) —
    /// proactively resolves a repo's commit history (bounded by
    /// `[backfill] depth` in `kb-code.toml`) through the same six-arm
    /// ladder `join`/`why`/`story` use, warming the `commit_sessions`
    /// cache. Daemon-only.
    Backfill {
        /// The daemon's configured repo NAME. Omit to run the precompute
        /// over EVERY repo the daemon is configured to browse
        /// (`GET /api/repos` enumerates them first).
        #[arg(long)]
        repo: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code annotations <PATH> --repo R [--all]` — W4.6 (D3 upgrades
    /// the display + adds the `open` subcommand below): every annotation on
    /// `PATH`, each with its live-resolved line + staleness, threaded
    /// (replies indented under their parent), resolved rows hidden unless
    /// `--all` (`GET /api/annotations?repo=&path=`,
    /// `kb_code_server::annotations`). Daemon-only.
    Annotations {
        /// File path within the repo. Omitted when `open` (below) is used
        /// instead.
        path: Option<String>,
        /// The daemon's configured repo NAME. Required for the plain
        /// per-path form (`open` below takes its own copy).
        #[arg(long)]
        repo: Option<String>,
        /// Include resolved annotations too (shown dimmed/marked
        /// `(resolved)`) — default hides them so the listing reads as an
        /// open-items view.
        #[arg(long)]
        all: bool,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        cmd: Option<AnnotationsCmd>,
    },
    /// `kb-code annotate <PATH>:<LINE> -m <body> [--to END|--symbol|--sha
    /// REV] [--intent I]` — create a durable annotation anchored to
    /// `LINE`'s CURRENT content (W4.6; D3 adds the kind flags + intent);
    /// OR one of the thread/lifecycle subcommands below, acting on an
    /// EXISTING annotation by id (D3). `POST`/`PATCH`/`DELETE
    /// /api/annotations[/{id}]`. Daemon-only.
    Annotate {
        /// `PATH:LINE` (1-based), e.g. `src/lib.rs:42`. Required for the
        /// create form; omitted when a lifecycle subcommand (below) is
        /// used instead.
        target: Option<String>,
        /// The annotation body (create form only).
        #[arg(short = 'm', long = "message")]
        message: Option<String>,
        /// Range end line (1-based, inclusive) — makes this a `"range"`
        /// annotation spanning `LINE..=END`. Mutually exclusive with
        /// `--symbol`/`--sha`.
        #[arg(long = "to", conflicts_with_all = ["symbol", "sha"])]
        to: Option<u32>,
        /// Derive `LINE`'s enclosing symbol — makes this a `"symbol"`
        /// annotation that follows the symbol through drift. Mutually
        /// exclusive with `--to`/`--sha`. A daemon 404 (no symbol encloses
        /// `LINE`) is rendered as a friendly nudge to drop this flag.
        #[arg(long, conflicts_with_all = ["to", "sha"])]
        symbol: bool,
        /// Pin to `LINE` in THIS revspec's version of the file — makes
        /// this a `"diff"` annotation, never re-resolved against the
        /// working tree. Mutually exclusive with `--to`/`--symbol`.
        #[arg(long, conflicts_with_all = ["to", "symbol"])]
        sha: Option<String>,
        /// note (default) | question | todo | flag-for-agent | tour-stop.
        #[arg(long)]
        intent: Option<String>,
        /// V70-A3X — review-scope this annotation (`CreateAnnotationBody::
        /// review_id`): the review must exist, belong to `--repo`, and
        /// `--ps` (default: latest) must exist. Create form only.
        #[arg(long)]
        review: Option<i64>,
        /// V70-A3X — patchset number within `--review` (default: latest).
        /// Requires `--review`.
        #[arg(long, requires = "review")]
        ps: Option<i64>,
        /// V70-A3X — `new` (default) | `old`: which side of the patchset
        /// the anchor is built from. Requires `--review`.
        #[arg(long, requires = "review")]
        side: Option<String>,
        /// The daemon's configured repo NAME (create form only).
        #[arg(long)]
        repo: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        cmd: Option<AnnotateCmd>,
    },
    /// `kb-code checkout <ref>` — W4.7: the daemon's confirmed, ONLY
    /// working-tree mutation (`POST /api/checkout`,
    /// `kb_code_server::checkout::switch_repo`). Refuses (printing every
    /// dirty path) on a dirty working tree; a clean tree switches to a
    /// known local branch or detaches onto anything else (tag/remote
    /// branch/raw sha). Daemon-only, LOOPBACK-ONLY (same gate as
    /// `session-diff`/`search transcripts` — only works pointed at a
    /// daemon this CLI can reach as a loopback peer).
    Checkout {
        /// Branch name, tag, or (short/full) sha to switch to.
        target: String,
        /// The daemon's configured repo NAME.
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code map [DIR] [--repo] [--budget N]` — W5.1: a ranked,
    /// token-budgeted outline of a directory/repo (`GET /api/map`,
    /// `kb_code_server::agentview::map`). Daemon-only.
    Map {
        /// Directory to scope the outline to (default: the whole repo).
        dir: Option<String>,
        #[arg(long)]
        repo: String,
        /// Approximate token budget (chars/4) — default 2000.
        #[arg(long)]
        budget: Option<usize>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code pack <PATHS...> [--budget N]` — W5.1: a context pack for
    /// the given files — map outline + provenance summary + annotations +
    /// recent story entries, then as much file content as the budget
    /// allows (`GET /api/pack`, `kb_code_server::agentview::pack`).
    /// Daemon-only.
    ///
    /// Phase E3 adds `--set <NAME-OR-ID>` — resolve the pack's paths from
    /// an existing reading set instead of PATHS (mutually exclusive;
    /// resolved to an id client-side via `GET /api/sets?repo=`, same
    /// exact-name-then-id-prefix ladder `kb-code set show` uses). A span
    /// carrying a line range or a note is shown alongside its file's
    /// section.
    Pack {
        /// One or more repo-relative file paths. Mutually exclusive with
        /// `--set`.
        paths: Vec<String>,
        /// Resolve to an existing reading set's own ordered paths instead
        /// of PATHS — NAME or ID.
        #[arg(long, conflicts_with = "paths")]
        set: Option<String>,
        #[arg(long)]
        repo: String,
        /// Approximate token budget (chars/4), content only — default 4000.
        #[arg(long)]
        budget: Option<usize>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code defs <SYMBOL>` — W5.2: an exact symbols-table lookup (fuzzy
    /// fallback when nothing matches exactly), `GET /api/defs`
    /// (`kb_code_server::agentview::xref`). Daemon-only.
    Defs {
        symbol: String,
        /// Scope to one configured repo (omit to search every repo).
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code xrefs <SYMBOL> --repo NAME` — W5.2: grep-searcher
    /// word-boundary text search for `SYMBOL` across a repo's working tree
    /// (`GET /api/xrefs`, `kb_code_server::agentview::xref`) — tags-tier,
    /// every result labeled approximate. Daemon-only. Named `xrefs` (not
    /// `refs`) to avoid colliding with the pre-existing `Cmd::Refs`
    /// (branches/tags, `GET .../refs`, W1.3) — the HTTP path is `/api/xrefs`
    /// for the same reason (see `router.rs`'s module doc).
    Xrefs {
        symbol: String,
        #[arg(long)]
        repo: String,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code similar <PATH>:<START>-<END> --repo NAME` — W5.2: nearest
    /// semantic-lane chunks to the given span, excluding the span's own
    /// source location (`GET /api/similar`,
    /// `kb_code_server::agentview::similar`). Daemon-only; 400s (same as
    /// `search semantic`) when the semantic lane isn't enabled.
    Similar {
        /// `PATH:START-END`, e.g. `src/lib.rs:10-42`.
        target: String,
        #[arg(long)]
        repo: String,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code impact <PATH> --repo NAME` — W5.2 co-change neighborhood
    /// (`GET /api/impact`). Or `kb-code impact <PATH>:<LINE>:<COL> --repo`
    /// — V3.1-H2 compositional impact analysis (`GET /api/impact/analysis`).
    /// Position form is detected by a trailing `:LINE:COL`. Daemon-only.
    Impact {
        /// File path (co-change) OR `PATH:LINE:COL` (analysis).
        path: String,
        #[arg(long)]
        repo: String,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code lenses <PATH> --repo NAME` — V3.1-H2 Code Vision lens
    /// aggregation (`GET /api/lenses`): per-declaration usage counts,
    /// implementors, author/session. Daemon-only.
    Lenses {
        path: String,
        #[arg(long)]
        repo: String,
        #[arg(long = "ref")]
        rev: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code hotspots --repo NAME` — V3.2-B1 attention hotspots
    /// (`GET /api/behavioral/hotspots`). Decomposed score terms, never a
    /// grade. Daemon-only.
    Hotspots {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        limit: Option<usize>,
        /// Named scope include (`tests`) or exclude (`!generated`).
        #[arg(long)]
        scope: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code coupling <PATH> --repo NAME` — V3.2-B1 ROSE co-change
    /// partners (`GET /api/behavioral/coupling`). Confidence is
    /// asymmetric. Daemon-only.
    Coupling {
        path: String,
        #[arg(long)]
        repo: String,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code owners <PATH> --repo NAME` — V3.2-B1 Bird-style ownership
    /// (`GET /api/behavioral/ownership`). Metric only — no defect
    /// prediction. Daemon-only.
    Owners {
        path: String,
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code age <PATH> --repo NAME` — V3.2-B1 line age from blame
    /// (`GET /api/behavioral/age`). Daemon-only.
    Age {
        path: String,
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code behavioral backfill|timeseries …` — V3.2-B1 backfill +
    /// V3.4-C1 request-time activity time-series. Daemon-only; backfill is
    /// LOOPBACK-ONLY.
    Behavioral {
        #[command(subcommand)]
        cmd: BehavioralCmd,
    },
    /// `kb-code canvas list --repo R` — V3.4-C1 list canvas sets
    /// (`GET /api/canvas`). **Read-only** — no create/edit/delete verbs
    /// (D7: canvas is SPA interaction chrome; server durability only).
    Canvas {
        #[command(subcommand)]
        cmd: CanvasCmd,
    },
    /// `kb-code recipes` — V3.3-Q1 catalog of named deterministic recipes
    /// (`GET /api/recipes`). Daemon-only. Pure enum listing — no repo touch.
    Recipes {
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code audit [--since <iso|dur>] [--limit N] [--json]` — V70-A2
    /// (SEC-20): the daemon's append-only mutations ledger, newest first.
    ///
    /// Every mutating `/api` request is one row — success OR failure —
    /// with the admission rung it was let in on (`loopback` | `bearer` |
    /// `review_gate`). Daemon-only; an ordinary bearer read.
    Audit {
        /// Window start: an ISO instant/date (`2026-09-03`,
        /// `2026-09-03T10:00:00Z`) or a duration (`90m`, `24h`, `7d`).
        /// Default: 24h.
        #[arg(long)]
        since: Option<String>,
        /// Row cap (server hard-caps at 500).
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code recipe <NAME> --repo NAME [--since] [--limit]` — V3.3-Q1
    /// run one named deterministic recipe (`GET /api/recipes/{name}`).
    /// Daemon-only. Operator-critical parity surface (D7).
    Recipe {
        /// Kebab-case recipe name (`new-public-api`, `god-functions`, …).
        name: String,
        #[arg(long)]
        repo: String,
        /// Required by `new-public-api` and `complexity-climbers`
        /// (git ref or ISO date `YYYY-MM-DD`).
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
        /// Named scope include (`tests`) or exclude (`!generated`).
        #[arg(long)]
        scope: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code hook install|uninstall|status` — W5.3: the
    /// `kb-code-why.sh` PreToolUse hook's install surface. NEVER edits
    /// `~/.claude/settings.json` (or any project one) itself — `install`/
    /// `uninstall` only PRINT the JSON snippet + plugin-manifest path;
    /// `status` checks the manual-install script path and daemon
    /// reachability. See `plugins/kb-code/README.md`.
    Hook {
        #[command(subcommand)]
        cmd: HookCmd,
    },
    /// `kb-code bench-search --queries <file.jsonl>` — W5.5: recall@1/@5
    /// (per lane + overall) and p50/p95 latency (per lane) over the
    /// unified Search-Everywhere box (`GET /api/search`), against a
    /// `.jsonl` set of `{query, expect_path, expect_kind?}` rows. A
    /// starter set for the kb repo itself ships at
    /// `crates/kb-code-cli/bench/kb-repo-queries.jsonl`.
    ///
    /// GOVERNANCE (ADR-6, kb-code Wave 5 plan): `[semantic] enabled`'s
    /// DEFAULT is `false` (`crate::config`'s `SemanticSection`, W2.3).
    /// Flipping that default to `true` requires a RECORDED run of this
    /// bench (`--json` output, not a vibe check) showing the semantic
    /// lane's measured recall/latency justify it — this command's output
    /// is the evidence, not the decision.
    ///
    /// SESSIONS/TRANSCRIPTS ARE STRUCTURALLY UNMEASURABLE HERE, ALWAYS
    /// 0% recall — not a lane defect, not a stale eval set. `bench::
    /// hit_rank` only matches a hit's `path` field; `search::sessions::
    /// SessionHit` and `transcripts::search::TranscriptHit` carry no
    /// `path` field at all (session_id/title/snippet/... instead), so
    /// EVERY hit either lane ever returns scores as a miss, regardless of
    /// query quality (see `bench.rs`'s own module doc, "Scoring", and
    /// `bench/kb-repo-queries.jsonl`'s header for the full explanation +
    /// the sessions lane's additional live-daemon-flakiness caveat). These
    /// two columns only appear in the table at all because a BARE
    /// (unprefixed) query fans out to every lane — no query row here
    /// deliberately targets them with `~`/`~~`.
    BenchSearch {
        /// Path to the `.jsonl` query file (`{"query":"...",
        /// "expect_path":"...", "expect_kind":"..."}` per line;
        /// `expect_kind` is optional and only labels the human table).
        #[arg(long)]
        queries: PathBuf,
        /// Scope every query to one configured repo (omit to search every
        /// repo the daemon knows about, same as a bare `kb-code search`).
        #[arg(long)]
        repo: Option<String>,
        /// Per-query result depth requested from `/api/search` (recall@1/@5
        /// are always scored regardless of this value; raise it if a query
        /// set wants recall@k for k > 5). Default: the unified box's own
        /// default limit.
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code resolve <PATH>:<LINE>:<COL> --repo <NAME>` — B3: position-
    /// based identifier lookup (`GET /api/resolve`, `kb_code_server::
    /// resolve`) — the CLI parity for the peek panel's new PRIMARY path.
    /// Occurrence-aware where the file-local tags-tier `defs`/`xrefs` above
    /// are name-only: ranked file-local / same-repo / other-repo
    /// candidates, each carrying its own `precision` tier (see that
    /// module's doc for the exact ranking + honesty note). Daemon-only.
    Resolve {
        /// `PATH:LINE:COL` — 1-based LINE, 0-based COL (tree-sitter's own
        /// `Point` convention, matching every other position field this
        /// crate serves — one field longer than `kb-code timeline`'s own
        /// `PATH:LINE`).
        target: String,
        /// The daemon's configured repo NAME.
        #[arg(long)]
        repo: String,
        /// Revspec to read the file at (default: the repo's current state).
        #[arg(long = "ref")]
        rev: Option<String>,
        /// The server has no `?limit=`/pagination surface yet (`resolve`'s
        /// own module doc — `MAX_CANDIDATES` is a hardcoded response cap);
        /// sent anyway for parity with every other list-shaped verb here, a
        /// harmless no-op against today's daemon.
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code usages <PATH>:<LINE>:<COL> --repo <NAME>` — V3.G2: classified
    /// usages of the symbol at a position (`GET /api/usages`) — exact /
    /// likely / candidate groups with access tags. Daemon-only.
    Usages {
        /// `PATH:LINE:COL` — same convention as `resolve`.
        target: String,
        #[arg(long)]
        repo: String,
        #[arg(long = "ref")]
        rev: Option<String>,
        /// Per-class cap (default 500, max 500).
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
        /// V71-E1 (D4) — ask for the `usages/2` wire (`GET /api/usages/2`)
        /// instead: per-row kind (closed vocabulary) · SCIP-style role
        /// bitset · precision · enclosing symbol · blob_sha · in-band TRUE
        /// totals, plus the Ruby STRICT-rule verdict when it applies.
        /// Without this flag the verb calls `usages/1`, unchanged.
        #[arg(long = "v2")]
        v2: bool,
    },
    /// `kb-code hover <PATH>:<LINE>:<COL> --repo <NAME>` — PRR-N5:
    /// composed tooltip-shaped view (symbol + framework halves) at a
    /// position (`GET /api/hover`). Daemon-only.
    Hover {
        /// `PATH:LINE:COL` — same convention as `resolve`.
        target: String,
        #[arg(long)]
        repo: String,
        #[arg(long = "ref")]
        rev: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    // --- PRR-L2 (append-only; delimited from concurrent edits elsewhere
    // in this file — see the CLI's own module doc for the convention).
    /// `kb-code diagnostics <PATH> --repo <NAME>` — PRR-L2: live diagnostics
    /// from a configured lip/1 provider (`GET /api/diagnostics`). Prints an
    /// honest "no provider configured"/"provider unavailable" line rather
    /// than an empty list when the daemon carries no [[intel.providers]]
    /// entry for this (repo, lang) or the round trip failed — null and
    /// empty are never conflated. Daemon-only.
    Diagnostics {
        /// Source-relative path.
        path: String,
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    // --- end PRR-L2 append -------------------------------------------------
    // --- S2-B1 (design-s2.md § S2-C; append-only, delimited from
    // concurrent edits elsewhere in this file — see the CLI's own module
    // doc for the convention).
    /// `kb-code code-actions PATH:LINE[:COL] [--end LINE[:COL]] --repo R
    /// [--kinds a,b] [--json] [--suggest N]` — S2-C: live LSP code actions
    /// (quick fixes) from a configured lip/1 provider
    /// (`POST /api/code-actions`). Lists actions with 1-based indices; an
    /// honest "unavailable (REASON)" line (never an empty list) when
    /// nothing was fetched. `--suggest N` converts the Nth listed action
    /// into one annotation+suggestion record PER (file, edit) via the
    /// EXISTING `POST /api/annotations/batch` op — never a new mutation
    /// route (design-s2.md § S2-C). Daemon-only.
    CodeActions {
        /// `PATH:LINE` or `PATH:LINE:COL` (col defaults to 0).
        target: String,
        /// `LINE` or `LINE:COL` (col defaults to 0). Defaults to `target`'s
        /// own line/col — a point range.
        #[arg(long = "end")]
        end: Option<String>,
        #[arg(long)]
        repo: String,
        /// Comma-separated LSP `CodeActionKind` allowlist (e.g.
        /// `quickfix`).
        #[arg(long)]
        kinds: Option<String>,
        /// Convert the 1-based action at this index into a suggestion.
        #[arg(long)]
        suggest: Option<usize>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    // --- end S2-B1 append ---------------------------------------------
    /// `kb-code framework <PATH> --repo <NAME> [--kind K]` — PRR-N5: the
    /// direct `rails_edges` read (`GET /api/framework/edges`) — every edge
    /// where `PATH` is the src or the dst, direction-labeled. Daemon-only.
    Framework {
        /// Source-relative path.
        path: String,
        #[arg(long)]
        repo: String,
        /// Optional closed rails-lens/1 `kind` filter (e.g.
        /// `render_partial`, `route_action`).
        #[arg(long)]
        kind: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code resolve-symbol <SYM> --repo <NAME>` — PRR-N5: symbol-level
    /// deep-link resolution (`GET /api/resolve-symbol`). `SYM` is the
    /// `<namespace>:<container>:<name>[:<kind>]` grammar (see
    /// `kb_code_server::symbol_addr`'s module doc) — e.g.
    /// `rust:kb_core::config:ServerSection` or `rails:route:users#create`.
    /// Never errors on a miss — prints a `found: false` line instead.
    /// Daemon-only.
    ResolveSymbol {
        sym: String,
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code callees <PATH>:<LINE>:<COL> --repo <NAME>` — V3.1-H1:
    /// call sites inside the function/method at the position
    /// (`GET /api/hierarchy/callees`). Daemon-only.
    Callees {
        target: String,
        #[arg(long)]
        repo: String,
        #[arg(long = "ref")]
        rev: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code callers <PATH>:<LINE>:<COL> --repo <NAME>` — V3.1-H1:
    /// call sites of the function/method at the position
    /// (`GET /api/hierarchy/callers`). Daemon-only.
    Callers {
        target: String,
        #[arg(long)]
        repo: String,
        #[arg(long = "ref")]
        rev: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code implementors <TYPE> --repo <NAME>` — V3.1-H1: one-level
    /// type hierarchy around TYPE (`GET /api/hierarchy/types`). Daemon-only.
    Implementors {
        /// Type/trait/interface/class name.
        name: String,
        #[arg(long)]
        repo: String,
        /// Optional path to disambiguate same-named types.
        #[arg(long)]
        path: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code set list|show|create|add|rm|delete|from-session` — Phase E3:
    /// named, server-persisted, ordered collections of file/span
    /// references (`GET`/`POST`/`PATCH`/`DELETE /api/sets[...]`,
    /// `kb_code_server::reading_sets`). Daemon-only; `from-session` is
    /// LOOPBACK-ONLY (reuses `sessiondiff::session_diff`, same gate as
    /// `session-diff`/`checkout`).
    Set {
        #[command(subcommand)]
        cmd: SetCmd,
    },
    /// `kb-code review …` — V3.R1 local Gerrit-lite review sessions
    /// (`/api/reviews…`). Mutations are LOOPBACK-ONLY.
    Review {
        #[command(subcommand)]
        cmd: ReviewCmd,
    },
    /// `kb-code pr …` — PRR-R2: the raw GitHub PR overlay
    /// (`/api/prs…`). Deliberately a SEPARATE noun from `review` (kb-code's
    /// own local review session) — see the module doc. `fetch` is
    /// LOOPBACK-ONLY (the one ref-write); every other verb is a plain
    /// bearer-gated read.
    Pr {
        #[command(subcommand)]
        cmd: PrCmd,
    },
    /// `kb-code branches --repo R [--sort name|suggested]` — V4.P1:
    /// `GET /api/branches` (`branches/1`). Daemon-only.
    Branches {
        /// The daemon's configured repo NAME.
        #[arg(long)]
        repo: String,
        /// `name` (default, lexicographic) or `suggested` (recency / open
        /// review / ahead).
        #[arg(long, default_value = "name")]
        sort: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code compare <FROM> <TO> --repo R [--three-dot]` — V4.P1:
    /// `GET /api/compare`. Daemon-only.
    Compare {
        from: String,
        to: String,
        #[arg(long)]
        repo: String,
        /// Three-dot (`A...B` = merge-base..B) instead of two-dot.
        #[arg(long = "three-dot")]
        three_dot: bool,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code merge-check <TO> [--from BASE] --repo R` — V4.P1:
    /// `GET /api/merge-check`. `--from` defaults to the server's default
    /// branch (`GET /api/branches`). Daemon-only.
    MergeCheck {
        to: String,
        /// Base revspec. Defaults to the repo's default branch.
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code repo-state --repo R` — V4.P1: `GET /api/repo-state`
    /// (current git op / dirty / conflicts). Daemon-only.
    RepoState {
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code suggest <ID> (-m TEXT | --from-file F)` — PUT a
    /// suggestion on an annotation; or `list` / `apply` / `drop`.
    /// Daemon-only. `apply` is LOOPBACK-ONLY.
    Suggest {
        /// Annotation id (PUT form). Omit when using a subcommand.
        id: Option<String>,
        /// Replacement text (PUT form). Mutually exclusive with
        /// `--from-file`.
        #[arg(short = 'm', long = "message", conflicts_with = "from_file")]
        message: Option<String>,
        /// Read replacement text from a file (PUT form).
        #[arg(long = "from-file", conflicts_with = "message")]
        from_file: Option<PathBuf>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        cmd: Option<SuggestCmd>,
    },
    /// `kb-code bookmarks [--repo R]` — Phase N: list durable per-repo
    /// bookmarks (`GET /api/bookmarks`). Daemon-only.
    Bookmarks {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code bookmark <PATH>:<LINE> …` / `kb-code bookmark rm …` —
    /// Phase N: create or delete a bookmark (`POST`/`DELETE
    /// /api/bookmarks`). Daemon-only.
    Bookmark {
        /// Create form: `PATH:LINE` (mutually exclusive with `rm`).
        target: Option<String>,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        mnemonic: Option<String>,
        #[arg(long)]
        note: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        cmd: Option<BookmarkCmd>,
    },
    /// `kb-code todos [--repo R] [--marker M] [--path-prefix P]` — Phase N:
    /// list TODO/FIXME/… comment markers (`GET /api/todos`). Daemon-only.
    Todos {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        marker: Option<String>,
        #[arg(long = "path-prefix")]
        path_prefix: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code scip ingest <INDEX.SCIP> --repo NAME` — S1: the SCIP
    /// precision tier's ingest surface (`POST /api/scip/ingest`,
    /// `kb_code_server::scip`). Parses a `.scip` index generated by a real
    /// language server/compiler front-end and POSTs already-mapped
    /// occurrence rows to the daemon, which resolves each document's path
    /// to its CURRENT blob and replaces that blob's `source = 'scip'`
    /// occurrence rows — a document whose file has drifted since the CLI
    /// read it is honestly skipped (`stale`), never ingested against
    /// out-of-date positions. Generate an index first:
    ///
    ///   rust-analyzer scip .          # Rust — run at the repo root
    ///   scip-typescript index         # TypeScript/JavaScript — run at the repo root
    ///
    /// Both write `index.scip` by default. LOOPBACK-ONLY (same gate as
    /// `checkout`/`session-diff`).
    Scip {
        #[command(subcommand)]
        cmd: ScipCmd,
    },
    /// `kb-code stacks [--repo R] [--all] [--json]` / `kb-code stacks diff
    /// --branch B` — V3.3-S2 stack awareness: list dependent-branch stacks
    /// (`GET /api/stacks`) and show one layer's incremental numstat
    /// (`GET /api/stacks/layer-diff`). Daemon-only.
    Stacks {
        /// Configured repo NAME (required for the list form; optional on
        /// `diff` if passed there).
        #[arg(long)]
        repo: Option<String>,
        /// Include single-layer stacks (branches based on the default
        /// with no dependents). Default listing hides them.
        #[arg(long)]
        all: bool,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        cmd: Option<StacksCmd>,
    },
    /// `kb-code doclens …` — DCB: the doc↔code lens over kb documents
    /// (`/api/doc-lens…`). Resolves a kb document's extracted code
    /// references against ONE named checkout, fresh, and never stores the
    /// verdict.
    ///
    /// NOT to be confused with `kb-code lenses <PATH>` (V3.1-H2 Code Vision
    /// per-declaration aggregation, `GET /api/lenses`) — a different feature
    /// answering a different question. Same collision-avoidance reasoning
    /// that named `xrefs` rather than `refs`.
    Doclens {
        #[command(subcommand)]
        cmd: DoclensCmd,
    },
    // ── S2-B2: `kb-code inbox` (unified-inbox/1) ──
    /// `kb-code inbox [--json] [--watch] [--interval SECS]` — S2-A: render
    /// `GET /api/inbox` (`unified-inbox/1`), kb-code-server's federated
    /// three-lane attention queue (reviews awaiting you, open questions in
    /// working trees, and kb's desk + open-comments lanes — surfaced-
    /// never-scored, per lane, never a merged cross-lane score). One-shot
    /// by default; `--json` prints the raw body verbatim. `--watch` is a
    /// plain HTTP poll loop (own seed-then-diff seen-set — see
    /// `inbox.rs`'s module doc; NOT the SSE-driven `annotate watch`
    /// machinery), printing only new/changed rows plus kb-lane
    /// availability flips after the initial snapshot. Daemon-only.
    Inbox {
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
        /// Poll `GET /api/inbox` on an interval instead of a one-shot read.
        #[arg(long)]
        watch: bool,
        /// Poll interval in seconds (only meaningful with `--watch`).
        #[arg(long, default_value_t = 30)]
        interval: u64,
    },
    // ── V70-A5: `kb-code commands …` (kbc-cmd/1) ──
    // APPENDED AT THE END of `Cmd` on purpose — sibling units are also
    // adding variants here, and appending keeps the merges disjoint.
    /// `kb-code commands …` — the kbc-cmd/1 command + keyboard registry
    /// (`crates/kb-code-server/commands/registry.json`, embedded here by
    /// `include_str!` and served by the daemon at `GET /api/commands`).
    ///
    /// The ONE declaration home for every kb-code binding: the SPA's `?`
    /// sheet, its palette and its which-key overlay all render from these
    /// rows, and `commands doctor` is the CI gate that keeps them honest.
    /// Offline — no daemon, no repo.
    Commands {
        #[command(subcommand)]
        cmd: CommandsCmd,
    },
    // ── V70-A10: `kb-code workspace …` — appended at the END of `Cmd`
    // per the unit brief (siblings are appending their own new top-level
    // verbs too; this keeps every concurrent addition non-conflicting). ──
    /// `kb-code workspace list|show|save|open|note|export` — V70-A10
    /// ("Workspaces v0", kb-code v7 "The Continuum"): a named, ordered set
    /// of open files + a captured desk snapshot + a ref, with notes. Rides
    /// the SAME `GET`/`POST`/`PATCH`/`DELETE /api/sets[...]` wire surface
    /// `kb-code set` uses (`kind=workspace`,
    /// `kb_code_server::reading_sets`'s "Workspaces" doc section) plus
    /// `POST`/`GET /api/annotations` (`set_id`). Daemon-only.
    Workspace {
        #[command(subcommand)]
        cmd: WorkspaceCmd,
    },
    // ── V70-A8 (D20 CLI hygiene) — the self-description surface ──────────
    /// `kb-code tools [--json]` — clap-tree walk manifest of every verb
    /// this binary knows about (recon `cli-agent-surface.md` open question
    /// 1). Offline — no daemon involved. `--json` adds a best-effort
    /// mutation-likelihood heuristic per verb; see `tools.rs`'s module doc
    /// for why it is a heuristic, never a fact.
    Tools {
        #[arg(long)]
        json: bool,
    },
    /// `kb-code token path` — print the resolved bearer-token FILE path
    /// (never the token itself — see `token.rs`'s module doc for why there
    /// is no `--token` flag and no `token show`).
    Token {
        #[command(subcommand)]
        cmd: TokenCmd,
    },
    /// `kb-code schema list|show <name> [--json-schema] [--example]` —
    /// `GET /api/schemas`/`GET /api/schemas/{name}` (D20's schemars self-
    /// description surface — see `kb_code_server::api_schemas`'s module
    /// doc for the exact (starter) set of names and why it isn't every
    /// wire shape this daemon serves).
    Schema {
        #[command(subcommand)]
        cmd: SchemaCmd,
    },
    /// `kb-code doctor [--agent] [--json]` — daemon-reachability +
    /// protocol/schema-epoch skew report (compares this binary's own
    /// `kb_core::sibling` constants + `kb_code_server::store::schema_epoch`
    /// against what `GET /api/identity` reports), token-resolution status
    /// (never the token itself), and whether the CURRENT working directory
    /// falls inside one of the daemon's configured repos (the ONE cwd→repo
    /// resolution this unit ships — see `resolve_repo_for_path`'s doc for
    /// why the three pre-existing shell/TS reimplementations of this exact
    /// algorithm are NOT replatformed onto it in this unit). `--agent`
    /// requests the terser, script-friendly rendering D20 names
    /// (`doctor --agent`); plain `doctor` prints the same checks at
    /// human-readable length. Never refuses a mutating verb on skew (D20's
    /// fuller "refusal only for mutating verbs" ask is cut — see this
    /// unit's handoff note) — it is a diagnostic, not a gate.
    Doctor {
        #[arg(long)]
        agent: bool,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    // ── V70-A8 (D20) — nine verb-less routes named in recon
    // `cli-agent-surface.md` open question 7 (all nine wired in this unit):
    // `diff`, `commit`, `file-history`, `range-diff`, `scopes`, `doc-refs`
    // below, plus `review impact` (`ReviewCmd::Impact`), `review findings
    // recurrence` (`ReviewFindingsCmd::Recurrence`), and `pr reviews`
    // (`PrCmd::Reviews`) — see those enums for the other three. ──────────
    /// `kb-code diff --repo R --path P --from REF [--to REF] [--json]` —
    /// `GET /api/diff` (W4.2; no CLI coverage before V70-A8). `--to`
    /// omitted diffs `--from` against the current working tree.
    Diff {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        path: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code commit SHA --repo R [--json]` — `GET /api/commit`.
    Commit {
        sha: String,
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code file-history PATH --repo R [--limit N] [--before UNIX]
    /// [--json]` — `GET /api/file-history`.
    FileHistory {
        path: String,
        #[arg(long)]
        repo: String,
        #[arg(long)]
        limit: Option<usize>,
        /// Unix seconds — commits authored after this are excluded.
        #[arg(long)]
        before: Option<i64>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code range-diff --repo R --old RANGE --new RANGE [--json]` —
    /// `GET /api/range-diff`.
    RangeDiff {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        old: String,
        #[arg(long)]
        new: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code scopes [--json]` — `GET /api/scopes`: the configured
    /// `[scopes]` map (name → glob patterns).
    Scopes {
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code doc-refs --repo R --path P [--json]` — `GET /api/doc-refs`
    /// (DCB): which kb documents' extracted code references currently
    /// resolve to this path.
    DocRefs {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        path: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    // ── V71-X1 (A8's cuts + one test fix) — appended at the END of `Cmd`
    // per the standing convention (concurrent sibling units also append
    // their own new top-level verbs; this keeps every addition disjoint). ──
    /// `kb-code watch <lane>... [--since UNIX] [--repo R] [--review N]
    /// [--backlog] [--once] [--timeout SECS] [--ignore-author X]...
    /// [--interval SECS] --json` — ONE unified NDJSON stream fanning out
    /// over the existing per-lane watchers, run CONCURRENTLY in this one
    /// process: `annotate` (the SSE `annotate watch` loop) and `inbox`
    /// (the poll `inbox --watch` loop). Each lane DELEGATES to the SAME
    /// pure seed/diff/format functions its OWN standalone command uses
    /// (`watch.rs`/`inbox.rs`) — this verb adds no new item-shape
    /// knowledge, only the multi-lane fan-out, a `"lane"` tag spliced
    /// flat onto every line, and the NEW `--since` seed rule: an item
    /// last-updated at or after `--since` surfaces on the very FIRST
    /// read of each lane (annotate: the SSE seed; inbox: the first
    /// poll), as if `--backlog` were scoped to just that window — "catch
    /// me up since I last watched," rather than "dump everything open."
    /// `--json` is REQUIRED (not optional the way it is on every other
    /// verb): two lanes print concurrently to the SAME stdout, and only
    /// the one-JSON-object-per-line shape both lanes already use in
    /// `--json` mode is safe to interleave — human mode's multi-line
    /// rendering is not, so this refuses rather than risk scrambled
    /// output. Exits once every requested lane's own loop exits (Ctrl-C/
    /// SIGTERM stop all lanes; `--once`/`--timeout` apply per lane).
    Watch {
        /// One or more of: annotate, inbox.
        #[arg(required = true)]
        lanes: Vec<String>,
        /// Only surface an item whose OWN timestamp is at or after this
        /// unix second on the first read of each lane. Every lane's
        /// later live update still surfaces regardless of this bound.
        #[arg(long)]
        since: Option<i64>,
        /// `annotate` lane scope — same meaning as `annotate watch`'s own
        /// `--repo`. Required (with/without `--review`) if `annotate` is
        /// one of `lanes`.
        #[arg(long)]
        repo: Option<String>,
        /// `annotate` lane scope — same meaning as `annotate watch`'s own
        /// `--review`.
        #[arg(long)]
        review: Option<i64>,
        /// Also surface each lane's initial open set once, in ADDITION to
        /// `--since` — same meaning as `annotate watch --backlog`.
        #[arg(long)]
        backlog: bool,
        #[arg(long)]
        once: bool,
        #[arg(long)]
        timeout: Option<u64>,
        /// `annotate` lane only — same meaning as `annotate watch
        /// --ignore-author`.
        #[arg(long = "ignore-author")]
        ignore_author: Vec<String>,
        /// `inbox` lane poll interval — same meaning as `inbox --watch
        /// --interval`.
        #[arg(long, default_value_t = 30)]
        interval: u64,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    // ── V71-G0 (kb-code v7.1 "Understanding") ────────────────────────────
    /// `kb-code entity <NAME> --repo R [--worktree W] [--json]` —
    /// `GET /api/entity` (`entities/1`): every definition site of one Ruby
    /// class or module, with a per-site trust class
    /// (`exact`/`likely`/`candidate`, computed per request — see
    /// `kb_code_server::entities::class_for`).
    ///
    /// `NAME` is a constant path (`Order`, `Reseller::Order`). A bare last
    /// segment is resolved across the whole repo and, when more than one
    /// constant answers to it, every one of them is listed — nothing is
    /// ever merged on a bare name. A member address (`Foo#bar`, `Foo.bar`)
    /// is refused BY NAME rather than 404'd: members are the entity page's
    /// own later unit.
    Entity {
        name: String,
        #[arg(long)]
        repo: String,
        /// Restrict to one checkout's rows (`entity_defs.worktree`).
        #[arg(long)]
        worktree: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code brief [--repo NAME] [--json]` — design doc D18/D23: "the
    /// agent's session-start read as `kb-code brief --json`" (also framed
    /// there as `kb-code weather --since`'s sibling); A8 cut this for
    /// scope. **v0**: resolves the caller's repo the SAME way `kb-code
    /// doctor` does (`--repo` wins; else the longest configured repo
    /// path containing `cwd`; else the sole configured repo), then
    /// composes ONE existing read (`GET /api/inbox`, `unified-inbox/1`)
    /// into a repo-scoped landing summary — reviews awaiting you and
    /// open annotations (by intent) IN THIS REPO, plus the kb lane's own
    /// availability verbatim (not repo-scoped — memory has no repo
    /// concept). This is deliberately NOT the design's full "weather
    /// report" (a deterministic diff against a claim ledger, D18's
    /// `kbc-claim/1`, itself unbuilt) — see this unit's handoff note for
    /// why that is out of scope here. No new route: every field below is
    /// filtered/re-counted client-side from `/api/inbox`'s existing
    /// response.
    Brief {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code seq list --repo R [--projection P] [--workspace ID]
    /// [--json]` — `GET /api/seq` (kbc-seq/1): every sequence projection
    /// (set · workspace · tour · trail · board) in one repo, resolved over
    /// the tables that still own them. A READ layer — every projection is
    /// still created and edited through its own family's verbs.
    Seq {
        #[command(subcommand)]
        cmd: SeqCmd,
    },
    // ── V71-E2 ────────────────────────────────────────────────────
    /// `kb-code act --list TARGET --repo R [--json]` — `GET /api/actions`
    /// (`kbc-actions/1`, D5): the SAME typed action list the SPA's menu
    /// renders, for the same resolved target. The point of the mirror is
    /// that when the operator says "I right-clicked that and asked", the
    /// agent can reproduce the identical menu — and that an agent does not
    /// have to memorise forty verbs, because `act --list` TELLS it what is
    /// possible at a location.
    ///
    /// `TARGET` is `PATH`, `PATH:LINE`, `PATH:LINE:COL` or `PATH:A-B`
    /// (a line range).
    ///
    /// `kb-code act <ID> --at TARGET` RESOLVES one row: it prints the typed
    /// operation and the `kb-code` command line that performs it, and for a
    /// row backed by a daemon READ it performs that read and prints the
    /// response. It is deliberately a RESOLVER, not an executor: every op in
    /// `kbc-actions/1` is a CLIENT operation (a navigation, a dock, a
    /// composer), so "running" one here would mean either re-implementing
    /// nine verbs inside this one or spawning a subprocess — and the
    /// daemon-never-spawns rule plus one-home-for-one-action both point the
    /// other way. A MUTATING row still refuses without `--confirm`, at the
    /// surface the operator actually types, and an ORDINAL is refused by
    /// name: ids are stable strings, and `act 2` must never mean "whatever
    /// is second today".
    Act {
        /// The stable action id to resolve. Omit it with `--list`.
        id: Option<String>,
        /// List every action for this target.
        #[arg(long, value_name = "TARGET")]
        list: Option<String>,
        /// The target to resolve ID against.
        #[arg(long, value_name = "TARGET")]
        at: Option<String>,
        #[arg(long)]
        repo: String,
        /// Revspec to read the file at.
        #[arg(long = "ref")]
        rev: Option<String>,
        /// The selected text, when the target is a selection — what makes
        /// the `text`/`path` targets resolvable.
        #[arg(long)]
        text: Option<String>,
        /// Which resolved target to build the list for (the segmented
        /// control's index; default 0).
        #[arg(long = "target-index")]
        target_index: Option<usize>,
        /// Required before a mutating row is resolved.
        #[arg(long)]
        confirm: bool,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code scope <sub>` — V71-F1, kbc-scope/1: the named path-set
    /// algebra the tree filter, `--scope` and (later) every other surface
    /// resolve through. Scopes have TWO sources with stated precedence;
    /// this milestone ships the read half of one of them — `[scopes]` in
    /// `kb-code.toml` (`source: config`, read-only seeds). Saved scopes in
    /// sqlite (`source: saved`) are NOT here: see this unit's handoff for
    /// why a migration was cut, and note that `scope from-paths` prints
    /// the TOML block to paste, which is a proposal you can read before it
    /// becomes state.
    Scope {
        #[command(subcommand)]
        cmd: ScopeCmd,
    },
}

/// `kb-code scope <sub>` — V71-F1.
#[derive(Subcommand, Debug)]
enum ScopeCmd {
    /// Every configured scope, with the number of files it resolves to
    /// TODAY. A scope whose count is 0 is flagged: "a scope that quietly
    /// matches nothing is worse than a broken link".
    List {
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// Resolve one scope NAME (`$`-less) or a full expression, and print
    /// its count — optionally every path it selects.
    Show {
        /// A configured scope name, or a kbc-scope/1 expression.
        name_or_expr: String,
        #[arg(long)]
        repo: String,
        /// Print every resolved path (one per line) after the summary.
        #[arg(long)]
        paths: bool,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// Propose the smallest expressions covering a SELECTION, each with
    /// the "+N files you did not select" delta resolved against the real
    /// repo. Nothing is saved: the output is the expression and the
    /// `[scopes]` TOML block to paste.
    FromPaths {
        /// The selected repo-relative paths.
        #[arg(required = true)]
        paths: Vec<String>,
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// Derive scopes for free from what the repo already declares, and
    /// print them as a `[scopes]` block. `packwerk` reads every
    /// `package.yml` in the index; `codeowners` reads the repo's own
    /// CODEOWNERS through `GET /api/file` (GitHub's lookup order, its
    /// last-match-wins semantics).
    Import {
        /// `packwerk` or `codeowners`.
        source: String,
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
}

/// `kb-code seq <sub>` — V71-G0.
#[derive(Subcommand, Debug)]
enum SeqCmd {
    /// List sequence projections.
    List {
        #[arg(long)]
        repo: String,
        /// One of `set|workspace|tour|trail|board` (plurals and `canvas`
        /// accepted as documented aliases). Omitted lists every one.
        #[arg(long)]
        projection: Option<String>,
        /// A workspace's set id — lists only the projections bound to it.
        #[arg(long)]
        workspace: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
}

/// `kb-code commands <sub>` — V70-A5.
#[derive(Subcommand, Debug)]
enum CommandsCmd {
    /// Every registered command, by scope. `--json` prints the registry
    /// verbatim (the same bytes `GET /api/commands` serves); `--md` prints a
    /// table per scope.
    Manifest {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        md: bool,
        /// Which preset column to show: `vim` (default), `plain`, `helix`.
        #[arg(long, default_value = "vim")]
        preset: String,
    },
    /// One scope's keys, printable — the `?` sheet's twin. Both render the
    /// same rows in the same registry order, so the printed card and the
    /// on-screen one cannot drift.
    Cheatsheet {
        /// `global`, `reader`, `diff`, `tree`, `review`, `branches`, `board`,
        /// `rail`, `drawer`, `palette`.
        #[arg(long, default_value = "reader")]
        scope: String,
        #[arg(long)]
        md: bool,
        #[arg(long, default_value = "vim")]
        preset: String,
    },
    /// The generated collision report: one key bound to two different
    /// commands in two coactive scopes at the same modal depth. A deeper
    /// scope shadowing a shallower one is the design and is NOT listed.
    Conflicts {
        #[arg(long)]
        json: bool,
    },
    /// Resolve a key the way the SPA's dispatcher would — the answer to "why
    /// did that key do that". Same algorithm, same registry.
    ///
    ///   kb-code commands explain 'g d' --scope reader
    ///   kb-code commands explain Escape --scope diff --context diff.menu
    Explain {
        /// A key SEQUENCE: `?`, `g d`, `Ctrl-w v`, `Space g h`, `Escape`.
        key: String,
        #[arg(long, default_value = "reader")]
        scope: String,
        /// `k=v` pairs, comma-separated. A bare key means `=true`.
        #[arg(long)]
        context: Option<String>,
        #[arg(long, default_value = "vim")]
        preset: String,
        #[arg(long)]
        json: bool,
    },
    /// The CI gate (also run as a `#[test]` in this crate, so `cargo test -p
    /// kb-code-cli` is the check): the registry parses, ids are unique, every
    /// shipped row's CLI twin resolves in THIS binary's clap tree or is an
    /// honest `none:<reason>`, no reserved browser chord is claimed, no
    /// global key is re-declared narrower, every Esc row carries a distinct
    /// dismiss order and never navigates, and every conflict is ratified.
    Doctor {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
enum TokenCmd {
    Path,
}

#[derive(Subcommand, Debug)]
enum SchemaCmd {
    /// `kb-code schema list [--json]` — `GET /api/schemas`.
    List {
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code schema show NAME [--json-schema] [--example] [--json]` —
    /// `GET /api/schemas/{name}`. With neither `--json-schema` nor
    /// `--example`, prints both; each flag alone narrows to just that
    /// part (`--json` wraps the full response in the `--json` envelope
    /// instead — mutually meaningful with either narrowing flag).
    Show {
        name: String,
        #[arg(long = "json-schema")]
        json_schema: bool,
        #[arg(long)]
        example: bool,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
}

/// `kb-code doclens <sub>` — DCB W1.C.
#[derive(Subcommand, Debug)]
enum DoclensCmd {
    /// Resolved refs for one doc against ONE checkout. `--repo` is required
    /// unless the doc is pinned; a checkout is NEVER auto-selected.
    Show {
        #[arg(long)]
        kb: String,
        /// kb's ARTIFACT ID (not a source-relative path).
        #[arg(long)]
        doc: String,
        #[arg(long)]
        repo: Option<String>,
        /// Filter rows client-side by group key.
        #[arg(long)]
        group: Option<String>,
        /// Filter rows client-side: present|ambiguous|absent|external.
        #[arg(long)]
        state: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// Run one doc_refs reverse-index sync pass NOW
    /// (`POST /api/doc-lens/sync`, DCB W3.A) and print what it did.
    ///
    /// Takes no `--kb`/`--doc`: the sync SET is "every kb with at least one
    /// pin", computed daemon-side — a caller-supplied kb list would be a
    /// second, drifting definition of the same thing. The route is
    /// LOOPBACK-ONLY, so this verb only reaches a daemon on this machine.
    Sync {
        /// Reset every synced kb's feed cursor first — a full corpus-side
        /// rescan. The escape hatch for "a repo-side rename happened and I
        /// want claims to catch up sooner than the doc's own next kb edit".
        #[arg(long)]
        force: bool,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// The checkout scorecard — every configured repo scored against this
    /// doc (`GET /api/doc-lens/repos`).
    Repos {
        #[arg(long)]
        kb: String,
        #[arg(long)]
        doc: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// Remember a checkout for this doc (`PUT /api/doc-lens/pin`).
    Pin {
        #[arg(long)]
        kb: String,
        #[arg(long)]
        doc: String,
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// List remembered checkouts (`GET /api/doc-lens/pins`). `--kb` is
    /// OPTIONAL: the question is "what have I pinned", which has no one doc.
    Pins {
        #[arg(long)]
        kb: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// Forget the remembered checkout for a doc
    /// (`DELETE /api/doc-lens/pin`). Idempotent — forgetting something that
    /// was never remembered SUCCEEDS, it just says so.
    Unpin {
        #[arg(long)]
        kb: String,
        #[arg(long)]
        doc: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
}

/// `kb-code stacks diff --branch <name> [--repo R]` — V3.3-S2.
#[derive(Subcommand, Debug)]
enum StacksCmd {
    /// Incremental numstat of one stack layer (`base_tip..branch_tip`).
    Diff {
        #[arg(long)]
        branch: String,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
}

/// `kb-code bookmark rm <ID-or-mnemonic> [--repo R]` — Phase N.
#[derive(Subcommand, Debug)]
enum BookmarkCmd {
    /// Delete by numeric id or by single-char mnemonic (mnemonic needs
    /// `--repo`).
    Rm {
        /// Numeric bookmark id, or a single-char mnemonic.
        id_or_mnemonic: String,
        #[arg(long)]
        repo: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
}

/// `kb-code annotations open …` — D3: the repo-wide `GET
/// /api/annotations/open` listing, nested under `annotations` the same way
/// `Cmd::Annotations`' own `path` positional coexists with this subcommand
/// (see `Cmd::Search`'s `query`-vs-`SearchCmd` precedent).
#[derive(Subcommand, Debug)]
enum AnnotationsCmd {
    /// `kb-code annotations open --repo R [--intent I] [--path-prefix P]`
    /// — D3: every UNRESOLVED, TOP-LEVEL annotation across the WHOLE repo
    /// (`GET /api/annotations/open`), verbatim — the D4 hook's future query
    /// surface, so `--json` output stays stable + complete (including
    /// `reply_count`/`truncated`).
    Open {
        #[arg(long)]
        repo: String,
        /// Exact `intent` match — one of note/question/todo/
        /// flag-for-agent/tour-stop.
        #[arg(long)]
        intent: Option<String>,
        /// Only annotations whose `path` starts with this prefix.
        #[arg(long = "path-prefix")]
        path_prefix: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
}

/// `kb-code annotate {reply,resolve,reopen,edit,set-intent,delete} …` — D3:
/// the thread + lifecycle verbs, each acting on an EXISTING annotation by
/// id. Coexists with `Cmd::Annotate`'s own `target`/`message`/… create-form
/// fields on the same variant (see that field's doc).
#[derive(Subcommand, Debug)]
enum AnnotateCmd {
    /// `kb-code annotate reply <ID> -m BODY --repo R --path P [--intent I]`
    /// — `POST /api/annotations` with `parent_id` set (one level of
    /// nesting only — the daemon 400s a reply-to-a-reply). Needs
    /// `--repo`/`--path` (unlike every other lifecycle verb below): a
    /// reply is still wire-shaped as a create, and `CreateAnnotationBody`'s
    /// `repo`/`path` are mandatory — the daemon validates them against the
    /// parent's own.
    Reply {
        /// The parent annotation's id.
        id: String,
        #[arg(short = 'm', long = "message")]
        message: String,
        #[arg(long)]
        repo: String,
        #[arg(long)]
        path: String,
        /// note (default) | question | todo | flag-for-agent | tour-stop.
        #[arg(long)]
        intent: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code annotate resolve <ID>` — `PATCH {resolved:true}`.
    Resolve {
        id: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code annotate reopen <ID>` — `PATCH {resolved:false}`.
    Reopen {
        id: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code annotate edit <ID> -m BODY` — `PATCH {body:BODY}`.
    Edit {
        id: String,
        #[arg(short = 'm', long = "message")]
        message: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code annotate set-intent <ID> <INTENT>` — `PATCH {intent:
    /// INTENT}`. `INTENT` is vocab-gated client-side (same failure the
    /// daemon would 400 on, caught before the round trip).
    SetIntent {
        id: String,
        /// note | question | todo | flag-for-agent | tour-stop.
        intent: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code annotate delete <ID> [--yes]` — `DELETE`. Cascades to any
    /// replies nested under `ID`. Prompts for confirmation unless `--yes`
    /// (mirrors `kb reset`'s own confirm convention).
    Delete {
        id: String,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code annotate batch [--file OPS.json] --repo R` — V4.P1:
    /// `POST /api/annotations/batch`. Reads a JSON array of ops (or
    /// `{"ops":[…]}`) from `--file`, or stdin when `--file` is omitted.
    /// Cap 100 ops; a server 400 is printed readably.
    Batch {
        /// Ops JSON file. Default: stdin.
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code annotate watch [--repo R] [--review N]` — V4.P1: SSE
    /// triage loop (modeled on `kb comments watch`). Seeds a seen-set
    /// from the current open set (no surface unless `--backlog`);
    /// subsequent `annotation.changed` / `suggestion.applied` /
    /// `review.changed{reason=verdict}` events refetch and print
    /// NEW/CHANGED items. Recommend `--ignore-author claude` so an
    /// agent loop does not re-triage its own writes.
    Watch {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        review: Option<i64>,
        /// Surface the initial open set once, then follow.
        #[arg(long)]
        backlog: bool,
        /// Exit 0 after the first surfaced item.
        #[arg(long)]
        once: bool,
        /// Exit 0 after this many idle seconds (resets on a surface).
        #[arg(long)]
        timeout: Option<u64>,
        /// Skip items whose author matches (repeatable). Recommend
        /// `--ignore-author claude`.
        #[arg(long = "ignore-author")]
        ignore_author: Vec<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
}

/// `kb-code suggest list|apply|drop` — V4.P1. Coexists with the PUT form
/// `kb-code suggest <ID> (-m TEXT | --from-file F)` on `Cmd::Suggest`.
#[derive(Subcommand, Debug)]
enum SuggestCmd {
    /// List comments that carry a suggestion. **Review-scoped only** —
    /// no repo-wide suggestion-listing route exists. Reads
    /// `GET /api/reviews/{N}/comments`.
    List {
        #[arg(long)]
        review: i64,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `POST /api/annotations/{id}/apply` — LOOPBACK-ONLY. Apply the
    /// stored suggestion to the working tree. `--resolve` also marks
    /// the annotation resolved. A 409 is rendered as an expected/found
    /// line-by-line diff. `already_applied` prints a note and exits 0.
    Apply {
        id: String,
        #[arg(long)]
        resolve: bool,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `DELETE /api/annotations/{id}/suggestion` — drop the stored
    /// suggestion without touching the working tree.
    Drop {
        id: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `POST /api/annotations/apply-batch` — LOOPBACK-ONLY (PRR-R10).
    /// Apply MANY stored suggestions in one atomic, multi-file batch.
    /// Two-phase: every id is verified first (existence, not-applied,
    /// anchor re-resolve, byte-exact original, same-file overlap) — ANY
    /// failure aborts the whole batch (409, nothing written) and this
    /// prints every id's verdict. `--resolve` also resolves each applied
    /// thread, mirroring the single `apply --resolve` checkbox.
    ApplyBatch {
        #[arg(required = true)]
        ids: Vec<String>,
        #[arg(long)]
        resolve: bool,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
}

/// `kb-code behavioral …` — V3.2-B1 history-counter bulk ops + V3.4-C1
/// request-time activity time-series.
#[derive(Subcommand, Debug)]
enum BehavioralCmd {
    /// Full-window rebuild of path/author/cochange counters
    /// (`POST /api/behavioral/backfill`). LOOPBACK-ONLY. Omit `--repo` to
    /// run every configured repo.
    Backfill {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code behavioral timeseries --repo R [--path P] [--weeks N]` —
    /// V3.4-C1 per-week activity buckets
    /// (`GET /api/behavioral/timeseries`). Attention signal only (commits /
    /// churn / authors) — never a health grade. Derived at request time.
    Timeseries {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        path: Option<String>,
        /// Lookback weeks (default 26, hard cap 104).
        #[arg(long)]
        weeks: Option<u32>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
}

/// `kb-code canvas list [--repo R]` — V3.4-C1 list canvas sets
/// (`GET /api/canvas`). **Read-only verb.**
///
/// Create/edit/delete of canvas sets are SPA interaction chrome with **no
/// CLI verb parity** (D7 recorded exemption) — operators list what exists
/// here; the SPA owns layout mutations over the loopback-gated HTTP API.
#[derive(Subcommand, Debug)]
enum CanvasCmd {
    /// List canvas sets (`GET /api/canvas?repo=`). Requires `--repo`.
    List {
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
enum HookCmd {
    /// Print the settings.json snippet (+ the plugin-install alternative)
    /// to wire up `kb-code-why.sh`. Never writes anything.
    Install,
    /// Print the settings.json snippet to remove (mirror of `install`).
    /// Never writes anything.
    Uninstall,
    /// Check whether the manual-install hook script is present and
    /// whether a kb-code daemon is reachable. Does NOT parse
    /// `~/.claude/settings.json` or plugin state — there's no single
    /// canonical location once plugins are in play.
    Status {
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
}

/// `kb-code set …` — Phase E3. `NAME-OR-ID` (everywhere it appears below)
/// resolves against `GET /api/sets?repo=`: an EXACT name match first, else
/// a unique id PREFIX match (`resolve_set_id`'s doc).
#[derive(Subcommand, Debug)]
enum SetCmd {
    /// `kb-code set list --repo R` — `GET /api/sets?repo=`.
    List {
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code set show <NAME-OR-ID> --repo R` — `GET /api/sets/{id}`.
    Show {
        name_or_id: String,
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code set create <NAME> --repo R [-d DESC] [--span
    /// PATH[:START[-END]]]...` — `POST /api/sets`. `--span` is repeatable;
    /// each one is `PATH` (whole file), `PATH:LINE` (one line), or
    /// `PATH:START-END` (a range) — see `parse_span_arg`.
    Create {
        name: String,
        #[arg(long)]
        repo: String,
        #[arg(short = 'd', long = "description")]
        description: Option<String>,
        #[arg(long = "span")]
        spans: Vec<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code set add <NAME-OR-ID> PATH[:START[-END]] [--note N] [--ref
    /// REV]` — `POST /api/sets/{id}/spans` (appends after the current last
    /// ordinal).
    Add {
        name_or_id: String,
        span: String,
        #[arg(long)]
        note: Option<String>,
        #[arg(long = "ref")]
        git_ref: Option<String>,
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code set rm <NAME-OR-ID> <ORDINAL>` — remove one span by its
    /// `ordinal` (a client-driven full-replacement `PATCH`: read the
    /// current spans, drop the one at `ORDINAL`, send the rest back).
    Rm {
        name_or_id: String,
        ordinal: usize,
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code set delete <NAME-OR-ID> [--yes]` — `DELETE /api/sets/{id}`
    /// (cascades to spans). Prompts for confirmation unless `--yes`.
    Delete {
        name_or_id: String,
        #[arg(long)]
        repo: String,
        #[arg(long)]
        yes: bool,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code set from-session <SESSION_ID> --repo R [--name N]` —
    /// `POST /api/sets/from-session`: materializes a whole-file reading set
    /// from a session's touched files (committed + uncommitted evidence,
    /// deduped, first-touch order). LOOPBACK-ONLY (same gate as
    /// `session-diff`/`checkout`).
    FromSession {
        session_id: String,
        #[arg(long)]
        repo: String,
        /// Default: `"session: <the session's first prompt, truncated>"`.
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code set from-doc <KB> <DOC> --repo R [--name N]` — `POST
    /// /api/sets/from-doc`: materializes a kb document's CURRENTLY RESOLVED
    /// code references (`path_state == "present"` only, group-ordered) into
    /// a reading set (DCB-W3.C/R23). LOOPBACK-ONLY (same gate as
    /// `from-session`/`checkout`).
    FromDoc {
        kb: String,
        doc: String,
        #[arg(long)]
        repo: String,
        /// Default: `"<doc title or doc id> · <minute-precision timestamp>"`.
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
enum TranscriptsCmd {
    /// Files tracked, turns indexed, and total indexed bytes
    /// (`GET /api/transcripts/status`).
    Status {
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
}

/// `kb-code review <subcommand>` — V3.R1 local review sessions.
#[derive(Subcommand, Debug)]
enum ReviewCmd {
    /// Start a review on HEAD_REF (creates ps1). `--repo` required.
    Start {
        head_ref: String,
        #[arg(long)]
        repo: String,
        #[arg(long = "base")]
        base: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long = "session")]
        session: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// List reviews for a repo (`GET /api/reviews?repo=`).
    List {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        state: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// Show one review + patchset list.
    Show {
        id: i64,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// Capture a new patchset (explicit snapshot).
    Snapshot {
        id: i64,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// Files changed in a patchset (`--ps N` or latest).
    Files {
        id: i64,
        #[arg(long)]
        ps: Option<i64>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// Interdiff between two patchsets.
    Interdiff {
        id: i64,
        #[arg(long)]
        from: i64,
        #[arg(long)]
        to: i64,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// Mark a path viewed (or `--unset` to clear).
    Viewed {
        id: i64,
        path: String,
        #[arg(long)]
        unset: bool,
        /// Blob sha at the time of viewing (required unless `--unset`).
        #[arg(long = "blob")]
        blob_sha: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// Close a review (stops auto-capture; keeps history).
    Close {
        id: i64,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// GC oldest patchsets down to `[review] max_patchsets`.
    Gc {
        #[arg(long = "review")]
        review: Option<i64>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// V3.2-B2 — review-risk attention composite (`GET /api/reviews/{id}/risk`).
    /// One column per term, score last. Ranks attention, not quality.
    Risk {
        id: i64,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// V70-A8 (D20, PRR-F) — per-changed-file blast-radius chips
    /// (`GET /api/reviews/{id}/impact`; no CLI coverage before this unit —
    /// recon `cli-agent-surface.md` open question 7). `--path` is REQUIRED:
    /// the route's `ReviewImpactParams.path` is a bare `String`, so an
    /// omitted query 400s before the handler runs. It must name a file in
    /// the review's LATEST patchset change set.
    Impact {
        id: i64,
        /// Repo-relative path of a file changed by this review.
        #[arg(long)]
        path: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// V3.3-S1 — review map: change-set dependency skeleton
    /// (`GET /api/reviews/{id}/map`). Nodes + edges among changed files.
    Map {
        id: i64,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// V3.3-S1 — deterministic reading order / tour
    /// (`GET /api/reviews/{id}/reading-order`). Numbered stops, tests last.
    Order {
        id: i64,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code review comments <ID> [--ps N] [--all]` — V4.P1:
    /// `GET /api/reviews/{id}/comments` (`review-comments/1`). Grouped
    /// by path.
    Comments {
        id: i64,
        /// Patchset number, or `latest` (default).
        #[arg(long)]
        ps: Option<String>,
        /// Include resolved threads.
        #[arg(long)]
        all: bool,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code review verdict <ID> <approve|request-changes|comment>
    /// [-m NOTE]` / `kb-code review verdict <ID> --clear` — V4.P1:
    /// `PUT`/`DELETE /api/reviews/{id}/verdict`. LOOPBACK-ONLY.
    Verdict {
        id: i64,
        /// `approve`, `request-changes`, or `comment`. Omit with `--clear`.
        state: Option<String>,
        #[arg(short = 'm', long = "message")]
        note: Option<String>,
        /// Clear the verdict (`DELETE`).
        #[arg(long)]
        clear: bool,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code review distill <ID> [--json]` — CT-E7: `GET
    /// /api/reviews/{id}/distill` (`review-distill/1`). One deterministic
    /// JSON dump of the review's full local record (meta, patchsets,
    /// files, verdict, every comment thread, suggestions incl. applied
    /// audit) — a pure read, re-distilling the same review at the same
    /// patchset state yields the same document. kb-code never writes to
    /// kb: if the review is worth keeping, pipe `--json` into `kb notes
    /// new` / `kb remember`, citing this review's id + head sha —
    /// deciding that is an AGENT-layer judgment call, never this CLI's.
    Distill {
        id: i64,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code review start-pr --repo R --pr N [--base][--title]
    /// [--session]` — PRR-R2: `POST /api/reviews/pr` (design doc §2 row 1).
    /// LOOPBACK-ONLY. Fetches `refs/pull/N/head` into `refs/kbc/pr/N`
    /// (load-bearing — 400 on failure), creates the review + captures ps1,
    /// and best-effort-enriches with GitHub PR metadata (degrades honestly
    /// on any GitHub-side failure — the review is created either way).
    StartPr {
        #[arg(long)]
        repo: String,
        #[arg(long = "pr")]
        pr_number: u32,
        #[arg(long = "base")]
        base: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long = "session")]
        session: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code review report ID [--json]` — PRR-R2: `GET
    /// /api/reviews/{id}/report` (design doc §2 row 4).
    /// `kb-code review report ID --set --from-file FILE [--json]` — `PUT
    /// /api/reviews/{id}/report` (design doc §2 row 5). LOOPBACK-ONLY.
    /// Wholesale-replaces the report; `generated_at` is server-stamped.
    Report {
        id: i64,
        /// PUT instead of GET — requires `--from-file`.
        #[arg(long)]
        set: bool,
        #[arg(long = "from-file")]
        from_file: Option<PathBuf>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code review artifact ID [--json]` — PRR-R2: `GET
    /// /api/reviews/{id}/artifact` (design doc §2 row 7). Live,
    /// UNPERSISTED verification of the review's kb artifact hint (set via
    /// `kb-code review set-artifact`, below, or kb's own raw `PATCH
    /// /api/reviews/{id}`).
    Artifact {
        id: i64,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code review set-artifact ID KB DOC_ID [--json]` — V70-A3X: `PATCH
    /// /api/reviews/{id}` with `artifact_hint_kb`/`artifact_hint_id`
    /// (`reviews::PatchReviewBody`'s own doc — the two fields are written
    /// TOGETHER as a pair). Never resolved/verified here (kb-code has no
    /// business validating a kb doc id against a schema it doesn't own) —
    /// `kb-code review artifact ID` is the live, unpersisted verification
    /// step this hint feeds.
    SetArtifact {
        id: i64,
        /// The kb corpus name (`kb.toml`'s `[kb.<name>]`).
        kb: String,
        /// The artifact's doc id within `kb`.
        doc_id: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code review findings import|list|add ...` — PRR-R3: the
    /// findings surface (design doc §2 rows 8-9; addendum §E). See
    /// [`ReviewFindingsCmd`] for the three sub-verbs (each carries its own
    /// `ID` positional, AFTER the sub-verb name — `review findings import
    /// ID …`, matching the milestone plan's own grammar; clap's
    /// positional-then-subcommand ordering means `id` cannot live on THIS
    /// outer variant, only on each inner one); `import`/`add` are
    /// LOOPBACK-ONLY.
    Findings {
        #[command(subcommand)]
        cmd: ReviewFindingsCmd,
    },
    /// `kb-code review disposition ID SLUG
    /// {agree,dispute,waive,fix-later,clear} [-m NOTE] [--json]` — PRR-R3:
    /// `PUT`/`DELETE /api/reviews/{id}/findings/{slug}/disposition`
    /// (design doc §2 rows 10-11). LOOPBACK-ONLY.
    Disposition {
        id: i64,
        slug: String,
        /// `agree` | `dispute` | `waive` | `fix-later` | `clear`.
        action: String,
        #[arg(short = 'm', long = "message")]
        note: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code review pr-status ID [--json]` — PRR-R4: `GET
    /// /api/reviews/{id}/pr-status` (design doc §2 row 12). The LOCAL half
    /// (snapshot vs local patchset tip) always answers; the LIVE half
    /// (fresh GitHub fetch + `commits_behind`) degrades to
    /// `unavailable_reason` on any GitHub-side failure.
    PrStatus {
        id: i64,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code review inbox [--repo R|--all-repos] [--state open]
    /// [--limit N] [--json]` — PRR-R4: `GET /api/reviews/inbox` (design doc
    /// §2 row 13). Cross-repo attention queue; `--repo`/`--all-repos` are
    /// mutually exclusive and exactly one is required.
    Inbox {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long = "all-repos")]
        all_repos: bool,
        /// `open` (default) | `closed` | `all`.
        #[arg(long)]
        state: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code review timeline ID [--json]` — PRR-R4: `GET
    /// /api/reviews/{id}/timeline` (milestone plan arbitration #7). Pure
    /// composition of existing rows, ascending by `at`.
    Timeline {
        id: i64,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code review export-github ID [--finding SLUG]... [--include-waived]
    /// [--include-orphaned-as-general] [--json]` — PRR-R5: `GET
    /// /api/reviews/{id}/export/github` (design doc §2 row 14 / §3.2). Pure
    /// computation, zero GitHub calls — hands back a ready-to-`gh` payload.
    /// The human-output tail prints a LOUD warning when `stale_export` is
    /// true, recommending `review pr-status` + a re-`snapshot` before
    /// publishing.
    ExportGithub {
        id: i64,
        /// Narrow the export to exactly these finding slugs (repeatable).
        /// Omit for every non-superseded, non-waived, unpublished finding.
        #[arg(long = "finding")]
        finding: Vec<String>,
        #[arg(long)]
        include_waived: bool,
        #[arg(long)]
        include_orphaned_as_general: bool,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code review publish ID SLUG --url URL [--comment-id ID] [--json]`
    /// — PRR-R5: `POST /api/reviews/{id}/findings/{slug}/published`.
    /// `kb-code review publish ID --verdict --url URL [--review-id ID]
    /// [--json]` — `POST /api/reviews/{id}/verdict/published`. Design doc §2
    /// rows 15-16. LOOPBACK-ONLY. Advisory-only recording, called AFTER the
    /// agent's own `gh` call succeeds — kb-code never touches GitHub's write
    /// API itself.
    Publish {
        id: i64,
        /// Finding slug — omit only when `--verdict` is set.
        slug: Option<String>,
        #[arg(long)]
        verdict: bool,
        #[arg(long)]
        url: String,
        /// The GitHub review-comment id (finding form only).
        #[arg(long = "comment-id")]
        comment_id: Option<String>,
        /// The GitHub review id (`--verdict` form only).
        #[arg(long = "review-id")]
        review_id: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code review github-threads ID [--json]` — PRR-R7
    /// (design-addendum-2.md §A): `GET /api/reviews/{id}/github-threads`.
    /// The GitHub PR's own review conversation, position-mapped onto the
    /// review's LATEST patchset via the same carry-forward ladder; GitHub
    /// stays the source of truth (nothing persisted). `400` when the review
    /// isn't PR-bound; any live GitHub failure degrades to
    /// `unavailable_reason` (HTTP 200, `threads: []`).
    GithubThreads {
        id: i64,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code review sweep [--repo R | --all-repos] [--include-closed]
    /// [--json]` — PRR-R8: `POST /api/reviews/sweep` (design-addendum-2
    /// §B). LOOPBACK-ONLY. Walks every PR-bound review (default
    /// `state=open`) and reconciles it against live GitHub — the cron/
    /// agent entry point for "every PR the LLM touched." `--repo`/
    /// `--all-repos` are mutually exclusive; exactly one is required (same
    /// guard as `review inbox`).
    Sweep {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long = "all-repos")]
        all_repos: bool,
        #[arg(long = "include-closed")]
        include_closed: bool,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code review analytics [--repo R] [--from UNIX] [--to UNIX]
    /// [--json]` — PRR-R9: `GET /api/reviews/analytics` (design-addendum-2
    /// §C). The disposition calibration instrument — severity×disposition
    /// matrix, acceptance rates, weekly buckets, latency, recurrence.
    /// Never a quality verdict.
    Analytics {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        from: Option<i64>,
        #[arg(long)]
        to: Option<i64>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code review compose ID {--from-file FILE|--stdin} [--json]` —
    /// V70-R: `POST /api/reviews/{id}/compose` (design doc D9 scoped to
    /// v0). LOOPBACK-ONLY. The one-shot authoring call: a `kbc-compose/1`
    /// body — `summary` (required prose), an optional `risk_score`/
    /// `verdict_headline`/`verdict_body`/`stats`/`verdict`/`verdict_note`,
    /// and `findings` (the SAME `kbc-findings/1` object `review findings
    /// import` accepts, nested under that key) — writes findings + report +
    /// an optional review-level verdict in ONE sqlite transaction,
    /// rather than three separate `findings import` / `report --set` /
    /// `verdict` calls each with its own commit.
    Compose {
        id: i64,
        #[arg(long = "from-file")]
        from_file: Option<PathBuf>,
        #[arg(long)]
        stdin: bool,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
}

/// PRR-R3 — `kb-code review findings <SUBCOMMAND> ID …`.
#[derive(Subcommand, Debug)]
enum ReviewFindingsCmd {
    /// `import ID {--from-file FILE|--stdin} [--mode full|additive]
    /// [--json]` — `POST /api/reviews/{id}/findings/import`.
    /// LOOPBACK-ONLY.
    Import {
        id: i64,
        #[arg(long = "from-file")]
        from_file: Option<PathBuf>,
        #[arg(long)]
        stdin: bool,
        /// `full` (default) | `additive`.
        #[arg(long)]
        mode: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `list ID [--ps N|latest][--disposition D][--all] [--json]` — `GET
    /// /api/reviews/{id}/findings`.
    List {
        id: i64,
        #[arg(long)]
        ps: Option<String>,
        #[arg(long)]
        disposition: Option<String>,
        /// Include superseded (tombstoned) findings.
        #[arg(long)]
        all: bool,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `add ID --severity S --category C --path P
    /// {--line N|--lines A-B|--whole-file} -m TITLE --rationale R
    /// [--recommendation ...][--slug ...][--evidence FILE
    /// [--evidence-lang LANG]] [--json]` — `POST /api/reviews/{id}/findings`.
    /// LOOPBACK-ONLY. `--evidence` reads FILE's bytes as the finding's
    /// `evidence.source` (the same `FindingEvidenceBody{lang, source}`
    /// shape `findings import` already sends); `--evidence-lang` tags it
    /// (e.g. `ruby`) and requires `--evidence` (rejected standalone —
    /// nothing to tag).
    // V70-H1 — boxed (`Box<ReviewFindingsAddArgs>`): this variant's ~15
    // fields (id/severity/category/path/line/lines/whole_file/removed/
    // title/rationale/recommendation/slug/evidence/evidence_lang/daemon/
    // json) sized the WHOLE enum to its width (clippy::large_enum_variant
    // — every other variant, even `List`, paid for `Add`'s bytes). A tuple
    // variant with one `Args`-deriving field is standard clap (`impl<T:
    // Args> Args for Box<T>` — clap_builder's derive.rs), so parsing is
    // unchanged; call sites now match `Add(args)` and read `args.id` etc.
    Add(Box<ReviewFindingsAddArgs>),
    /// V70-A8 (D20, PRR-F) — which of this review's own findings recur
    /// across other reviews (`GET /api/reviews/{id}/findings/recurrence`;
    /// no CLI coverage before this unit — recon `cli-agent-surface.md`
    /// open question 7).
    Recurrence {
        id: i64,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
}

/// `kb-code review findings add ID --severity S --category C --path P
/// {--line N|--lines A-B|--whole-file} -m TITLE --rationale R
/// [--recommendation ...][--slug ...][--evidence FILE
/// [--evidence-lang LANG]] [--json]` — `POST /api/reviews/{id}/findings`.
/// LOOPBACK-ONLY. Boxed out of [`ReviewFindingsCmd::Add`] to fix
/// clippy::large_enum_variant (V70-H1) — see that variant's doc.
#[derive(Args, Debug)]
pub(crate) struct ReviewFindingsAddArgs {
    pub id: i64,
    #[arg(long)]
    pub severity: String,
    #[arg(long)]
    pub category: String,
    #[arg(long)]
    pub path: String,
    #[arg(long)]
    pub line: Option<i64>,
    /// `A-B` (inclusive).
    #[arg(long)]
    pub lines: Option<String>,
    #[arg(long = "whole-file")]
    pub whole_file: bool,
    /// The cited line/file was DELETED by this diff (anchors against
    /// the `old` side).
    #[arg(long)]
    pub removed: bool,
    #[arg(short = 'm', long = "message")]
    pub title: String,
    #[arg(long)]
    pub rationale: String,
    #[arg(long)]
    pub recommendation: Option<String>,
    #[arg(long)]
    pub slug: Option<String>,
    /// A file whose bytes become `evidence.source` — a code snippet
    /// substantiating the finding.
    #[arg(long)]
    pub evidence: Option<PathBuf>,
    /// Language tag for `--evidence`'s content (e.g. `ruby`, `rust`).
    /// Requires `--evidence`.
    #[arg(long = "evidence-lang", requires = "evidence")]
    pub evidence_lang: Option<String>,
    #[arg(long, default_value = "http://127.0.0.1:4747")]
    pub daemon: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Subcommand, Debug)]
enum PrCmd {
    /// `kb-code pr list --repo R [--json]` — `GET /api/prs` (existing
    /// route; zero CLI coverage before PRR-R2).
    List {
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code pr show N --repo R [--json]` — PRR-R2: `GET /api/prs/{n}`
    /// (design doc §2 row 2). Works for open/closed/merged.
    Show {
        number: u64,
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code pr checks N --repo R [--json]` — PRR-R2: `GET
    /// /api/prs/{n}/checks` (design doc §2 row 3).
    Checks {
        number: u64,
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code pr comments N --repo R [--json]` — `GET
    /// /api/prs/{n}/comments` (existing route; zero CLI coverage before
    /// PRR-R2).
    Comments {
        number: u64,
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code pr fetch N --repo R [--json]` — `POST /api/prs/fetch`
    /// (existing route; zero CLI coverage before PRR-R2). LOOPBACK-ONLY.
    Fetch {
        number: u32,
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// V70-A8 (D20) — reviewer decisions + review-decision summary
    /// (`GET /api/prs/{n}/reviews`; no CLI coverage before this unit —
    /// recon `cli-agent-surface.md` open question 7). NOT to be confused
    /// with `kb-code review` (this crate's own local review sessions).
    Reviews {
        number: u64,
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
enum ScipCmd {
    /// Parse INDEX (a `.scip` protobuf file) and POST its mapped
    /// occurrences to the daemon in batches (see `Cmd::Scip`'s own doc for
    /// how to generate one).
    Ingest {
        /// Path to the `.scip` index file.
        index: PathBuf,
        /// The daemon's configured repo NAME this index was generated
        /// against.
        #[arg(long)]
        repo: String,
        /// Documents per `POST /api/scip/ingest` call — batched so a huge
        /// index (a monorepo's worth of documents) doesn't become one
        /// unbounded request body.
        #[arg(long, default_value_t = 200)]
        batch_size: usize,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// PRR-N12 (N2) — `kb-code scip run --repo NAME | --all [--dry-run]
    /// [--timeout-secs N]`: read each target repo's configured `[[scip.
    /// repos]]` argv + working-tree path off `GET /api/repos` (kb-code-
    /// server, N1), spawn it (`std::process::Command`, argv array, never a
    /// shell string, `current_dir` = the repo's working tree), and on a
    /// zero exit chain straight into the SAME `scip ingest` code path
    /// (`scip_ingest_cmd`) against `<repo_path>/<output>`. One command
    /// replaces "remember the exact `rust-analyzer scip .` invocation, then
    /// remember to run `scip ingest` after." A per-repo failure (indexer
    /// exits non-zero, times out, or the chained ingest fails) is reported
    /// and never aborts the rest of an `--all` batch.
    Run {
        /// Run for exactly this configured repo NAME. Mutually exclusive
        /// with `--all`.
        #[arg(long)]
        repo: Option<String>,
        /// Run for every repo that has a `[[scip.repos]]` entry configured
        /// on the daemon (`GET /api/repos`'s `scip.configured`). Mutually
        /// exclusive with `--repo`.
        #[arg(long)]
        all: bool,
        /// Print the argv (+ working directory) that WOULD run, for every
        /// target repo, without spawning anything or touching the daemon's
        /// occurrence store.
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Wall-clock cap on the configured indexer subprocess, per repo.
        #[arg(long = "timeout-secs", default_value_t = 600)]
        timeout_secs: u64,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
enum SearchCmd {
    /// Fuzzy-match file paths, blended with open-history frecency. An
    /// empty QUERY returns the most recently opened files instead.
    Files {
        /// Fuzzy needle. Pass "" (or omit and rely on the shell) for the
        /// "recent files" fallback.
        #[arg(default_value = "")]
        query: String,
        /// Scope to one configured repo (omit to search every repo).
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// Fuzzy-match symbol names (name + container context, e.g.
    /// "GitRepo::open"), joined to currently-live files.
    Symbols {
        query: String,
        /// Scope to one configured repo (omit to search every repo).
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// Literal (default) or regex text search over a repo's working tree.
    Text {
        query: String,
        /// Required — text search reads one repo's working-tree files.
        #[arg(long)]
        repo: String,
        /// Treat QUERY as a regex instead of a literal string.
        #[arg(long)]
        regex: bool,
        /// Case-sensitive match (default: case-insensitive).
        #[arg(long)]
        case: bool,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// W2.3 — semantic (embedding-based) code search. Daemon-only; 400s if
    /// `[semantic]` is off, or off for the requested `--repo`
    /// (`GET /api/search/semantic`).
    Semantic {
        query: String,
        /// Scope to one configured repo (omit to search every enabled repo).
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// FTS5 full-text search over local Claude Code transcript JSONL
    /// (W2.5, `GET /api/search/transcripts`) — LOOPBACK-ONLY, see
    /// `Cmd::Transcripts`'s doc.
    Transcripts {
        /// FTS5 MATCH query (sqlite's own boolean/prefix/NEAR syntax).
        query: String,
        /// Scope to one session id.
        #[arg(long)]
        session: Option<String>,
        /// Scope to one turn kind: user|assistant|thinking|tool_use|tool_result.
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
}

/// V70-A8 (D20) — split out of `main()` so the latter can compute a
/// meaningful process exit code (`envelope::exit_code_for`) from whatever
/// `Result` comes back, rather than the default `Termination` impl's
/// hardcoded 1 for every failure shape. `Cli::parse()` stays in `main()`
/// itself (a `--help`/usage failure exits via clap's own `std::process::
/// exit(2)` before `run` would ever be called — see `envelope::
/// EXIT_USAGE`'s doc).
async fn run(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::Identity { daemon, json } => identity(&daemon, json).await,
        Cmd::Audit {
            since,
            limit,
            daemon,
            json,
        } => audit_cmd(&daemon, since.as_deref(), limit, json).await,
        Cmd::Refs { repo } => refs(&repo),
        Cmd::Tree {
            path,
            repo,
            rev,
            json,
            daemon,
            view,
            scope,
            filter,
            mode,
            decorate,
            base,
            review,
            depth,
            limit,
            format,
        } => {
            // V71-F1 — ANY kbc-tree/1 flag routes to the projected wire;
            // with none of them the verb is byte-identical to before.
            let projected = view.is_some()
                || scope.is_some()
                || filter.is_some()
                || mode.is_some()
                || decorate.is_some()
                || base.is_some()
                || review.is_some()
                || depth.is_some()
                || limit.is_some()
                || format.is_some();
            if projected {
                let Some(daemon) = daemon.as_deref() else {
                    anyhow::bail!(
                        "the projected tree is computed by the daemon (one projection, two \
                         renderers) — pass --daemon <url>; the offline `kb-code tree` is the \
                         per-directory listing only"
                    );
                };
                let opts = TreeV2Opts {
                    repo: &repo,
                    view: view.as_deref(),
                    root: path.as_deref(),
                    scope: scope.as_deref(),
                    filter: filter.as_deref(),
                    mode: mode.as_deref(),
                    decorate: decorate.as_deref(),
                    base: base.as_deref(),
                    review,
                    depth: Some(depth.unwrap_or(0)),
                    expand: None,
                    limit,
                };
                let fmt = format
                    .as_deref()
                    .unwrap_or(if json { "json" } else { "tree" });
                tree_v2_cmd(daemon, &opts, fmt).await
            } else {
                match daemon {
                    Some(base) => {
                        tree_daemon(&base, &repo, path.as_deref().unwrap_or(""), &rev, json).await
                    }
                    None => tree(Path::new(&repo), path.as_deref().unwrap_or(""), &rev, json),
                }
            }
        }
        Cmd::Cat {
            path,
            repo,
            rev,
            daemon,
        } => match daemon {
            Some(base) => cat_daemon(&base, &repo, &path, &rev).await,
            None => cat(Path::new(&repo), &path, &rev),
        },
        Cmd::Repos { daemon, json } => repos_cmd(&daemon, json).await,
        Cmd::Syntax { daemon, json } => syntax_cmd(&daemon, json).await,
        Cmd::Parity { daemon, json } => parity_cmd(&daemon, json).await,
        Cmd::Symbols {
            path,
            repo,
            rev,
            query,
            daemon,
            json,
        } => {
            symbols_cmd(
                &daemon,
                &repo,
                path.as_deref(),
                &rev,
                query.as_deref(),
                json,
            )
            .await
        }
        Cmd::Events {
            daemon,
            follow: _follow,
            json,
        } => events_cmd(&daemon, json).await,
        Cmd::Search {
            query,
            repo,
            limit,
            daemon,
            json,
            explain,
            budget,
            count_only,
            facets,
            group,
            cmd,
        } => match cmd {
            Some(SearchCmd::Files {
                query,
                repo,
                limit,
                daemon,
                json,
            }) => search_files_cmd(&daemon, repo.as_deref(), &query, limit, json).await,
            Some(SearchCmd::Symbols {
                query,
                repo,
                limit,
                daemon,
                json,
            }) => search_symbols_cmd(&daemon, repo.as_deref(), &query, limit, json).await,
            Some(SearchCmd::Text {
                query,
                repo,
                regex,
                case,
                daemon,
                json,
            }) => search_text_cmd(&daemon, &repo, &query, regex, case, json).await,
            Some(SearchCmd::Semantic {
                query,
                repo,
                limit,
                daemon,
                json,
            }) => search_semantic_cmd(&daemon, repo.as_deref(), &query, limit, json).await,
            Some(SearchCmd::Transcripts {
                query,
                session,
                kind,
                limit,
                daemon,
                json,
            }) => {
                search_transcripts_cmd(
                    &daemon,
                    &query,
                    session.as_deref(),
                    kind.as_deref(),
                    limit,
                    json,
                )
                .await
            }
            None => match query {
                Some(q) => {
                    search_unified_cmd(
                        &daemon,
                        repo.as_deref(),
                        &q,
                        limit,
                        json,
                        SearchOutputOpts {
                            explain,
                            budget,
                            count_only,
                            facets,
                            group,
                        },
                    )
                    .await
                }
                None => anyhow::bail!(
                    "kb-code search: pass a QUERY for the unified Search-Everywhere box \
                     (`kb-code search <q>`), or a lane subcommand \
                     (files|symbols|text|semantic|transcripts)"
                ),
            },
        },
        Cmd::Transcripts { cmd } => match cmd {
            TranscriptsCmd::Status { daemon, json } => transcripts_status_cmd(&daemon, json).await,
        },
        Cmd::Blame {
            path,
            repo,
            rev,
            lines,
            daemon,
            json,
        } => {
            let line_range = lines.as_deref().map(parse_line_range_arg).transpose()?;
            blame_cmd(&daemon, &repo, &path, rev.as_deref(), line_range, json).await
        }
        Cmd::Timeline {
            target,
            repo,
            max,
            daemon,
            json,
        } => {
            let (path, line_s) = target
                .rsplit_once(':')
                .with_context(|| format!("expected PATH:LINE, got {target:?}"))?;
            let line: u32 = line_s
                .parse()
                .with_context(|| format!("invalid line number in {target:?}"))?;
            timeline_cmd(&daemon, &repo, path, line, max, json).await
        }
        Cmd::Join {
            sha,
            repo,
            daemon,
            json,
        } => join_cmd(&daemon, &repo, &sha, json).await,
        Cmd::Why {
            target,
            repo,
            daemon,
            json,
        } => {
            let (path, line) = parse_path_line(&target);
            why_cmd(&daemon, &repo, &path, line, json).await
        }
        Cmd::Story {
            path,
            repo,
            symbol,
            daemon,
            json,
        } => story_cmd(&daemon, &repo, &path, symbol.as_deref(), json).await,
        Cmd::ProvenanceReport {
            repo,
            max_count,
            daemon,
            json,
        } => provenance_report_cmd(&daemon, &repo, max_count, json).await,
        Cmd::SessionDiff {
            session,
            repo,
            daemon,
            json,
        } => session_diff_cmd(&daemon, &session, repo.as_deref(), json).await,
        Cmd::Backfill { repo, daemon, json } => backfill_cmd(&daemon, repo.as_deref(), json).await,
        Cmd::Annotations {
            path,
            repo,
            all,
            daemon,
            json,
            cmd,
        } => match cmd {
            Some(AnnotationsCmd::Open {
                repo,
                intent,
                path_prefix,
                daemon,
                json,
            }) => {
                annotations_open_cmd(
                    &daemon,
                    &repo,
                    intent.as_deref(),
                    path_prefix.as_deref(),
                    json,
                )
                .await
            }
            None => {
                let path = path.ok_or_else(|| {
                    anyhow::anyhow!(
                        "kb-code annotations: pass a PATH, or `open` for the repo-wide listing"
                    )
                })?;
                let repo =
                    repo.ok_or_else(|| anyhow::anyhow!("kb-code annotations: --repo is required"))?;
                annotations_cmd(&daemon, &repo, &path, all, json).await
            }
        },
        Cmd::Annotate {
            target,
            message,
            to,
            symbol,
            sha,
            intent,
            review,
            ps,
            side,
            repo,
            daemon,
            json,
            cmd,
        } => match cmd {
            Some(AnnotateCmd::Reply {
                id,
                message,
                repo,
                path,
                intent,
                daemon,
                json,
            }) => {
                annotate_reply_cmd(
                    &daemon,
                    &id,
                    &repo,
                    &path,
                    &message,
                    intent.as_deref(),
                    json,
                )
                .await
            }
            Some(AnnotateCmd::Resolve { id, daemon, json }) => {
                annotate_resolve_cmd(&daemon, &id, json).await
            }
            Some(AnnotateCmd::Reopen { id, daemon, json }) => {
                annotate_reopen_cmd(&daemon, &id, json).await
            }
            Some(AnnotateCmd::Edit {
                id,
                message,
                daemon,
                json,
            }) => annotate_edit_cmd(&daemon, &id, &message, json).await,
            Some(AnnotateCmd::SetIntent {
                id,
                intent,
                daemon,
                json,
            }) => annotate_set_intent_cmd(&daemon, &id, &intent, json).await,
            Some(AnnotateCmd::Delete {
                id,
                yes,
                daemon,
                json,
            }) => annotate_delete_cmd(&daemon, &id, yes, json).await,
            Some(AnnotateCmd::Batch {
                file,
                repo,
                daemon,
                json,
            }) => annotate_batch_cmd(&daemon, &repo, file.as_deref(), json).await,
            Some(AnnotateCmd::Watch {
                repo,
                review,
                backlog,
                once,
                timeout,
                ignore_author,
                daemon,
                json,
            }) => {
                let scope = watch::WatchScope::new(repo, review)?;
                watch::run(watch::WatchArgs {
                    daemon,
                    scope,
                    json,
                    once,
                    timeout_secs: timeout,
                    backlog,
                    ignore_author,
                    // V71-X1 — `--since`/lane tagging are `kb-code watch`'s
                    // own additions; this standalone verb's surface and
                    // output are unchanged.
                    since: None,
                    lane_tag: None,
                })
                .await
            }
            None => {
                let target = target.ok_or_else(|| {
                    anyhow::anyhow!(
                        "kb-code annotate: pass PATH:LINE, or a lifecycle subcommand \
                         (reply|resolve|reopen|edit|set-intent|delete|batch|watch)"
                    )
                })?;
                let message = message
                    .ok_or_else(|| anyhow::anyhow!("kb-code annotate: -m/--message is required"))?;
                let repo =
                    repo.ok_or_else(|| anyhow::anyhow!("kb-code annotate: --repo is required"))?;
                let (path, line_s) = target
                    .rsplit_once(':')
                    .with_context(|| format!("expected PATH:LINE, got {target:?}"))?;
                let line: u32 = line_s
                    .parse()
                    .with_context(|| format!("invalid line number in {target:?}"))?;
                if let Some(s) = side.as_deref() {
                    if s != "new" && s != "old" {
                        anyhow::bail!("kb-code annotate: --side must be `new` or `old`, got {s:?}");
                    }
                }
                annotate_create_cmd(
                    &daemon,
                    &repo,
                    path,
                    line,
                    &message,
                    to,
                    symbol,
                    sha.as_deref(),
                    intent.as_deref(),
                    review,
                    ps,
                    side.as_deref(),
                    json,
                )
                .await
            }
        },
        Cmd::Checkout {
            target,
            repo,
            daemon,
            json,
        } => checkout_cmd(&daemon, &repo, &target, json).await,
        Cmd::Map {
            dir,
            repo,
            budget,
            daemon,
            json,
        } => map_cmd(&daemon, &repo, dir.as_deref().unwrap_or(""), budget, json).await,
        Cmd::Pack {
            paths,
            set,
            repo,
            budget,
            daemon,
            json,
        } => pack_cmd(&daemon, &repo, &paths, set.as_deref(), budget, json).await,
        Cmd::Defs {
            symbol,
            repo,
            limit,
            daemon,
            json,
        } => defs_cmd(&daemon, repo.as_deref(), &symbol, limit, json).await,
        Cmd::Xrefs {
            symbol,
            repo,
            limit,
            daemon,
            json,
        } => xrefs_cmd(&daemon, &repo, &symbol, limit, json).await,
        Cmd::Similar {
            target,
            repo,
            limit,
            daemon,
            json,
        } => {
            let (path, start, end) = parse_similar_target(&target)?;
            similar_cmd(&daemon, &repo, &path, start, end, limit, json).await
        }
        Cmd::Impact {
            path,
            repo,
            limit,
            daemon,
            json,
        } => {
            // V3.1-H2: PATH:LINE:COL → compositional analysis; bare PATH →
            // legacy co-change neighborhood.
            if let Ok((p, line, col)) = parse_resolve_target(&path) {
                impact_analysis_cmd(&daemon, &repo, &p, line, col, limit, json).await
            } else {
                impact_cmd(&daemon, &repo, &path, limit, json).await
            }
        }
        Cmd::Lenses {
            path,
            repo,
            rev,
            daemon,
            json,
        } => lenses_cmd(&daemon, &repo, &path, rev.as_deref(), json).await,
        Cmd::Hotspots {
            repo,
            limit,
            scope,
            daemon,
            json,
        } => hotspots_cmd(&daemon, &repo, limit, scope.as_deref(), json).await,
        Cmd::Coupling {
            path,
            repo,
            limit,
            daemon,
            json,
        } => coupling_cmd(&daemon, &repo, &path, limit, json).await,
        Cmd::Owners {
            path,
            repo,
            daemon,
            json,
        } => owners_cmd(&daemon, &repo, &path, json).await,
        Cmd::Age {
            path,
            repo,
            daemon,
            json,
        } => age_cmd(&daemon, &repo, &path, json).await,
        Cmd::Behavioral { cmd } => match cmd {
            BehavioralCmd::Backfill { repo, daemon, json } => {
                behavioral_backfill_cmd(&daemon, repo.as_deref(), json).await
            }
            BehavioralCmd::Timeseries {
                repo,
                path,
                weeks,
                daemon,
                json,
            } => behavioral_timeseries_cmd(&daemon, &repo, path.as_deref(), weeks, json).await,
        },
        Cmd::Canvas { cmd } => match cmd {
            CanvasCmd::List { repo, daemon, json } => canvas_list_cmd(&daemon, &repo, json).await,
        },
        Cmd::Doclens { cmd } => match cmd {
            DoclensCmd::Show {
                kb,
                doc,
                repo,
                group,
                state,
                daemon,
                json,
            } => {
                doclens_show_cmd(
                    &daemon,
                    &kb,
                    &doc,
                    repo.as_deref(),
                    group.as_deref(),
                    state.as_deref(),
                    json,
                )
                .await
            }
            DoclensCmd::Sync {
                force,
                daemon,
                json,
            } => doclens_sync_cmd(&daemon, force, json).await,
            DoclensCmd::Repos {
                kb,
                doc,
                daemon,
                json,
            } => doclens_repos_cmd(&daemon, &kb, &doc, json).await,
            DoclensCmd::Pin {
                kb,
                doc,
                repo,
                daemon,
                json,
            } => doclens_pin_cmd(&daemon, &kb, &doc, &repo, json).await,
            DoclensCmd::Pins { kb, daemon, json } => {
                doclens_pins_cmd(&daemon, kb.as_deref(), json).await
            }
            DoclensCmd::Unpin {
                kb,
                doc,
                daemon,
                json,
            } => doclens_unpin_cmd(&daemon, &kb, &doc, json).await,
        },
        Cmd::Recipes { daemon, json } => recipes_catalog_cmd(&daemon, json).await,
        Cmd::Recipe {
            name,
            repo,
            since,
            limit,
            scope,
            daemon,
            json,
        } => {
            recipe_run_cmd(
                &daemon,
                &name,
                &repo,
                since.as_deref(),
                limit,
                scope.as_deref(),
                json,
            )
            .await
        }
        Cmd::Hook { cmd } => match cmd {
            HookCmd::Install => {
                hook_install();
                Ok(())
            }
            HookCmd::Uninstall => {
                hook_uninstall();
                Ok(())
            }
            HookCmd::Status { daemon, json } => hook_status(&daemon, json).await,
        },
        Cmd::BenchSearch {
            queries,
            repo,
            limit,
            daemon,
            json,
        } => bench_search_cmd(&daemon, repo.as_deref(), &queries, limit, json).await,
        Cmd::Resolve {
            target,
            repo,
            rev,
            limit,
            daemon,
            json,
        } => {
            let (path, line, col) = parse_resolve_target(&target)?;
            resolve_cmd(
                &daemon,
                &repo,
                &path,
                line,
                col,
                rev.as_deref(),
                limit,
                json,
            )
            .await
        }
        Cmd::Review { cmd } => match cmd {
            ReviewCmd::Start {
                head_ref,
                repo,
                base,
                title,
                session,
                daemon,
                json,
            } => {
                review_start_cmd(
                    &daemon,
                    &repo,
                    &head_ref,
                    base.as_deref(),
                    title.as_deref(),
                    session.as_deref(),
                    json,
                )
                .await
            }
            ReviewCmd::List {
                repo,
                state,
                daemon,
                json,
            } => review_list_cmd(&daemon, &repo, state.as_deref(), json).await,
            ReviewCmd::Show { id, daemon, json } => review_show_cmd(&daemon, id, json).await,
            ReviewCmd::Snapshot { id, daemon, json } => {
                review_snapshot_cmd(&daemon, id, json).await
            }
            ReviewCmd::Files {
                id,
                ps,
                daemon,
                json,
            } => review_files_cmd(&daemon, id, ps, json).await,
            ReviewCmd::Interdiff {
                id,
                from,
                to,
                daemon,
                json,
            } => review_interdiff_cmd(&daemon, id, from, to, json).await,
            ReviewCmd::Viewed {
                id,
                path,
                unset,
                blob_sha,
                daemon,
                json,
            } => review_viewed_cmd(&daemon, id, &path, unset, blob_sha.as_deref(), json).await,
            ReviewCmd::Close { id, daemon, json } => review_close_cmd(&daemon, id, json).await,
            ReviewCmd::Gc {
                review,
                daemon,
                json,
            } => review_gc_cmd(&daemon, review, json).await,
            ReviewCmd::Risk { id, daemon, json } => review_risk_cmd(&daemon, id, json).await,
            ReviewCmd::Impact {
                id,
                path,
                daemon,
                json,
            } => review_impact_cmd(&daemon, id, &path, json).await,
            ReviewCmd::Map { id, daemon, json } => review_map_cmd(&daemon, id, json).await,
            ReviewCmd::Order { id, daemon, json } => review_order_cmd(&daemon, id, json).await,
            ReviewCmd::Comments {
                id,
                ps,
                all,
                daemon,
                json,
            } => review_comments_cmd(&daemon, id, ps.as_deref(), all, json).await,
            ReviewCmd::Verdict {
                id,
                state,
                note,
                clear,
                daemon,
                json,
            } => {
                review_verdict_cmd(&daemon, id, state.as_deref(), note.as_deref(), clear, json)
                    .await
            }
            ReviewCmd::Distill { id, daemon, json } => review_distill_cmd(&daemon, id, json).await,
            ReviewCmd::StartPr {
                repo,
                pr_number,
                base,
                title,
                session,
                daemon,
                json,
            } => {
                review_start_pr_cmd(
                    &daemon,
                    &repo,
                    pr_number,
                    base.as_deref(),
                    title.as_deref(),
                    session.as_deref(),
                    json,
                )
                .await
            }
            ReviewCmd::Report {
                id,
                set,
                from_file,
                daemon,
                json,
            } => review_report_cmd(&daemon, id, set, from_file.as_deref(), json).await,
            ReviewCmd::Artifact { id, daemon, json } => {
                review_artifact_cmd(&daemon, id, json).await
            }
            ReviewCmd::SetArtifact {
                id,
                kb,
                doc_id,
                daemon,
                json,
            } => review_set_artifact_cmd(&daemon, id, &kb, &doc_id, json).await,
            ReviewCmd::Findings { cmd } => match cmd {
                ReviewFindingsCmd::Import {
                    id,
                    from_file,
                    stdin,
                    mode,
                    daemon,
                    json,
                } => {
                    review_findings_import_cmd(
                        &daemon,
                        id,
                        from_file.as_deref(),
                        stdin,
                        mode.as_deref(),
                        json,
                    )
                    .await
                }
                ReviewFindingsCmd::List {
                    id,
                    ps,
                    disposition,
                    all,
                    daemon,
                    json,
                } => {
                    review_findings_list_cmd(
                        &daemon,
                        id,
                        ps.as_deref(),
                        disposition.as_deref(),
                        all,
                        json,
                    )
                    .await
                }
                ReviewFindingsCmd::Add(args) => {
                    let ReviewFindingsAddArgs {
                        id,
                        severity,
                        category,
                        path,
                        line,
                        lines,
                        whole_file,
                        removed,
                        title,
                        rationale,
                        recommendation,
                        slug,
                        evidence,
                        evidence_lang,
                        daemon,
                        json,
                    } = *args;
                    review_findings_add_cmd(
                        &daemon,
                        id,
                        &severity,
                        &category,
                        &path,
                        line,
                        lines.as_deref(),
                        whole_file,
                        removed,
                        &title,
                        &rationale,
                        recommendation.as_deref(),
                        slug.as_deref(),
                        evidence.as_deref(),
                        evidence_lang.as_deref(),
                        json,
                    )
                    .await
                }
                ReviewFindingsCmd::Recurrence { id, daemon, json } => {
                    review_findings_recurrence_cmd(&daemon, id, json).await
                }
            },
            ReviewCmd::Disposition {
                id,
                slug,
                action,
                note,
                daemon,
                json,
            } => review_disposition_cmd(&daemon, id, &slug, &action, note.as_deref(), json).await,
            ReviewCmd::ExportGithub {
                id,
                finding,
                include_waived,
                include_orphaned_as_general,
                daemon,
                json,
            } => {
                review_export_github_cmd(
                    &daemon,
                    id,
                    &finding,
                    include_waived,
                    include_orphaned_as_general,
                    json,
                )
                .await
            }
            ReviewCmd::Publish {
                id,
                slug,
                verdict,
                url,
                comment_id,
                review_id,
                daemon,
                json,
            } => {
                review_publish_cmd(
                    &daemon,
                    id,
                    slug.as_deref(),
                    verdict,
                    &url,
                    comment_id.as_deref(),
                    review_id.as_deref(),
                    json,
                )
                .await
            }
            ReviewCmd::PrStatus { id, daemon, json } => {
                review_pr_status_cmd(&daemon, id, json).await
            }
            ReviewCmd::Inbox {
                repo,
                all_repos,
                state,
                limit,
                daemon,
                json,
            } => {
                review_inbox_cmd(
                    &daemon,
                    repo.as_deref(),
                    all_repos,
                    state.as_deref(),
                    limit,
                    json,
                )
                .await
            }
            ReviewCmd::Timeline { id, daemon, json } => {
                review_timeline_cmd(&daemon, id, json).await
            }
            ReviewCmd::Sweep {
                repo,
                all_repos,
                include_closed,
                daemon,
                json,
            } => review_sweep_cmd(&daemon, repo.as_deref(), all_repos, include_closed, json).await,
            ReviewCmd::Analytics {
                repo,
                from,
                to,
                daemon,
                json,
            } => review_analytics_cmd(&daemon, repo.as_deref(), from, to, json).await,
            ReviewCmd::GithubThreads { id, daemon, json } => {
                review_github_threads_cmd(&daemon, id, json).await
            }
            ReviewCmd::Compose {
                id,
                from_file,
                stdin,
                daemon,
                json,
            } => review_compose_cmd(&daemon, id, from_file.as_deref(), stdin, json).await,
        },
        Cmd::Pr { cmd } => match cmd {
            PrCmd::List { repo, daemon, json } => pr_list_cmd(&daemon, &repo, json).await,
            PrCmd::Show {
                number,
                repo,
                daemon,
                json,
            } => pr_show_cmd(&daemon, &repo, number, json).await,
            PrCmd::Checks {
                number,
                repo,
                daemon,
                json,
            } => pr_checks_cmd(&daemon, &repo, number, json).await,
            PrCmd::Comments {
                number,
                repo,
                daemon,
                json,
            } => pr_comments_cmd(&daemon, &repo, number, json).await,
            PrCmd::Fetch {
                number,
                repo,
                daemon,
                json,
            } => pr_fetch_cmd(&daemon, &repo, number, json).await,
            PrCmd::Reviews {
                number,
                repo,
                daemon,
                json,
            } => pr_reviews_cmd(&daemon, &repo, number, json).await,
        },
        Cmd::Branches {
            repo,
            sort,
            daemon,
            json,
        } => branches_cmd(&daemon, &repo, &sort, json).await,
        Cmd::Compare {
            from,
            to,
            repo,
            three_dot,
            daemon,
            json,
        } => compare_cmd(&daemon, &repo, &from, &to, three_dot, json).await,
        Cmd::MergeCheck {
            to,
            from,
            repo,
            daemon,
            json,
        } => merge_check_cmd(&daemon, &repo, from.as_deref(), &to, json).await,
        Cmd::RepoState { repo, daemon, json } => repo_state_cmd(&daemon, &repo, json).await,
        Cmd::Suggest {
            id,
            message,
            from_file,
            daemon,
            json,
            cmd,
        } => match cmd {
            Some(SuggestCmd::List {
                review,
                daemon,
                json,
            }) => suggest_list_cmd(&daemon, review, json).await,
            Some(SuggestCmd::Apply {
                id,
                resolve,
                daemon,
                json,
            }) => suggest_apply_cmd(&daemon, &id, resolve, json).await,
            Some(SuggestCmd::Drop { id, daemon, json }) => {
                suggest_drop_cmd(&daemon, &id, json).await
            }
            Some(SuggestCmd::ApplyBatch {
                ids,
                resolve,
                daemon,
                json,
            }) => suggest_apply_batch_cmd(&daemon, &ids, resolve, json).await,
            None => {
                let id = id.ok_or_else(|| {
                    anyhow::anyhow!(
                        "kb-code suggest: pass an annotation ID with -m/--from-file, \
                         or a subcommand (list|apply|drop)"
                    )
                })?;
                suggest_put_cmd(&daemon, &id, message.as_deref(), from_file.as_deref(), json).await
            }
        },
        Cmd::Usages {
            target,
            repo,
            rev,
            limit,
            daemon,
            json,
            v2,
        } => {
            let (path, line, col) = parse_resolve_target(&target)?;
            usages_cmd(
                &daemon,
                &repo,
                &path,
                line,
                col,
                rev.as_deref(),
                limit,
                json,
                v2,
            )
            .await
        }
        // PRR-N5 — hover / framework-edges / resolve-symbol (see the
        // module doc's Cmd variants above).
        Cmd::Hover {
            target,
            repo,
            rev,
            daemon,
            json,
        } => {
            let (path, line, col) = parse_resolve_target(&target)?;
            hover_cmd(&daemon, &repo, &path, line, col, rev.as_deref(), json).await
        }
        // PRR-L2 (append-only match arm; see the Cmd::Diagnostics variant doc).
        Cmd::Diagnostics {
            path,
            repo,
            daemon,
            json,
        } => diagnostics_cmd(&daemon, &repo, &path, json).await,
        // S2-B1 (append-only match arm; see the Cmd::CodeActions variant doc).
        Cmd::CodeActions {
            target,
            end,
            repo,
            kinds,
            suggest,
            daemon,
            json,
        } => {
            let (path, start_line, start_col) = parse_code_actions_target(&target)?;
            let end_pos = end.as_deref().map(parse_code_actions_end).transpose()?;
            let kinds_vec: Option<Vec<String>> = kinds.as_deref().map(|s| {
                s.split(',')
                    .map(|k| k.trim().to_string())
                    .filter(|k| !k.is_empty())
                    .collect()
            });
            code_actions_cmd(
                &daemon,
                &repo,
                &path,
                start_line,
                start_col,
                end_pos,
                kinds_vec.as_deref(),
                suggest,
                json,
            )
            .await
        }
        Cmd::Framework {
            path,
            repo,
            kind,
            daemon,
            json,
        } => framework_cmd(&daemon, &repo, &path, kind.as_deref(), json).await,
        Cmd::ResolveSymbol {
            sym,
            repo,
            daemon,
            json,
        } => resolve_symbol_cmd(&daemon, &repo, &sym, json).await,
        Cmd::Callees {
            target,
            repo,
            rev,
            daemon,
            json,
        } => {
            let (path, line, col) = parse_resolve_target(&target)?;
            callees_cmd(&daemon, &repo, &path, line, col, rev.as_deref(), json).await
        }
        Cmd::Callers {
            target,
            repo,
            rev,
            daemon,
            json,
        } => {
            let (path, line, col) = parse_resolve_target(&target)?;
            callers_cmd(&daemon, &repo, &path, line, col, rev.as_deref(), json).await
        }
        Cmd::Implementors {
            name,
            repo,
            path,
            daemon,
            json,
        } => implementors_cmd(&daemon, &repo, &name, path.as_deref(), json).await,
        Cmd::Set { cmd } => match cmd {
            SetCmd::List { repo, daemon, json } => set_list_cmd(&daemon, &repo, json).await,
            SetCmd::Show {
                name_or_id,
                repo,
                daemon,
                json,
            } => set_show_cmd(&daemon, &repo, &name_or_id, json).await,
            SetCmd::Create {
                name,
                repo,
                description,
                spans,
                daemon,
                json,
            } => set_create_cmd(&daemon, &repo, &name, description.as_deref(), &spans, json).await,
            SetCmd::Add {
                name_or_id,
                span,
                note,
                git_ref,
                repo,
                daemon,
                json,
            } => {
                set_add_cmd(
                    &daemon,
                    &repo,
                    &name_or_id,
                    &span,
                    note.as_deref(),
                    git_ref.as_deref(),
                    json,
                )
                .await
            }
            SetCmd::Rm {
                name_or_id,
                ordinal,
                repo,
                daemon,
                json,
            } => set_rm_cmd(&daemon, &repo, &name_or_id, ordinal, json).await,
            SetCmd::Delete {
                name_or_id,
                repo,
                yes,
                daemon,
                json,
            } => set_delete_cmd(&daemon, &repo, &name_or_id, yes, json).await,
            SetCmd::FromSession {
                session_id,
                repo,
                name,
                daemon,
                json,
            } => set_from_session_cmd(&daemon, &repo, &session_id, name.as_deref(), json).await,
            SetCmd::FromDoc {
                kb,
                doc,
                repo,
                name,
                daemon,
                json,
            } => set_from_doc_cmd(&daemon, &repo, &kb, &doc, name.as_deref(), json).await,
        },
        Cmd::Bookmarks { repo, daemon, json } => {
            bookmarks_list_cmd(&daemon, repo.as_deref(), json).await
        }
        Cmd::Bookmark {
            target,
            repo,
            mnemonic,
            note,
            daemon,
            json,
            cmd,
        } => match cmd {
            Some(BookmarkCmd::Rm {
                id_or_mnemonic,
                repo: rm_repo,
                daemon: rm_daemon,
                json: rm_json,
            }) => {
                bookmark_rm_cmd(
                    &rm_daemon,
                    rm_repo.as_deref().or(repo.as_deref()),
                    &id_or_mnemonic,
                    rm_json,
                )
                .await
            }
            None => {
                let target = target.ok_or_else(|| {
                    anyhow::anyhow!("bookmark create needs PATH:LINE (or use `bookmark rm <id>`)")
                })?;
                let repo = repo.ok_or_else(|| {
                    anyhow::anyhow!("--repo is required when creating a bookmark")
                })?;
                bookmark_create_cmd(
                    &daemon,
                    &repo,
                    &target,
                    mnemonic.as_deref(),
                    note.as_deref(),
                    json,
                )
                .await
            }
        },
        Cmd::Todos {
            repo,
            marker,
            path_prefix,
            daemon,
            json,
        } => {
            todos_list_cmd(
                &daemon,
                repo.as_deref(),
                marker.as_deref(),
                path_prefix.as_deref(),
                json,
            )
            .await
        }
        Cmd::Scip { cmd } => match cmd {
            ScipCmd::Ingest {
                index,
                repo,
                batch_size,
                daemon,
                json,
            } => scip_ingest_cmd(&daemon, &repo, &index, batch_size, json).await,
            ScipCmd::Run {
                repo,
                all,
                dry_run,
                timeout_secs,
                daemon,
                json,
            } => scip_run_cmd(&daemon, repo.as_deref(), all, dry_run, timeout_secs, json).await,
        },
        Cmd::Stacks {
            repo,
            all,
            daemon,
            json,
            cmd,
        } => match cmd {
            Some(StacksCmd::Diff {
                branch,
                repo: diff_repo,
                daemon: diff_daemon,
                json: diff_json,
            }) => {
                let repo = diff_repo
                    .or(repo)
                    .ok_or_else(|| anyhow::anyhow!("--repo is required for stacks diff"))?;
                stacks_diff_cmd(&diff_daemon, &repo, &branch, diff_json).await
            }
            None => {
                let repo = repo.ok_or_else(|| anyhow::anyhow!("--repo is required for stacks"))?;
                stacks_list_cmd(&daemon, &repo, all, json).await
            }
        },
        // ── S2-B2: `kb-code inbox` dispatch ──
        Cmd::Inbox {
            daemon,
            json,
            watch,
            interval,
        } => {
            if watch {
                inbox::run_watch(&daemon, json, interval).await
            } else {
                inbox::run_once(&daemon, json).await
            }
        }
        // ── V70-A5: `kb-code commands …` ──
        Cmd::Commands { cmd } => {
            let reg = commands::load()?;
            match cmd {
                CommandsCmd::Manifest { json, md, preset } => {
                    commands::print_manifest(&reg, json, md, &preset)
                }
                CommandsCmd::Cheatsheet { scope, md, preset } => {
                    commands::print_cheatsheet(&reg, &scope, md, &preset)
                }
                CommandsCmd::Conflicts { json } => commands::print_conflicts(&reg, json),
                CommandsCmd::Explain {
                    key,
                    scope,
                    context,
                    preset,
                    json,
                } => {
                    let ctx = commands::parse_context(context.as_deref());
                    commands::print_explain(&reg, &key, &scope, &ctx, &preset, json)
                }
                // `Cli::command()` rather than a hand-written verb list: the
                // twin check has to cross-join against the REAL tree, or a
                // renamed verb would leave a stale string passing forever.
                CommandsCmd::Doctor { json } => {
                    commands::print_doctor(&reg, &<Cli as clap::CommandFactory>::command(), json)
                }
            }
        }
        // ── V70-A10: `kb-code workspace …` dispatch ──
        Cmd::Workspace { cmd } => match cmd {
            WorkspaceCmd::List {
                repo,
                group,
                daemon,
                json,
            } => workspace_list_cmd(&daemon, &repo, group.as_deref(), json).await,
            WorkspaceCmd::Show {
                name_or_id,
                repo,
                daemon,
                json,
            } => workspace_show_cmd(&daemon, &repo, &name_or_id, json).await,
            WorkspaceCmd::Save {
                repo,
                name,
                description,
                description_file,
                git_ref,
                desk_json,
                files,
                daemon,
                json,
            } => {
                workspace_save_cmd(
                    &daemon,
                    &repo,
                    &name,
                    description.as_deref(),
                    description_file.as_deref(),
                    git_ref.as_deref(),
                    desk_json.as_deref(),
                    &files,
                    json,
                )
                .await
            }
            WorkspaceCmd::Open {
                name_or_id,
                repo,
                print_url: _,
                daemon,
                json,
            } => workspace_open_cmd(&daemon, &repo, &name_or_id, json).await,
            WorkspaceCmd::Note { cmd } => match cmd {
                WorkspaceNoteCmd::Add {
                    name_or_id,
                    repo,
                    body,
                    at,
                    reply_to,
                    daemon,
                    json,
                } => {
                    workspace_note_add_cmd(
                        &daemon,
                        &repo,
                        &name_or_id,
                        &body,
                        at.as_deref(),
                        reply_to.as_deref(),
                        json,
                    )
                    .await
                }
            },
            WorkspaceCmd::Export {
                name_or_id,
                repo,
                md: _,
                daemon,
            } => workspace_export_cmd(&daemon, &repo, &name_or_id).await,
        },
        // ── V70-A8 (D20 CLI hygiene) ──────────────────────────────────
        Cmd::Tools { json } => tools::run(json),
        Cmd::Token { cmd } => match cmd {
            TokenCmd::Path => token::cmd_token_path(),
        },
        Cmd::Schema { cmd } => match cmd {
            SchemaCmd::List { daemon, json } => schema_list_cmd(&daemon, json).await,
            SchemaCmd::Show {
                name,
                daemon,
                json_schema,
                example,
                json,
            } => schema_show_cmd(&daemon, &name, json_schema, example, json).await,
        },
        Cmd::Doctor {
            agent,
            daemon,
            json,
        } => doctor_cmd(&daemon, agent, json).await,
        Cmd::Diff {
            repo,
            path,
            from,
            to,
            daemon,
            json,
        } => diff_cmd(&daemon, &repo, &path, &from, to.as_deref(), json).await,
        Cmd::Commit {
            sha,
            repo,
            daemon,
            json,
        } => commit_cmd(&daemon, &repo, &sha, json).await,
        Cmd::FileHistory {
            path,
            repo,
            limit,
            before,
            daemon,
            json,
        } => file_history_cmd(&daemon, &repo, &path, limit, before, json).await,
        Cmd::RangeDiff {
            repo,
            old,
            new,
            daemon,
            json,
        } => range_diff_cmd(&daemon, &repo, &old, &new, json).await,
        Cmd::Scopes { daemon, json } => scopes_cmd(&daemon, json).await,
        Cmd::DocRefs {
            repo,
            path,
            daemon,
            json,
        } => doc_refs_cmd(&daemon, &repo, &path, json).await,
        Cmd::Watch {
            lanes,
            since,
            repo,
            review,
            backlog,
            once,
            timeout,
            ignore_author,
            interval,
            daemon,
            json,
        } => {
            watch_unified_cmd(
                &lanes,
                since,
                repo,
                review,
                backlog,
                once,
                timeout,
                ignore_author,
                interval,
                &daemon,
                json,
            )
            .await
        }
        Cmd::Brief { repo, daemon, json } => brief_cmd(&daemon, repo.as_deref(), json).await,
        // ── V71-G0 ────────────────────────────────────────────────────
        Cmd::Entity {
            name,
            repo,
            worktree,
            daemon,
            json,
        } => entity_cmd(&daemon, &repo, &name, worktree.as_deref(), json).await,
        Cmd::Seq { cmd } => match cmd {
            SeqCmd::List {
                repo,
                projection,
                workspace,
                daemon,
                json,
            } => {
                seq_list_cmd(
                    &daemon,
                    &repo,
                    projection.as_deref(),
                    workspace.as_deref(),
                    json,
                )
                .await
            }
        },
        // ── V71-E2 ────────────────────────────────────────────────────
        Cmd::Act {
            id,
            list,
            at,
            repo,
            rev,
            text,
            target_index,
            confirm,
            daemon,
            json,
        } => {
            act_cmd(
                &daemon,
                &repo,
                id.as_deref(),
                list.as_deref(),
                at.as_deref(),
                rev.as_deref(),
                text.as_deref(),
                target_index,
                confirm,
                json,
            )
            .await
        }
        // V71-F1 — appended at the END of the dispatch, matching the END of
        // the enum.
        Cmd::Scope { cmd } => match cmd {
            ScopeCmd::List { repo, daemon, json } => scope_list_cmd(&daemon, &repo, json).await,
            ScopeCmd::Show {
                name_or_expr,
                repo,
                paths,
                daemon,
                json,
            } => scope_show_cmd(&daemon, &repo, &name_or_expr, paths, json).await,
            ScopeCmd::FromPaths {
                paths,
                repo,
                daemon,
                json,
            } => scope_from_paths_cmd(&daemon, &repo, &paths, json).await,
            ScopeCmd::Import {
                source,
                repo,
                daemon,
                json,
            } => scope_import_cmd(&daemon, &repo, &source, json).await,
        },
    }
}

/// V70-A8 (D20) — `main()` itself is now a thin wrapper: parse, run,
/// and on failure print the SAME `Error: {err:?}` line the default
/// `#[tokio::main] async fn main() -> Result<()>` `Termination` impl
/// prints (byte-identical stderr shape for every pre-V70-A8 failure), but
/// exit with `envelope::exit_code_for(&err)` instead of the hardcoded 1.
#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(err) = run(cli).await {
        eprintln!("Error: {err:?}");
        std::process::exit(envelope::exit_code_for(&err));
    }
}

/// Parse a `kb-code set create --span PATH[:START[-END]]`/`kb-code set add
/// PATH[:START[-END]]`/`kb-code pack --set` span argument: `PATH`, a bare
/// `PATH:LINE`, or `PATH:START-END`. Mirrors `parse_similar_target`'s
/// grammar but with the whole trailing spec optional — a bare `PATH`
/// (whole file) is legal here, unlike `similar`'s mandatory range, so this
/// never fails outright: a trailing `:<tail>` that parses as neither
/// `START-END` nor a single `LINE` is treated as part of the path itself
/// (same "be liberal about a path that legitimately contains a colon"
/// posture `parse_path_line`'s doc already documents) rather than an error.
fn parse_span_arg(s: &str) -> (String, Option<u32>, Option<u32>) {
    let Some((path, tail)) = s.rsplit_once(':') else {
        return (s.to_string(), None, None);
    };
    if let Some((start_s, end_s)) = tail.split_once('-') {
        if let (Ok(start), Ok(end)) = (start_s.parse::<u32>(), end_s.parse::<u32>()) {
            return (path.to_string(), Some(start), Some(end));
        }
        return (s.to_string(), None, None);
    }
    match tail.parse::<u32>() {
        Ok(line) => (path.to_string(), Some(line), Some(line)),
        Err(_) => (s.to_string(), None, None),
    }
}

/// Parse a `kb-code similar` target: `PATH:START-END`.
fn parse_similar_target(s: &str) -> Result<(String, u32, u32)> {
    let (path, range) = s
        .rsplit_once(':')
        .with_context(|| format!("expected PATH:START-END, got {s:?}"))?;
    let (start_s, end_s) = range
        .split_once('-')
        .with_context(|| format!("expected START-END, got {range:?}"))?;
    let start: u32 = start_s
        .parse()
        .with_context(|| format!("invalid start in {s:?}"))?;
    let end: u32 = end_s
        .parse()
        .with_context(|| format!("invalid end in {s:?}"))?;
    Ok((path.to_string(), start, end))
}

/// Parse a `kb-code why` target: `PATH` or `PATH:LINE` — the trailing
/// `:<digits>` is a line number ONLY when it parses cleanly as one; any
/// other trailing colon segment (or none at all) leaves the whole string as
/// the path (mirrors `parse_line_range_arg`'s "be liberal about what looks
/// like a path" posture, since paths may legitimately contain colons on
/// some filesystems).
fn parse_path_line(s: &str) -> (String, Option<u32>) {
    if let Some((path, line_s)) = s.rsplit_once(':') {
        if let Ok(line) = line_s.parse::<u32>() {
            return (path.to_string(), Some(line));
        }
    }
    (s.to_string(), None)
}

/// Parse a CLI `START:END` line-range argument (`kb-code blame --lines`).
fn parse_line_range_arg(s: &str) -> Result<(u32, u32)> {
    let (start, end) = s
        .split_once(':')
        .with_context(|| format!("expected START:END, got {s:?}"))?;
    let start: u32 = start
        .parse()
        .with_context(|| format!("invalid start in {s:?}"))?;
    let end: u32 = end
        .parse()
        .with_context(|| format!("invalid end in {s:?}"))?;
    Ok((start, end))
}

/// Parse a `kb-code resolve` target: `PATH:LINE:COL`. Splits on the LAST TWO
/// `:` (`rsplitn(3, ':')`, not two chained `rsplit_once`s) so a PATH that
/// happens to contain its own colon (rare, but legal on some filesystems —
/// same posture `parse_path_line`'s doc documents for `why`) only loses its
/// trailing `:LINE:COL`, never a colon earlier in the path.
fn parse_resolve_target(s: &str) -> Result<(String, u32, u32)> {
    let mut parts = s.rsplitn(3, ':');
    let col_s = parts
        .next()
        .with_context(|| format!("expected PATH:LINE:COL, got {s:?}"))?;
    let line_s = parts
        .next()
        .with_context(|| format!("expected PATH:LINE:COL, got {s:?}"))?;
    let path = parts
        .next()
        .with_context(|| format!("expected PATH:LINE:COL, got {s:?}"))?;
    let line: u32 = line_s
        .parse()
        .with_context(|| format!("invalid line number in {s:?}"))?;
    let col: u32 = col_s
        .parse()
        .with_context(|| format!("invalid col number in {s:?}"))?;
    Ok((path.to_string(), line, col))
}

// --- S2-B1 (design-s2.md § S2-C; append-only fns, delimited from
// concurrent edits elsewhere in this file) ---------------------------------

/// `PATH:LINE` or `PATH:LINE:COL` (col defaults to 0) — S2-C's own
/// convention, distinct from [`parse_resolve_target`]'s mandatory
/// `PATH:LINE:COL` (a code-actions range often starts at column 0, and
/// requiring `:0` on every call would be needless friction). Same
/// "colons before the last two belong to PATH" assumption
/// `parse_resolve_target` already makes when a column IS given.
fn parse_code_actions_target(s: &str) -> Result<(String, u32, u32)> {
    let colons = s.matches(':').count();
    if colons == 0 {
        anyhow::bail!("expected PATH:LINE or PATH:LINE:COL, got {s:?}");
    }
    if colons == 1 {
        let (path, line_s) = s.rsplit_once(':').expect("checked colons == 1 above");
        let line: u32 = line_s
            .parse()
            .with_context(|| format!("invalid line number in {s:?}"))?;
        return Ok((path.to_string(), line, 0));
    }
    let mut parts = s.rsplitn(3, ':');
    let col_s = parts.next().expect("checked colons >= 2 above");
    let line_s = parts.next().expect("checked colons >= 2 above");
    let path = parts.next().expect("checked colons >= 2 above");
    let line: u32 = line_s
        .parse()
        .with_context(|| format!("invalid line number in {s:?}"))?;
    let col: u32 = col_s
        .parse()
        .with_context(|| format!("invalid col number in {s:?}"))?;
    Ok((path.to_string(), line, col))
}

/// `--end LINE` or `--end LINE:COL` (col defaults to 0).
fn parse_code_actions_end(s: &str) -> Result<(u32, u32)> {
    match s.split_once(':') {
        Some((line_s, col_s)) => {
            let line: u32 = line_s
                .parse()
                .with_context(|| format!("invalid --end line number in {s:?}"))?;
            let col: u32 = col_s
                .parse()
                .with_context(|| format!("invalid --end col number in {s:?}"))?;
            Ok((line, col))
        }
        None => {
            let line: u32 = s
                .parse()
                .with_context(|| format!("invalid --end line number in {s:?}"))?;
            Ok((line, 0))
        }
    }
}

/// One AddComment(+suggestion) op, JSON-shaped exactly like
/// `kb_code_server::routes::AnnotationBatchOp::AddComment` — see that
/// type's doc (routes.rs, at this unit's fork sha). `--suggest N` builds
/// ONE of these per (file, edit) pair (design-s2.md § S2-C's CLI section:
/// "one op per file-edit"), never grouped by file: kb's suggestion model
/// anchors on ONE contiguous line span
/// (`kb_code_server::routes::stored_anchor_lines`), so a code action whose
/// edit touches several lines within the same file — or several files —
/// still needs one op per edit, not one op per file.
///
/// kb's suggestion `replacement` is a WHOLE-LINE(S) replacement (the
/// server captures `original` as the full text of `line..=line_end` —
/// `kb_code_server::routes::original_from_content`), while an LSP
/// `TextEdit` is column-precise. This fn rebuilds the resulting full
/// line(s) text by splicing `edit.new_text` into `file_content` at the
/// edit's byte columns — never sending the bare `new_text` fragment as
/// the replacement, which would silently discard whatever surrounds it on
/// the touched line(s).
fn code_action_edit_to_batch_op(
    title: &str,
    provider: &str,
    path: &str,
    edit: &CodeActionEdit,
    file_content: &str,
) -> Result<serde_json::Value> {
    let replacement = splice_full_lines(file_content, edit)?;
    let (anchor_kind, line_end) = if edit.start_line == edit.end_line {
        (kb_code_server::annotations::ANCHOR_KIND_LINE, None)
    } else {
        (
            kb_code_server::annotations::ANCHOR_KIND_RANGE,
            Some(edit.end_line),
        )
    };
    Ok(serde_json::json!({
        "op": "add_comment",
        "path": path,
        "line": edit.start_line,
        "line_end": line_end,
        "anchor_kind": anchor_kind,
        "body": format!("Quick fix: {title}\n\nvia {provider} (lsp-live)"),
        "intent": "note",
        "suggestion": { "replacement": replacement },
    }))
}

/// The splice math [`code_action_edit_to_batch_op`] needs — pure, no I/O.
/// `file_content` must be the file's CURRENT text (the caller fetches it
/// fresh via `GET /api/file` before calling this); byte-offset columns
/// that land off a line or off a UTF-8 char boundary error out rather
/// than panicking or silently truncating.
fn splice_full_lines(file_content: &str, edit: &CodeActionEdit) -> Result<String> {
    anyhow::ensure!(
        edit.start_line >= 1 && edit.end_line >= 1,
        "code action edit has a 0 line (lip/1 lines are 1-based)"
    );
    let lines: Vec<&str> = file_content.split('\n').collect();
    let start_idx = (edit.start_line - 1) as usize;
    let end_idx = (edit.end_line - 1) as usize;
    let start_line = *lines
        .get(start_idx)
        .with_context(|| format!("line {} is out of range for this file", edit.start_line))?;
    let end_line = *lines
        .get(end_idx)
        .with_context(|| format!("line {} is out of range for this file", edit.end_line))?;
    let prefix = start_line
        .get(0..edit.start_col as usize)
        .with_context(|| {
            format!(
                "start_col {} out of range on line {}",
                edit.start_col, edit.start_line
            )
        })?;
    let suffix = end_line
        .get(edit.end_col as usize..end_line.len())
        .with_context(|| {
            format!(
                "end_col {} out of range on line {}",
                edit.end_col, edit.end_line
            )
        })?;
    Ok(format!("{prefix}{}{suffix}", edit.new_text))
}

/// Flattens one [`CodeAction`]'s edits (in the server's own file/edit
/// order — "relay in LSP order; ordering is the consumer's concern",
/// design-s2.md § S2-C) into one batch op per (file, edit) — see
/// [`code_action_edit_to_batch_op`]'s doc. `file_contents` must already
/// carry every path the action's edits touch (the caller fetches them via
/// `GET /api/file` before calling this) — a missing entry is a caller
/// bug, reported rather than silently skipped.
fn code_action_to_batch_ops(
    action: &CodeAction,
    provider: &str,
    file_contents: &std::collections::HashMap<String, String>,
) -> Result<Vec<serde_json::Value>> {
    let mut ops = Vec::new();
    for file_edit in &action.edits {
        let content = file_contents
            .get(&file_edit.path)
            .ok_or_else(|| anyhow::anyhow!("no file content fetched for {:?}", file_edit.path))?;
        for edit in &file_edit.edits {
            ops.push(code_action_edit_to_batch_op(
                &action.title,
                provider,
                &file_edit.path,
                edit,
                content,
            )?);
        }
    }
    Ok(ops)
}

// --- end S2-B1 append -------------------------------------------------

// --- offline (in-process) ------------------------------------------------

fn refs(repo_path: &Path) -> Result<()> {
    let repo = GitRepo::open(repo_path).with_context(|| format!("open {}", repo_path.display()))?;
    let refs = repo.list_refs().context("list refs")?;
    for r in refs {
        let marker = if r.is_head { "  (HEAD)" } else { "" };
        println!(
            "{:<6} {:<30} {}{}",
            r.kind.as_str(),
            r.name,
            r.target_sha,
            marker
        );
    }
    Ok(())
}

fn tree(repo_path: &Path, path: &str, rev: &str, json: bool) -> Result<()> {
    let repo = GitRepo::open(repo_path).with_context(|| format!("open {}", repo_path.display()))?;
    let entries = repo
        .list_tree(rev, path)
        .with_context(|| format!("list tree {path:?} at {rev:?}"))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }
    print_tree_entries(
        entries
            .iter()
            .map(|e| (e.kind.as_str(), e.size, e.oid.as_str(), e.name.as_str())),
    );
    Ok(())
}

fn cat(repo_path: &Path, path: &str, rev: &str) -> Result<()> {
    let repo = GitRepo::open(repo_path).with_context(|| format!("open {}", repo_path.display()))?;
    let bytes = repo
        .read_blob(rev, path, DEFAULT_BLOB_SIZE_CAP)
        .with_context(|| format!("cat {path:?} at {rev:?}"))?;
    std::io::stdout()
        .write_all(&bytes)
        .context("write stdout")?;
    Ok(())
}

/// Shared human-readable tree table, used by both the offline `tree` (typed
/// `git::TreeEntry`s) and daemon `tree_daemon` (parsed JSON) paths, so the
/// two render byte-identically.
fn print_tree_entries<'a>(entries: impl Iterator<Item = (&'a str, Option<u64>, &'a str, &'a str)>) {
    for (kind, size, oid, name) in entries {
        let size = size
            .map(|s| s.to_string())
            .unwrap_or_else(|| "-".to_string());
        let short_oid = &oid[..oid.len().min(12)];
        println!("{kind:<10} {size:>10} {short_oid}  {name}");
    }
}

// --- daemon HTTP client ---------------------------------------------------

/// V70-A2 (SEC-02) — the ONE place this CLI builds a reqwest client.
///
/// Every client carries `X-Kbc-Request: 1` as a DEFAULT header, so every
/// mutating verb satisfies the daemon's mutation-header guard without any
/// per-call-site plumbing to forget. It is set on reads too: the header
/// costs nothing, and a client that sets it only sometimes is a client
/// whose next mutating verb forgets.
///
/// The three call sites that need a longer timeout (`age`, `behavioral
/// backfill`, …) go through [`client_builder`] rather than
/// `reqwest::Client::builder()` directly, for the same reason.
///
/// V70-A8 (D20 secret hygiene) — ALSO sets `Authorization: Bearer <token>`
/// as a default header whenever [`token::resolve_bearer_token`] resolves
/// one (env `KB_CODE_TOKEN` or the `token_file`, see that module's doc).
/// Absent a token (the ordinary loopback-daemon case), this is a byte-
/// identical no-op — no header is added, matching every pre-V70-A8
/// invocation. A resolved-but-header-invalid token (non-ASCII/control
/// bytes — `HeaderValue::from_str` rejects those) is silently skipped
/// rather than panicking here: the daemon then 401s the request exactly as
/// if no token had been set, which is diagnosable (`kb-code doctor
/// --agent`), whereas panicking a synchronous builder fn over a malformed
/// secret would take down a caller that never even needed to reach a
/// non-loopback daemon.
pub(crate) fn client_builder() -> reqwest::ClientBuilder {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-kbc-request",
        reqwest::header::HeaderValue::from_static("1"),
    );
    if let Some(bearer) = token::resolve_bearer_token() {
        if let Ok(mut value) = reqwest::header::HeaderValue::from_str(&format!("Bearer {bearer}")) {
            value.set_sensitive(true);
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }
    }
    reqwest::Client::builder().default_headers(headers)
}

fn http_client() -> Result<reqwest::Client> {
    client_builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("build http client")
}

async fn get_json(
    client: &reqwest::Client,
    daemon: &str,
    path: &str,
    query: &[(&str, &str)],
) -> Result<serde_json::Value> {
    let url = format!("{}{path}", daemon.trim_end_matches('/'));
    client
        .get(&url)
        .query(query)
        .send()
        .await
        .with_context(|| format!("GET {url} — is kb-code-server running at {daemon}?"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?
        .json()
        .await
        .with_context(|| format!("parse {url} response as JSON"))
}

/// V71-D1b — the unified search box's own client timeout. D1's bench
/// recorded a 29.6s FIRST unified `/api/search` call on a cold daemon
/// (`store::symbols_for_repo` materialising 26,820 rows over a 6,553-file
/// repo) — longer than [`http_client`]'s 10s default, which turned a
/// daemon that was simply still warming into a bare "is kb-code-server
/// running?". V71-D1b warms that cache in the background at boot, so this
/// should rarely be hit in practice; it stays generous (comfortably above
/// the recorded cliff) as a second line of defence for whatever races it.
const SEARCH_CLIENT_TIMEOUT: Duration = Duration::from_secs(45);

/// Like [`get_json`], but a request TIMEOUT gets its own honest message
/// instead of [`get_json`]'s "is kb-code-server running?" — a timeout means
/// the connection is fine and the daemon is simply still working (see
/// [`SEARCH_CLIENT_TIMEOUT`]'s doc), which is a materially different thing
/// to tell a caller than "nothing is listening at all". Every other failure
/// (connection refused, DNS, TLS, …) falls back to `get_json`'s own message
/// unchanged.
async fn get_json_warming_aware(
    client: &reqwest::Client,
    daemon: &str,
    path: &str,
    query: &[(&str, &str)],
) -> Result<serde_json::Value> {
    let url = format!("{}{path}", daemon.trim_end_matches('/'));
    let resp = client.get(&url).query(query).send().await.map_err(|e| {
        if e.is_timeout() {
            anyhow::anyhow!(
                "GET {url} timed out after {:?} — the daemon may still be warming its \
                 symbol cache after a restart (a first search over a large repo can take \
                 longer than usual); try again in a few seconds",
                SEARCH_CLIENT_TIMEOUT
            )
        } else {
            anyhow::Error::new(e).context(format!(
                "GET {url} — is kb-code-server running at {daemon}?"
            ))
        }
    })?;
    resp.error_for_status()
        .with_context(|| format!("GET {url}"))?
        .json()
        .await
        .with_context(|| format!("parse {url} response as JSON"))
}

/// `POST` counterpart to [`get_json`] — `kb-code backfill`'s
/// `POST /api/backfill?repo=` is the only POST route this CLI drives today.
async fn post_json(
    client: &reqwest::Client,
    daemon: &str,
    path: &str,
    query: &[(&str, &str)],
) -> Result<serde_json::Value> {
    let url = format!("{}{path}", daemon.trim_end_matches('/'));
    client
        .post(&url)
        .query(query)
        .send()
        .await
        .with_context(|| format!("POST {url} — is kb-code-server running at {daemon}?"))?
        .error_for_status()
        .with_context(|| format!("POST {url}"))?
        .json()
        .await
        .with_context(|| format!("parse {url} response as JSON"))
}

async fn identity(daemon: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(&client, daemon, "/api/identity", &[]).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    let s = |k: &str| body[k].as_str().unwrap_or("?").to_string();
    println!("name:       {}", s("name"));
    println!("version:    {}", s("version"));
    println!("started_at: {}", s("started_at"));
    let repos = body["repos"].as_array().cloned().unwrap_or_default();
    println!("repos:      {}", repos.len());
    for r in &repos {
        println!(
            "  - {} ({})",
            r["name"].as_str().unwrap_or("?"),
            r["path"].as_str().unwrap_or("?"),
        );
    }
    Ok(())
}

/// Resolve `--since` into a unix instant. Accepts a duration
/// (`90m`/`24h`/`7d` — "how far back"), an ISO date (`2026-09-03`) or a
/// full RFC 3339 instant. Deliberately a small closed grammar rather than
/// a date-parsing dependency: this is one flag on one verb, and an
/// unparseable value is an ERROR, never a silent default (a caller who
/// typo'd a window must not get a different window than they asked for).
fn parse_since(spec: &str) -> Result<i64> {
    let now = chrono::Utc::now();
    let s = spec.trim();
    if let Some(rest) = s.strip_suffix(['m', 'h', 'd']) {
        if let Ok(n) = rest.parse::<i64>() {
            let secs = match s.chars().last() {
                Some('m') => 60,
                Some('h') => 3600,
                _ => 86_400,
            };
            return Ok(now.timestamp() - n * secs);
        }
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Ok(dt.timestamp());
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        if let Some(dt) = d.and_hms_opt(0, 0, 0) {
            return Ok(dt.and_utc().timestamp());
        }
    }
    anyhow::bail!("could not parse --since {s:?} (want `90m`/`24h`/`7d`, `YYYY-MM-DD`, or an RFC 3339 instant)")
}

/// `kb-code audit` — V70-A2 (SEC-20). See the clap variant's doc.
async fn audit_cmd(
    daemon: &str,
    since: Option<&str>,
    limit: Option<usize>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut q: Vec<(&str, String)> = Vec::new();
    if let Some(s) = since {
        q.push(("since", parse_since(s)?.to_string()));
    }
    if let Some(n) = limit {
        q.push(("limit", n.to_string()));
    }
    let qref: Vec<(&str, &str)> = q.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let body = get_json(&client, daemon, "/api/audit", &qref).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let entries = body["entries"].as_array().cloned().unwrap_or_default();
    if entries.is_empty() {
        println!("audit · no mutations in the window");
        return Ok(());
    }
    println!(
        "audit · {} mutation(s), newest first",
        body["count"].as_u64().unwrap_or(entries.len() as u64)
    );
    for e in &entries {
        let repo = e["repo"].as_str().unwrap_or("-");
        let target = e["target"].as_str().unwrap_or("");
        println!(
            "{:<26} {:<7} {:<11} {:<4} {:<12} {} {}",
            e["ts"].as_str().unwrap_or("?"),
            e["method"].as_str().unwrap_or("?"),
            e["admission"].as_str().unwrap_or("?"),
            e["outcome"].as_str().unwrap_or("?"),
            repo,
            e["route"].as_str().unwrap_or("?"),
            target,
        );
    }
    Ok(())
}

async fn tree_daemon(daemon: &str, repo: &str, path: &str, rev: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(
        &client,
        daemon,
        "/api/tree",
        &[("repo", repo), ("path", path), ("ref", rev)],
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let entries = body["entries"].as_array().cloned().unwrap_or_default();
    print_tree_entries(entries.iter().map(|e| {
        (
            e["kind"].as_str().unwrap_or("?"),
            e["size"].as_u64(),
            e["oid"].as_str().unwrap_or(""),
            e["name"].as_str().unwrap_or("?"),
        )
    }));
    Ok(())
}

async fn cat_daemon(daemon: &str, repo: &str, path: &str, rev: &str) -> Result<()> {
    let client = http_client()?;
    let body = get_json(
        &client,
        daemon,
        "/api/file",
        &[("repo", repo), ("path", path), ("ref", rev)],
    )
    .await?;
    let bytes = decode_file_content(&body)?;
    std::io::stdout()
        .write_all(&bytes)
        .context("write stdout")?;
    Ok(())
}

/// `GET /api/file`'s `{encoding, content}` pair, decoded to raw bytes.
fn decode_file_content(body: &serde_json::Value) -> Result<Vec<u8>> {
    use base64::Engine;
    let encoding = body["encoding"].as_str().unwrap_or("utf8");
    let content = body["content"].as_str().unwrap_or("");
    if encoding == "base64" {
        base64::engine::general_purpose::STANDARD
            .decode(content)
            .context("decode base64 file content")
    } else {
        Ok(content.as_bytes().to_vec())
    }
}

async fn repos_cmd(daemon: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(&client, daemon, "/api/repos", &[]).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if let Some(loopback) = body.get("loopback").and_then(|v| v.as_bool()) {
        println!(
            "daemon reachable over: {}",
            if loopback { "loopback" } else { "non-loopback" }
        );
    }
    let repos = body["repos"].as_array().cloned().unwrap_or_default();
    for r in &repos {
        let head = &r["head"];
        let branch = head["branch"].as_str().unwrap_or("-");
        let sha = head["sha"].as_str().unwrap_or("");
        let short_sha = &sha[..sha.len().min(12)];
        // V70-A8 (D20 `repos --json`) — `writable`/`is_worktree` are
        // additive fields on `RepoListEntry`; absent (older daemon) prints
        // as "?" rather than a false "ro"/non-worktree claim.
        let rw = match r.get("writable").and_then(|v| v.as_bool()) {
            Some(true) => "rw",
            Some(false) => "ro",
            None => "? ",
        };
        let wt = match r.get("is_worktree").and_then(|v| v.as_bool()) {
            Some(true) => "worktree",
            Some(false) => "",
            None => "",
        };
        println!(
            "{:<20} {:>6} files {:>6} symbols  {:<10} {rw}  {branch}@{short_sha}  {wt}",
            r["name"].as_str().unwrap_or("?"),
            r["file_count"].as_u64().unwrap_or(0),
            r["symbol_count"].as_u64().unwrap_or(0),
            r["watcher"].as_str().unwrap_or("?"),
        );
    }
    Ok(())
}

// ── V72-H1 — `kb-code syntax` / `kb-code parity` ────────────────────────
//
// Both verbs are pure daemon reads with no params, and both take the route
// PATH from the server crate's own declared contract
// (`kb_code_server::syntax::V72_H1_ROUTES`) rather than a string literal,
// so `cli_requests_send_every_param_their_route_requires` can walk the two
// against each other exactly as it does for `entity`/`seq`/`act`/`tree`.
//
// They are daemon-only ON PURPOSE even though the registry is build-time
// data this binary also links: the honest answer to "what can be done with
// a `.rake` file" is what the DAEMON's build can do, and a CLI rendering
// its own table would quietly answer for a different binary.

/// The `GET /api/syntax` request: `(path, query)`.
fn syntax_request() -> (&'static str, Vec<(&'static str, String)>) {
    (kb_code_server::syntax::SYNTAX_ROUTE.path, Vec::new())
}

/// The `GET /api/parity` request: `(path, query)`.
fn parity_request() -> (&'static str, Vec<(&'static str, String)>) {
    (kb_code_server::syntax::PARITY_ROUTE.path, Vec::new())
}

/// Render one row's addressing keys: `.rb .rake` / `Gemfile` / `#!ruby`.
fn syntax_keys(row: &serde_json::Value) -> String {
    let list = |field: &str, prefix: &str| -> Vec<String> {
        row[field]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|v| format!("{prefix}{v}"))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut keys = list("extensions", ".");
    keys.extend(list("filenames", ""));
    keys.extend(list("interpreters", "#!"));
    if keys.is_empty() {
        "—".to_string()
    } else {
        keys.join(" ")
    }
}

/// `kb-code syntax` — `GET /api/syntax`.
async fn syntax_cmd(daemon: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let (path, query) = syntax_request();
    let body = get_json(&client, daemon, path, &as_query_pairs(&query)).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let rows = body["rows"].as_array().cloned().unwrap_or_default();
    println!(
        "{:<12} {:<15} {:<30} ADDRESSED BY",
        "LANG", "TIER", "GRAMMAR"
    );
    let mut notes: Vec<(String, String)> = Vec::new();
    for r in &rows {
        let lang = r["lang"].as_str().unwrap_or("?");
        println!(
            "{:<12} {:<15} {:<30} {}",
            lang,
            r["tier"].as_str().unwrap_or("?"),
            r["grammar"].as_str().unwrap_or("— none linked"),
            syntax_keys(r),
        );
        if let Some(note) = r["note"].as_str() {
            notes.push((lang.to_string(), note.to_string()));
        }
        if r["injection_host"].as_bool().unwrap_or(false) {
            notes.push((
                lang.to_string(),
                "injection host — declared for the injection-aware pipeline, which is not \
                 built yet"
                    .to_string(),
            ));
        }
    }
    println!("\n{} file type(s)", body["total"].as_u64().unwrap_or(0));
    if !notes.is_empty() {
        println!("\nnotes:");
        for (lang, note) in notes {
            println!("  {lang}: {note}");
        }
    }
    Ok(())
}

/// `kb-code parity` — `GET /api/parity`. Renders the grid with a numbered
/// legend: every non-`yes` cell carries a reason, and printing each one
/// inline would make the grid unreadable, so identical reasons share a
/// marker and the legend prints them once.
async fn parity_cmd(daemon: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let (path, query) = parity_request();
    let body = get_json(&client, daemon, path, &as_query_pairs(&query)).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let caps: Vec<String> = body["capabilities"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let rows = body["rows"].as_array().cloned().unwrap_or_default();

    let mut header = format!("{:<12} {:<15}", "LANG", "TIER");
    for c in &caps {
        header.push_str(&format!(" {c:<12}"));
    }
    println!("{header}");

    // Reason → marker, in first-seen order.
    let mut legend: Vec<String> = Vec::new();
    for r in &rows {
        let mut line = format!(
            "{:<12} {:<15}",
            r["lang"].as_str().unwrap_or("?"),
            r["tier"].as_str().unwrap_or("?")
        );
        for cap in &caps {
            let cell = r["cells"]
                .as_array()
                .and_then(|a| a.iter().find(|c| c["capability"].as_str() == Some(cap)));
            let state = cell
                .and_then(|c| c["state"].as_str())
                .unwrap_or("?")
                .to_string();
            let marked = match cell.and_then(|c| c["reason"].as_str()) {
                Some(reason) => {
                    let idx = match legend.iter().position(|r| r.as_str() == reason) {
                        Some(i) => i,
                        None => {
                            legend.push(reason.to_string());
                            legend.len() - 1
                        }
                    };
                    format!("{state}[{}]", idx + 1)
                }
                None => state,
            };
            line.push_str(&format!(" {marked:<12}"));
        }
        println!("{line}");
    }
    println!(
        "\n{} language(s) × {} capability(ies)",
        body["total"].as_u64().unwrap_or(0),
        caps.len()
    );
    if !legend.is_empty() {
        println!("\nwhy:");
        for (i, reason) in legend.iter().enumerate() {
            println!("  [{}] {reason}", i + 1);
        }
    }
    Ok(())
}

// ════════════════════════════════════════════════════════════════════════
// V70-A8 (D20 CLI hygiene)
// ════════════════════════════════════════════════════════════════════════

/// The ONE cwd→repo inference algorithm this unit ships (D20's "cwd→repo
/// [resolution unified to] one implementation" — see `Cmd::Doctor`'s doc).
/// `repos` is `(name, configured-path)` pairs, e.g. `GET /api/repos`'
/// `name`/`path` fields verbatim. Longest-match wins; a match is only ever
/// on a path-COMPONENT boundary (`target == path` or `target.starts_with(
/// path + "/")`) — never a bare string prefix, so a repo configured at
/// `/x/proj` can never match a sibling directory `/x/proj-extra`.
///
/// This does NOT replace the three pre-existing, independent
/// reimplementations of this exact algorithm named in recon
/// `cli-agent-surface.md` open question 8 (`plugins/kb-code/hooks/
/// kb-code-why.sh` + `kb-code-annotations.sh`'s inline jq, and
/// `plugins/kb-memory/hooks/kb-omp.ts`'s `needRepo`) — replatforming three
/// independently-tested, fail-open hook/adapter surfaces onto a Rust CLI
/// subprocess call is its own unit with its own test pass, not a hygiene
/// pass this unit's one-shot-compile-check budget can safely absorb. This
/// function is the canonical implementation going forward and is used by
/// `kb-code doctor` below; see this unit's handoff note for the full
/// reasoning (including a real boundary-check bug found, but NOT fixed
/// here, in `kb-omp.ts`'s copy: `cwd.startsWith(r.path)` has no trailing-
/// slash guard).
fn resolve_repo_for_path<'a>(repos: &'a [(String, String)], target: &str) -> Option<&'a str> {
    repos
        .iter()
        .filter(|(_, path)| {
            let path = path.trim_end_matches('/');
            target == path || target.starts_with(&format!("{path}/"))
        })
        .max_by_key(|(_, path)| path.len())
        .map(|(name, _)| name.as_str())
}

#[cfg(test)]
mod resolve_repo_for_path_tests {
    use super::resolve_repo_for_path;

    fn repos(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(n, p)| (n.to_string(), p.to_string()))
            .collect()
    }

    #[test]
    fn matches_a_file_under_the_repo_root() {
        let repos = repos(&[("kb", "/home/user/project/kb")]);
        assert_eq!(
            resolve_repo_for_path(&repos, "/home/user/project/kb/src/main.rs"),
            Some("kb")
        );
    }

    #[test]
    fn matches_the_repo_root_itself() {
        let repos = repos(&[("kb", "/home/user/project/kb")]);
        assert_eq!(
            resolve_repo_for_path(&repos, "/home/user/project/kb"),
            Some("kb")
        );
    }

    #[test]
    fn never_matches_a_sibling_with_a_shared_prefix() {
        // The exact bug this fn is written to NOT have (present in
        // `kb-omp.ts`'s `needRepo`, see this fn's own doc): a bare
        // `starts_with` would wrongly match `/x/proj-extra` against a
        // repo configured at `/x/proj`.
        let repos = repos(&[("proj", "/x/proj")]);
        assert_eq!(resolve_repo_for_path(&repos, "/x/proj-extra/file.rs"), None);
    }

    #[test]
    fn longest_configured_path_wins_for_nested_repos() {
        let repos = repos(&[("outer", "/x"), ("inner", "/x/y")]);
        assert_eq!(resolve_repo_for_path(&repos, "/x/y/z.rs"), Some("inner"));
        assert_eq!(resolve_repo_for_path(&repos, "/x/other.rs"), Some("outer"));
    }

    #[test]
    fn no_configured_repo_contains_the_path() {
        let repos = repos(&[("kb", "/home/user/project/kb")]);
        assert_eq!(
            resolve_repo_for_path(&repos, "/tmp/somewhere/else.rs"),
            None
        );
    }

    #[test]
    fn tolerates_a_trailing_slash_on_the_configured_path() {
        let repos = repos(&[("kb", "/home/user/project/kb/")]);
        assert_eq!(
            resolve_repo_for_path(&repos, "/home/user/project/kb/src/main.rs"),
            Some("kb")
        );
    }
}

/// `kb-code doctor [--agent] [--json]` — see `Cmd::Doctor`'s doc.
async fn doctor_cmd(daemon: &str, agent: bool, json: bool) -> Result<()> {
    let client = http_client()?;
    let identity_result = get_json(&client, daemon, "/api/identity", &[]).await;

    let cli_protocol = kb_core::sibling::SIBLING_PROTOCOL;
    let cli_major = kb_core::sibling::SIBLING_MAJOR;
    let cli_schema_epoch = kb_code_server::store::schema_epoch();
    let token_source = if std::env::var("KB_CODE_TOKEN")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .is_some()
    {
        "env:KB_CODE_TOKEN"
    } else if token::resolve_bearer_token().is_some() {
        "token_file"
    } else {
        "none"
    };
    let token_file_path = token::token_file_path()
        .ok()
        .map(|p| p.display().to_string());
    let cwd = std::env::current_dir().ok();

    let mut checks = Vec::new();
    let mut ok_overall = true;

    match &identity_result {
        Ok(body) => {
            checks.push(serde_json::json!({"check": "daemon_reachable", "ok": true}));
            let d_protocol = body["sibling_protocol"].as_str().unwrap_or("");
            let d_major = body["sibling_major"].as_u64().unwrap_or(0);
            let d_epoch = body["schema_epoch"].as_u64().unwrap_or(0);
            let handshake_ok = d_protocol == cli_protocol
                && d_major == cli_major as u64
                && d_epoch == cli_schema_epoch as u64;
            ok_overall &= handshake_ok;
            checks.push(serde_json::json!({
                "check": "sibling_handshake",
                "ok": handshake_ok,
                "cli": {"protocol": cli_protocol, "major": cli_major, "schema_epoch": cli_schema_epoch},
                "daemon": {"protocol": d_protocol, "major": d_major, "schema_epoch": d_epoch},
            }));

            let repos: Vec<(String, String)> = body["repos"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|r| {
                    (
                        r["name"].as_str().unwrap_or("").to_string(),
                        r["path"].as_str().unwrap_or("").to_string(),
                    )
                })
                .collect();
            let cwd_str = cwd.as_ref().map(|c| c.display().to_string());
            let matched = cwd_str
                .as_deref()
                .and_then(|c| resolve_repo_for_path(&repos, c));
            checks.push(serde_json::json!({
                "check": "cwd_in_configured_repo",
                "ok": matched.is_some(),
                "cwd": cwd_str,
                "matched_repo": matched,
            }));
        }
        Err(e) => {
            ok_overall = false;
            checks.push(serde_json::json!({
                "check": "daemon_reachable",
                "ok": false,
                "error": format!("{e:#}"),
            }));
        }
    }

    checks.push(serde_json::json!({
        "check": "bearer_token",
        "source": token_source,
        "token_file": token_file_path,
    }));

    if json || agent {
        // A totally-unreachable daemon gets the ERROR envelope shape
        // (`{ok:false, error:{...}}`, D20) — the daemon-reached-but-skewed
        // case still gets the SUCCESS envelope with `degraded:true` (the
        // checks themselves are the useful payload there, not a bare
        // error string).
        if let Err(e) = &identity_result {
            envelope::print_err(
                "unreachable",
                &format!("{e:#}"),
                Some("is kb-code-server running at this --daemon?"),
            );
        } else {
            envelope::print_ok(
                "kbc-doctor/1",
                serde_json::json!({"ok": ok_overall, "checks": checks}),
                Vec::new(),
                !ok_overall,
                None,
            );
        }
    } else {
        println!(
            "kb-code doctor — {}",
            if ok_overall { "ok" } else { "problem found" }
        );
        for c in &checks {
            println!("  {}", serde_json::to_string(c)?);
        }
    }
    if !ok_overall {
        anyhow::bail!("kb-code doctor found a problem — see the checks printed above");
    }
    Ok(())
}

async fn schema_list_cmd(daemon: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(&client, daemon, "/api/schemas", &[]).await?;
    if json {
        envelope::print_ok("kbc-api-schemas/1", &body, Vec::new(), false, None);
        return Ok(());
    }
    println!("kb-code schema names:");
    for n in body["names"].as_array().cloned().unwrap_or_default() {
        println!("  {}", n.as_str().unwrap_or("?"));
    }
    Ok(())
}

async fn schema_show_cmd(
    daemon: &str,
    name: &str,
    json_schema_only: bool,
    example_only: bool,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let body = get_json(&client, daemon, &format!("/api/schemas/{name}"), &[]).await?;
    if json {
        envelope::print_ok(&format!("schema/{name}"), &body, Vec::new(), false, None);
        return Ok(());
    }
    if json_schema_only && !example_only {
        println!("{}", serde_json::to_string_pretty(&body["json_schema"])?);
        return Ok(());
    }
    if example_only && !json_schema_only {
        println!("{}", serde_json::to_string_pretty(&body["example"])?);
        return Ok(());
    }
    println!("schema: {name}");
    println!("{}", serde_json::to_string_pretty(&body["json_schema"])?);
    if let Some(example) = body.get("example") {
        if !example.is_null() {
            println!("\nexample:\n{}", serde_json::to_string_pretty(example)?);
        }
    }
    Ok(())
}

async fn diff_cmd(
    daemon: &str,
    repo: &str,
    path: &str,
    from: &str,
    to: Option<&str>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut q: Vec<(&str, &str)> = vec![("repo", repo), ("path", path), ("from", from)];
    if let Some(t) = to {
        q.push(("to", t));
    }
    let body = get_json(&client, daemon, "/api/diff", &q).await?;
    if json {
        envelope::print_ok("diff/1", &body, Vec::new(), false, None);
        return Ok(());
    }
    print!("{}", body["diff"].as_str().unwrap_or(""));
    Ok(())
}

async fn commit_cmd(daemon: &str, repo: &str, sha: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(
        &client,
        daemon,
        "/api/commit",
        &[("repo", repo), ("sha", sha)],
    )
    .await?;
    if json {
        envelope::print_ok("commit/1", &body, Vec::new(), false, None);
        return Ok(());
    }
    let full_sha = body["sha"].as_str().unwrap_or(sha);
    println!(
        "{}  {}",
        &full_sha[..full_sha.len().min(12)],
        body["subject"].as_str().unwrap_or("")
    );
    let author = &body["author"];
    println!(
        "author:    {} <{}>",
        author["name"].as_str().unwrap_or("?"),
        author["email"].as_str().unwrap_or("?")
    );
    let committer = &body["committer"];
    println!(
        "committer: {} <{}>",
        committer["name"].as_str().unwrap_or("?"),
        committer["email"].as_str().unwrap_or("?")
    );
    if let Some(parents) = body["parents"].as_array() {
        let parents: Vec<&str> = parents.iter().filter_map(|p| p.as_str()).collect();
        if !parents.is_empty() {
            println!("parents:   {}", parents.join(" "));
        }
    }
    if let Some(files) = body["files"].as_array() {
        println!("files ({}):", files.len());
        for f in files {
            let path = f.as_str().or_else(|| f["path"].as_str()).unwrap_or("?");
            println!("  {path}");
        }
    }
    Ok(())
}

async fn file_history_cmd(
    daemon: &str,
    repo: &str,
    path: &str,
    limit: Option<usize>,
    before: Option<i64>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut q: Vec<(&str, String)> = vec![("repo", repo.to_string()), ("path", path.to_string())];
    if let Some(n) = limit {
        q.push(("limit", n.to_string()));
    }
    if let Some(b) = before {
        q.push(("before", b.to_string()));
    }
    let qref: Vec<(&str, &str)> = q.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let body = get_json(&client, daemon, "/api/file-history", &qref).await?;
    if json {
        envelope::print_ok("file-history/1", &body, Vec::new(), false, None);
        return Ok(());
    }
    let entries = body["entries"].as_array().cloned().unwrap_or_default();
    for e in &entries {
        println!(
            "{}  {}  {}",
            e["sha"].as_str().unwrap_or("?").get(..12).unwrap_or("?"),
            e["author_time"].as_str().unwrap_or(""),
            e["subject"].as_str().unwrap_or(""),
        );
    }
    if body["truncated"].as_bool().unwrap_or(false) {
        println!("(truncated — pass --limit/--before to page further back)");
    }
    Ok(())
}

async fn range_diff_cmd(daemon: &str, repo: &str, old: &str, new: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(
        &client,
        daemon,
        "/api/range-diff",
        &[("repo", repo), ("old", old), ("new", new)],
    )
    .await?;
    if json {
        envelope::print_ok("range-diff/1", &body, Vec::new(), false, None);
        return Ok(());
    }
    let pairs = body["pairs"].as_array().cloned().unwrap_or_default();
    println!("range-diff {old}..{new} — {} pair(s)", pairs.len());
    for p in &pairs {
        println!("  {}", serde_json::to_string(p)?);
    }
    if body["truncated"].as_bool().unwrap_or(false) {
        println!("(truncated)");
    }
    Ok(())
}

async fn scopes_cmd(daemon: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(&client, daemon, "/api/scopes", &[]).await?;
    if json {
        envelope::print_ok("scopes/1", &body, Vec::new(), false, None);
        return Ok(());
    }
    let scopes = body["scopes"].as_object().cloned().unwrap_or_default();
    if scopes.is_empty() {
        println!("(no [scopes] configured)");
        return Ok(());
    }
    for (name, patterns) in &scopes {
        let patterns: Vec<&str> = patterns
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        println!("{name:<20} {}", patterns.join(", "));
    }
    Ok(())
}

async fn doc_refs_cmd(daemon: &str, repo: &str, path: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(
        &client,
        daemon,
        "/api/doc-refs",
        &[("repo", repo), ("path", path)],
    )
    .await?;
    if json {
        envelope::print_ok("doc-refs/1", &body, Vec::new(), false, None);
        return Ok(());
    }
    let claims = body["claims"].as_array().cloned().unwrap_or_default();
    if claims.is_empty() {
        println!("(no kb documents currently claim this path)");
        return Ok(());
    }
    for c in &claims {
        println!(
            "{}  {}",
            c["kb"].as_str().unwrap_or("?"),
            c["doc_public_href"].as_str().unwrap_or("?"),
        );
    }
    Ok(())
}

/// The closed lane vocabulary `kb-code watch` accepts — see `Cmd::Watch`'s
/// doc for what each delegates to.
const WATCH_LANES: &[&str] = &["annotate", "inbox"];

/// `kb-code watch <lane>...` — V71-X1. Spawns one task per requested lane
/// and waits for ALL of them (`futures::future::try_join_all`-shaped via a
/// hand-rolled loop, since this crate doesn't otherwise depend on
/// `futures`): `annotate` runs `watch::run` UNMODIFIED save for the two
/// new `WatchArgs` fields this unit adds (`since`, `lane_tag`); `inbox`
/// runs the NEW `inbox::run_watch_lane` (see that fn's doc for why it is
/// a different SHELL around `inbox --watch`'s own pure functions, not a
/// second implementation of them). Both print NDJSON directly to stdout
/// from their own task — safe to interleave because every line either
/// lane emits in `--json` mode is exactly one compact `println!` call
/// (verified in each module's own tests), never a multi-line write.
#[allow(clippy::too_many_arguments)]
async fn watch_unified_cmd(
    lanes: &[String],
    since: Option<i64>,
    repo: Option<String>,
    review: Option<i64>,
    backlog: bool,
    once: bool,
    timeout: Option<u64>,
    ignore_author: Vec<String>,
    interval: u64,
    daemon: &str,
    json: bool,
) -> Result<()> {
    if !json {
        anyhow::bail!(
            "kb-code watch: pass --json — two lanes print concurrently to the same \
             stdout, and only the one-line-per-item NDJSON shape is safe to interleave"
        );
    }
    let mut requested: Vec<&str> = Vec::new();
    for lane in lanes {
        let lane = lane.as_str();
        if !WATCH_LANES.contains(&lane) {
            anyhow::bail!(
                "kb-code watch: unknown lane {lane:?} — one of: {}",
                WATCH_LANES.join(", ")
            );
        }
        if !requested.contains(&lane) {
            requested.push(lane);
        }
    }

    let mut tasks: Vec<tokio::task::JoinHandle<Result<()>>> = Vec::new();
    for lane in requested {
        match lane {
            "annotate" => {
                let scope = watch::WatchScope::new(repo.clone(), review)?;
                let args = watch::WatchArgs {
                    daemon: daemon.to_string(),
                    scope,
                    json,
                    once,
                    timeout_secs: timeout,
                    backlog,
                    ignore_author: ignore_author.clone(),
                    since,
                    lane_tag: Some("annotate".to_string()),
                };
                tasks.push(tokio::spawn(watch::run(args)));
            }
            "inbox" => {
                let daemon = daemon.to_string();
                tasks.push(tokio::spawn(async move {
                    inbox::run_watch_lane(
                        &daemon,
                        json,
                        interval,
                        backlog,
                        since,
                        once,
                        timeout,
                        Some("inbox"),
                    )
                    .await
                }));
            }
            _ => unreachable!("validated against WATCH_LANES above"),
        }
    }

    let mut first_err: Option<anyhow::Error> = None;
    for task in tasks {
        match task.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) if first_err.is_none() => first_err = Some(e),
            Err(join_err) if first_err.is_none() => {
                first_err = Some(anyhow::anyhow!(
                    "kb-code watch: lane task panicked: {join_err}"
                ))
            }
            _ => {}
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// V71-X1's dead-surface test (the class of defect v7.0 kept shipping —
/// see this unit's report): a lane name that validates against
/// `WATCH_LANES` but has no matching arm in `watch_unified_cmd`'s dispatch
/// `match` falls through to `unreachable!()` and PANICS. Walking every
/// declared lane name end to end (against an address nothing listens on,
/// so each call fails fast on the network round trip rather than hanging)
/// exercises the real dispatch arm for each one; a future lane added to
/// `WATCH_LANES` with no matching arm fails this test LOUDLY (a panic)
/// instead of silently doing nothing the day someone actually runs it.
#[cfg(test)]
mod watch_lane_dispatch_tests {
    use super::*;

    #[tokio::test]
    async fn every_declared_lane_reaches_its_own_dispatch_arm() {
        for lane in WATCH_LANES {
            let lanes = vec![lane.to_string()];
            let err = watch_unified_cmd(
                &lanes,
                None,
                None,
                None,
                false,
                false,
                None,
                Vec::new(),
                30,
                "http://127.0.0.1:1",
                true,
            )
            .await
            .expect_err("an unreachable daemon must fail cleanly, never silently succeed");
            assert!(
                !err.to_string().to_lowercase().contains("unknown lane"),
                "lane {lane:?} must reach a real dispatch arm, not the validation bail: {err}"
            );
        }
    }

    #[tokio::test]
    async fn an_unlisted_lane_name_is_rejected_before_any_dispatch() {
        let err = watch_unified_cmd(
            &["not-a-real-lane".to_string()],
            None,
            None,
            None,
            false,
            false,
            None,
            Vec::new(),
            30,
            "http://127.0.0.1:1",
            true,
        )
        .await
        .expect_err("an unknown lane name must be rejected");
        assert!(err.to_string().contains("unknown lane"));
    }

    #[tokio::test]
    async fn human_mode_is_refused_up_front() {
        let err = watch_unified_cmd(
            &["annotate".to_string()],
            None,
            Some("kb".to_string()),
            None,
            false,
            false,
            None,
            Vec::new(),
            30,
            "http://127.0.0.1:1",
            false,
        )
        .await
        .expect_err("non-JSON output must be refused before any network call");
        assert!(err.to_string().contains("--json"));
    }
}

/// Resolve the repo the SAME way `doctor_cmd` does: `explicit` wins; else
/// the longest configured repo path containing `cwd`; else the sole
/// configured repo; else an error naming every configured repo. Shared by
/// `brief_cmd` — factored out once a second caller needed the identical
/// ladder (`doctor_cmd`'s own inline version stays as a diagnostic, not a
/// hard requirement, so it is NOT rebuilt on this helper).
fn resolve_repo_or_bail(
    repos: &[(String, String)],
    explicit: Option<&str>,
    verb: &str,
) -> Result<String> {
    if let Some(r) = explicit {
        return if repos.iter().any(|(name, _)| name == r) {
            Ok(r.to_string())
        } else {
            Err(anyhow::anyhow!(
                "kb-code {verb}: {r:?} is not a configured repo. Configured: {}",
                repos
                    .iter()
                    .map(|(n, _)| n.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        };
    }
    let cwd = std::env::current_dir().ok();
    let cwd_str = cwd.as_ref().map(|c| c.display().to_string());
    if let Some(name) = cwd_str
        .as_deref()
        .and_then(|c| resolve_repo_for_path(repos, c))
    {
        return Ok(name.to_string());
    }
    if repos.len() == 1 {
        return Ok(repos[0].0.clone());
    }
    Err(anyhow::anyhow!(
        "kb-code {verb}: cwd is not inside a configured repo — pass --repo. Configured: {}",
        repos
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// `kb-code brief` — V71-X1 v0. See `Cmd::Brief`'s doc for scope. No new
/// route: `/api/identity` (repo resolution, same as `doctor`) and
/// `/api/inbox` (`unified-inbox/1`, already fans out over every
/// configured repo) are both existing reads; this filters/re-counts their
/// responses client-side.
async fn brief_cmd(daemon: &str, repo: Option<&str>, json: bool) -> Result<()> {
    let client = http_client()?;
    let identity = get_json(&client, daemon, "/api/identity", &[]).await?;
    let repos: Vec<(String, String)> = identity["repos"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|r| {
            (
                r["name"].as_str().unwrap_or("").to_string(),
                r["path"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    let resolved_repo = resolve_repo_or_bail(&repos, repo, "brief")?;

    let inbox = get_json(&client, daemon, "/api/inbox", &[]).await?;
    let reviews: Vec<serde_json::Value> = inbox["reviews"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r["repo"].as_str() == Some(resolved_repo.as_str()))
        .collect();
    let annotations: Vec<serde_json::Value> = inbox["annotations"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|a| a["repo"].as_str() == Some(resolved_repo.as_str()))
        .collect();
    let questions = annotations
        .iter()
        .filter(|a| a["intent"].as_str() == Some("question"))
        .count();
    let flags_for_agent = annotations
        .iter()
        .filter(|a| a["intent"].as_str() == Some("flag-for-agent"))
        .count();

    let kb_available = inbox["kb"]["available"].as_bool();
    let kb_reason = inbox["kb"]["reason"].as_str();

    // Newest-first, capped — a landing summary, not the full lane dump
    // (`GET /api/inbox`/`kb-code inbox` already exists for that).
    const BRIEF_ITEM_CAP: usize = 5;
    let data = serde_json::json!({
        "repo": resolved_repo,
        "reviews": {
            "count": reviews.len(),
            "items": reviews.iter().take(BRIEF_ITEM_CAP).cloned().collect::<Vec<_>>(),
        },
        "annotations": {
            "count": annotations.len(),
            "questions": questions,
            "flags_for_agent": flags_for_agent,
            "items": annotations.iter().take(BRIEF_ITEM_CAP).cloned().collect::<Vec<_>>(),
        },
        "kb": {
            "available": kb_available,
            "reason": kb_reason,
        },
    });

    if json {
        envelope::print_ok("kbc-brief/1", &data, Vec::new(), false, None);
        return Ok(());
    }

    println!("kb-code brief — {resolved_repo}");
    println!(
        "  {} review(s) awaiting you, {} open annotation(s) ({questions} question(s), \
         {flags_for_agent} flag(s) for you)",
        reviews.len(),
        annotations.len(),
    );
    for r in reviews.iter().take(BRIEF_ITEM_CAP) {
        println!(
            "  review {} {}: {} unanswered question(s), {} unresolved finding(s)",
            r["review_id"].as_i64().unwrap_or(0),
            truncate(r["title"].as_str().unwrap_or("?"), 40),
            r["unanswered_questions"].as_i64().unwrap_or(0),
            r["unresolved_findings"].as_i64().unwrap_or(0),
        );
    }
    for a in annotations.iter().take(BRIEF_ITEM_CAP) {
        println!(
            "  {} {}:{} — {}",
            a["intent"].as_str().unwrap_or("?"),
            a["path"].as_str().unwrap_or("?"),
            a["line"]
                .as_u64()
                .map(|n| n.to_string())
                .unwrap_or_default(),
            truncate(a["excerpt"].as_str().unwrap_or(""), 80),
        );
    }
    match kb_available {
        Some(true) => println!("  kb lane: available"),
        Some(false) => println!(
            "  kb lane: unavailable ({})",
            kb_reason.unwrap_or("unknown")
        ),
        None => println!("  kb lane: absent from response"),
    }
    Ok(())
}

async fn review_impact_cmd(daemon: &str, id: i64, path: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(
        &client,
        daemon,
        &format!("/api/reviews/{id}/impact"),
        &[("path", path)],
    )
    .await?;
    if json {
        envelope::print_ok("review-impact/1", &body, Vec::new(), false, None);
        return Ok(());
    }
    println!("{}", serde_json::to_string_pretty(&body)?);
    Ok(())
}

async fn review_findings_recurrence_cmd(daemon: &str, id: i64, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(
        &client,
        daemon,
        &format!("/api/reviews/{id}/findings/recurrence"),
        &[],
    )
    .await?;
    if json {
        envelope::print_ok(
            "review-findings-recurrence/1",
            &body,
            Vec::new(),
            false,
            None,
        );
        return Ok(());
    }
    let findings = body["findings"].as_array().cloned().unwrap_or_default();
    if findings.is_empty() {
        println!("(none of this review's findings recur elsewhere)");
        return Ok(());
    }
    for f in &findings {
        let prior = f["prior"].as_array().cloned().unwrap_or_default();
        println!(
            "{:<30} seen in {} prior review(s)",
            f["slug"].as_str().unwrap_or("?"),
            f["seen_in_reviews"].as_u64().unwrap_or(prior.len() as u64),
        );
        for p in &prior {
            println!(
                "    #{}  {}",
                p["review_id"].as_i64().unwrap_or(0),
                p["title"].as_str().unwrap_or(""),
            );
        }
    }
    Ok(())
}

async fn pr_reviews_cmd(daemon: &str, repo: &str, number: u64, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(
        &client,
        daemon,
        &format!("/api/prs/{number}/reviews"),
        &[("repo", repo)],
    )
    .await?;
    if json {
        envelope::print_ok("pr-reviews/1", &body, Vec::new(), false, None);
        return Ok(());
    }
    if let Some(r) = body["unavailable_reason"].as_str() {
        println!("(unavailable: {r})");
        return Ok(());
    }
    if let Some(decision) = body["review_decision"].as_str() {
        println!("review decision: {decision}");
    }
    for r in body["reviewers"].as_array().cloned().unwrap_or_default() {
        println!(
            "  {:<20} {}",
            r["login"].as_str().unwrap_or("?"),
            r["state"].as_str().unwrap_or("?"),
        );
    }
    let requested = body["requested_reviewers"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if !requested.is_empty() {
        println!(
            "requested: {}",
            requested
                .iter()
                .filter_map(|r| r.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

async fn symbols_cmd(
    daemon: &str,
    repo: &str,
    path: Option<&str>,
    rev: &str,
    query: Option<&str>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut q: Vec<(&str, &str)> = vec![("repo", repo)];
    match (path, query) {
        (Some(p), None) => {
            q.push(("path", p));
            q.push(("ref", rev));
        }
        (None, Some(needle)) => q.push(("q", needle)),
        (Some(_), Some(_)) => anyhow::bail!("pass exactly one of PATH or --query, not both"),
        (None, None) => anyhow::bail!("pass one of PATH or --query"),
    }
    let body = get_json(&client, daemon, "/api/symbols", &q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if let Some(symbols) = body.get("symbols").and_then(|v| v.as_array()) {
        let path = body["path"].as_str().unwrap_or("");
        for s in symbols {
            print_symbol_line(path, s);
        }
    } else if let Some(matches) = body.get("matches").and_then(|v| v.as_array()) {
        for m in matches {
            let path = m["path"].as_str().unwrap_or("");
            print_symbol_line(path, m);
        }
    }
    Ok(())
}

fn print_symbol_line(path: &str, s: &serde_json::Value) {
    let name = s["name"].as_str().unwrap_or("?");
    let kind = s["kind"].as_str().unwrap_or("?");
    let line = s["line_start"].as_u64().unwrap_or(0);
    match s["container"].as_str() {
        Some(c) => println!("{path}:{line:<6} {kind:<14} {c}::{name}"),
        None => println!("{path}:{line:<6} {kind:<14} {name}"),
    }
}

async fn events_cmd(daemon: &str, json: bool) -> Result<()> {
    sse::tail(daemon, move |frame| print_frame(frame, json)).await
}

/// One line per frame. Mirrors kb-cli's own `kb events`
/// (`crates/kb-cli/src/commands/events.rs::print_frame`/`format_line`).
fn print_frame(frame: &sse::Frame, json: bool) {
    let Some(kind) = frame.event.as_deref() else {
        return; // keep-alive comment frames carry no event
    };
    let raw = frame.data.as_deref().unwrap_or("{}");
    let envelope: serde_json::Value =
        serde_json::from_str(raw).unwrap_or_else(|_| serde_json::json!({ "raw": raw }));
    if json {
        let mut out = serde_json::Map::new();
        if let Some(id) = &frame.id {
            out.insert("id".into(), serde_json::json!(id));
        }
        out.insert("type".into(), serde_json::json!(kind));
        for key in ["ts", "v", "payload", "raw"] {
            if let Some(v) = envelope.get(key) {
                out.insert(key.to_string(), v.clone());
            }
        }
        if !out.contains_key("payload") && !out.contains_key("raw") {
            out.insert("payload".into(), envelope);
        }
        println!("{}", serde_json::Value::Object(out));
    } else {
        let payload = envelope.get("payload").unwrap_or(&envelope);
        println!(
            "{:>8}  {:<24} {}",
            frame.id.as_deref().unwrap_or("-"),
            kind,
            payload
        );
    }
}

// --- search (W2.1) --------------------------------------------------------

async fn search_files_cmd(
    daemon: &str,
    repo: Option<&str>,
    q: &str,
    limit: Option<usize>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut query: Vec<(&str, &str)> = vec![("q", q)];
    if let Some(r) = repo {
        query.push(("repo", r));
    }
    let limit_str = limit.map(|l| l.to_string());
    if let Some(l) = &limit_str {
        query.push(("limit", l));
    }
    let body = get_json(&client, daemon, "/api/search/files", &query).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if let Some(hits) = body.get("hits").and_then(|v| v.as_array()) {
        for h in hits {
            println!(
                "{:>10.1}  {:<16} {}",
                h["score"].as_f64().unwrap_or(0.0),
                h["repo"].as_str().unwrap_or("?"),
                h["path"].as_str().unwrap_or("?"),
            );
        }
    }
    Ok(())
}

async fn search_symbols_cmd(
    daemon: &str,
    repo: Option<&str>,
    q: &str,
    limit: Option<usize>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut query: Vec<(&str, &str)> = vec![("q", q)];
    if let Some(r) = repo {
        query.push(("repo", r));
    }
    let limit_str = limit.map(|l| l.to_string());
    if let Some(l) = &limit_str {
        query.push(("limit", l));
    }
    let body = get_json(&client, daemon, "/api/search/symbols", &query).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if let Some(hits) = body.get("hits").and_then(|v| v.as_array()) {
        for h in hits {
            let repo = h["repo"].as_str().unwrap_or("?");
            let path = h["path"].as_str().unwrap_or("?");
            print_symbol_line(&format!("{repo}:{path}"), h);
        }
    }
    Ok(())
}

async fn search_text_cmd(
    daemon: &str,
    repo: &str,
    q: &str,
    regex: bool,
    case: bool,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let regex_s = regex.to_string();
    let case_s = case.to_string();
    let query = [
        ("repo", repo),
        ("q", q),
        ("regex", regex_s.as_str()),
        ("case", case_s.as_str()),
    ];
    let body = get_json(&client, daemon, "/api/search/text", &query).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if let Some(results) = body.get("results").and_then(|v| v.as_array()) {
        for file in results {
            let path = file["path"].as_str().unwrap_or("?");
            println!("{path}");
            if let Some(matches) = file.get("matches").and_then(|v| v.as_array()) {
                for m in matches {
                    println!(
                        "  {:>6}  {}",
                        m["line_no"].as_u64().unwrap_or(0),
                        m["line"].as_str().unwrap_or(""),
                    );
                }
            }
        }
        if body["truncated"].as_bool().unwrap_or(false) {
            println!("(truncated — result cap reached)");
        }
        if body["time_budget_exceeded"].as_bool().unwrap_or(false) {
            println!("(stopped early — time budget exceeded)");
        }
        // V71-D1b — `scanned`/`total` are additive fields absent from an
        // older daemon's response (`.as_u64()` then reads `None`, so this
        // silently prints nothing rather than a bogus "0/0"); present, they
        // turn a bare empty `results` array into an honest "we didn't get
        // to look at everything" instead of a silent zero.
        if let (Some(scanned), Some(total)) = (body["scanned"].as_u64(), body["total"].as_u64()) {
            if scanned < total {
                println!("(scanned {scanned}/{total} eligible files before stopping)");
            }
        }
    }
    Ok(())
}

// --- search semantic (W2.3) ------------------------------------------------

async fn search_semantic_cmd(
    daemon: &str,
    repo: Option<&str>,
    q: &str,
    limit: Option<u32>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut query: Vec<(&str, &str)> = vec![("q", q)];
    if let Some(r) = repo {
        query.push(("repo", r));
    }
    let limit_str = limit.map(|l| l.to_string());
    if let Some(l) = &limit_str {
        query.push(("limit", l));
    }
    let body = get_json(&client, daemon, "/api/search/semantic", &query).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if let Some(hits) = body.get("hits").and_then(|v| v.as_array()) {
        for h in hits {
            let repo = h["repo"].as_str().unwrap_or("?");
            let path = h["path"].as_str().unwrap_or("?");
            let start = h["span_start"].as_u64().unwrap_or(0);
            let end = h["span_end"].as_u64().unwrap_or(0);
            println!(
                "{:>8.4}  {repo}:{path}:{start}-{end}",
                h["score"].as_f64().unwrap_or(0.0),
            );
            if let Some(snippet) = h["snippet"].as_str() {
                if let Some(header) = snippet.lines().next() {
                    println!("          {header}");
                }
            }
        }
    }
    Ok(())
}

// --- search (unified box, W2.4) --------------------------------------------

/// `kb-code search <q>` — `GET /api/search`, rendered as one headered
/// section per lane the query's prefix/grammar selected (`== <lane> ==`,
/// then that lane's own hit lines — reusing the SAME per-hit line shapes
/// the standalone `search files|symbols|...` commands print, so a user
/// switching between the unified box and a direct lane subcommand sees
/// familiar output). An `unavailable_reason` or `pending` section prints a
/// one-line explanation instead of hits.
/// V71-D1 — the three output knobs `kb-code search <q>` grew for the LLM
/// contract (design D3's "stable ids, budgets, staleness surfaced"). Grouped
/// into one struct rather than three more positional `bool`/`Option`
/// parameters on an already-six-argument fn.
#[derive(Debug, Clone, Default)]
struct SearchOutputOpts {
    /// Appends `explain:1` to the query — see `Cmd::Search`'s field doc for
    /// why this is grammar sugar rather than a wire parameter.
    explain: bool,
    /// Token budget for `--json` (~4 bytes per token).
    budget: Option<usize>,
    /// Per-lane counts only.
    count_only: bool,
    /// V71-D2 — appends `facets:1`.
    facets: bool,
    /// V71-D2 — appends `group:<key>`. Deliberately NOT validated here:
    /// kbcq/1 is TOTAL and has ONE parser, so a bad key comes back as a
    /// named `Diagnostic` from the daemon rather than as a second,
    /// drifting vocabulary check in the CLI.
    group: Option<String>,
}

/// V71-D2 — the ONE place `kb-code search`'s output flags turn into kbcq/1
/// text. Pure, so [`tests::search_flags_write_the_kbcq_clause_they_promise`]
/// can walk every flag against the string it actually produces: the v7.0
/// defect class here was "a verb that never sent the param its route
/// required", and a flag that silently appends nothing is that same bug
/// wearing a friendlier face.
///
/// A clause whose KEY is already in the query is not appended a second time
/// — the author's own token wins, so `kb-code search 'x group:dir'
/// --group file` runs the author's `group:dir` rather than a contradictory
/// pair the grammar would silently resolve by last-one-wins.
fn build_search_query(q: &str, opts: &SearchOutputOpts) -> String {
    let mut out = q.to_string();
    let mut append = |key: &str, clause: String| {
        if out.contains(&format!("{key}:")) {
            return;
        }
        if !out.is_empty() && !out.ends_with(' ') {
            out.push(' ');
        }
        out.push_str(&clause);
    };
    if opts.explain {
        append("explain", "explain:1".to_string());
    }
    if opts.facets {
        append("facets", "facets:1".to_string());
    }
    if let Some(g) = &opts.group {
        append("group", format!("group:{g}"));
    }
    out
}

async fn search_unified_cmd(
    daemon: &str,
    repo: Option<&str>,
    q: &str,
    limit: Option<usize>,
    json: bool,
    opts: SearchOutputOpts,
) -> Result<()> {
    // V71-D1b — a cold daemon's first unified search can take far longer
    // than `http_client`'s 10s default (see `SEARCH_CLIENT_TIMEOUT`'s doc);
    // `get_json_warming_aware` also swaps in an honest "still warming"
    // message for a timeout specifically, rather than the default
    // "is kb-code-server running?".
    let client = client_builder()
        .timeout(SEARCH_CLIENT_TIMEOUT)
        .build()
        .context("build http client")?;
    // `--explain` is kbcq/1 sugar: the daemon's own grammar carries the
    // flag, so the CLI, the SPA's query bar and a saved search all express
    // it the SAME way — one grammar, one string, no second parameter that
    // could disagree with it.
    let q_owned = build_search_query(q, &opts);
    let q = q_owned.as_str();
    let mut query: Vec<(&str, &str)> = vec![("q", q)];
    if let Some(r) = repo {
        query.push(("repo", r));
    }
    let limit_str = limit.map(|l| l.to_string());
    if let Some(l) = &limit_str {
        query.push(("limit", l));
    }
    let body = get_json_warming_aware(&client, daemon, "/api/search", &query).await?;
    if opts.count_only {
        let counts = search_lane_counts(&body);
        if json {
            println!("{}", serde_json::to_string_pretty(&counts)?);
        } else {
            for (lane, n) in counts.iter() {
                println!("{lane:<12} {n}");
            }
        }
        return Ok(());
    }
    if json {
        let (body, dropped) = match opts.budget {
            Some(budget) => apply_search_budget(body, budget),
            None => (body, 0),
        };
        let mut body = body;
        if dropped > 0 {
            // Never a silent cut: the payload says what it dropped and why.
            body["truncated"] = serde_json::json!({ "by": "budget", "omitted": dropped });
        }
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    // The query that actually RAN, plus every token the parser could not
    // honour — a typo must be visible, not silently searched as a word.
    if let Some(normalized) = body.get("normalized").and_then(|v| v.as_str()) {
        if !normalized.is_empty() && normalized != q {
            println!("(ran: {normalized})");
        }
    }
    for d in body
        .get("diagnostics")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
    {
        let msg = d["message"].as_str().unwrap_or("");
        match d.get("suggestion").and_then(|v| v.as_str()) {
            Some(hint) => println!("! {msg} (did you mean `{hint}`?)"),
            None => println!("! {msg}"),
        }
    }

    let Some(sections) = body.get("sections").and_then(|v| v.as_array()) else {
        println!("(no sections — check the query's lane prefix)");
        return Ok(());
    };
    for sec in sections {
        let lane = sec["lane"].as_str().unwrap_or("?");
        println!("== {lane} ==");
        if let Some(reason) = sec.get("unavailable_reason").and_then(|v| v.as_str()) {
            println!("  (unavailable: {reason})");
            continue;
        }
        if sec
            .get("pending")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            println!("  (pending — re-query this lane directly: kb-code search {lane} ...)");
            continue;
        }
        let results = sec.get("results").and_then(|v| v.as_array());
        match results {
            Some(hits) if !hits.is_empty() => match sec.get("groups").and_then(|v| v.as_array()) {
                // V71-D2 — `group:` came back: render the page the way the
                // results page renders it, headers and all, addressing rows
                // by the group's own `indices`. Same bytes, same shape,
                // one grouping computed once server-side.
                Some(groups) => {
                    for g in groups {
                        println!(
                            "  -- {} ({}) --",
                            g["label"].as_str().unwrap_or("?"),
                            g["count"].as_u64().unwrap_or(0)
                        );
                        for idx in g["indices"].as_array().into_iter().flatten() {
                            let Some(i) = idx.as_u64().map(|n| n as usize) else {
                                continue;
                            };
                            if let Some(hit) = hits.get(i) {
                                print_unified_hit(lane, hit);
                            }
                        }
                    }
                }
                None => {
                    for hit in hits {
                        print_unified_hit(lane, hit);
                    }
                }
            },
            _ => println!("  (no hits)"),
        }
        if sec
            .get("truncated")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            println!("  (truncated — more hits exist than shown)");
        }
        if let Some(explain) = sec.get("explain") {
            // Honest by construction: the note the daemon composes says
            // there is no fused score, and this prints it verbatim rather
            // than paraphrasing it into a number.
            println!(
                "  (rank: {} · fusion: {} · factors on: {})",
                explain["rank_basis"].as_str().unwrap_or("?"),
                explain["fusion"].as_str().unwrap_or("?"),
                explain["factors_on"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "none".to_string()),
            );
        }
    }
    print_search_facets(&body);
    if opts.explain {
        // The staleness pair is ALWAYS on the wire (`--json` carries it
        // unconditionally); in human output it earns its line only under
        // `--explain`, because a generation number means nothing to a human
        // until they have two of them to compare.
        if let Some(stale) = body.get("stale") {
            println!(
                "(index generation {} · as of {})",
                stale["generation"].as_u64().unwrap_or(0),
                stale["as_of"].as_str().unwrap_or("?"),
            );
        }
    }
    Ok(())
}

/// V71-D2 — the facet census, human-rendered. Every value prints the kbcq/1
/// CLAUSE that selects it, so the terminal teaches the same query the SPA's
/// facet rail writes — one grammar, one way to say a thing.
fn print_search_facets(body: &serde_json::Value) {
    let Some(facets) = body.get("facets") else {
        return;
    };
    println!(
        "== facets ({}) ==",
        facets["basis"].as_str().unwrap_or("page")
    );
    if let Some(note) = facets["note"].as_str() {
        println!("  ({note})");
    }
    for g in facets["groups"].as_array().into_iter().flatten() {
        let omitted = g["omitted"].as_u64().unwrap_or(0);
        let tail = if omitted > 0 {
            format!("  (+{omitted} more not shown)")
        } else {
            String::new()
        };
        println!("  {}{tail}", g["label"].as_str().unwrap_or("?"));
        for v in g["values"].as_array().into_iter().flatten() {
            println!(
                "    {:>5}  {:<24} {}",
                v["count"].as_u64().unwrap_or(0),
                v["value"].as_str().unwrap_or(""),
                v["clause"].as_str().unwrap_or(""),
            );
        }
    }
}

/// `--count-only`'s payload: `[(lane, hits)]` in the response's own (fixed,
/// never-interleaved) section order — the cheap probe an agent runs before
/// spending a budget on bodies.
fn search_lane_counts(body: &serde_json::Value) -> Vec<(String, usize)> {
    body.get("sections")
        .and_then(|v| v.as_array())
        .map(|sections| {
            sections
                .iter()
                .map(|sec| {
                    let lane = sec["lane"].as_str().unwrap_or("?").to_string();
                    let results = sec.get("results").and_then(|v| v.as_array());
                    // The text lane's `results` is grouped per FILE server-side
                    // (`TextFileResult[]`), so a bare `len()` would answer a
                    // different question there than in every other lane. Count
                    // the MATCHES, which is what the SPA's own `laneRowCount`
                    // counts and what "how big is this result set" means to a
                    // caller deciding whether to spend a budget on it.
                    let n = match (lane.as_str(), results) {
                        ("text", Some(files)) => files
                            .iter()
                            .map(|f| {
                                f.get("matches")
                                    .and_then(|v| v.as_array())
                                    .map(|m| m.len())
                                    .unwrap_or(0)
                            })
                            .sum(),
                        (_, Some(hits)) => hits.len(),
                        (_, None) => 0,
                    };
                    (lane, n)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Roughly how many TOKENS a JSON value costs — `len / 4`, the same
/// four-bytes-per-token rule of thumb kb's own context packer uses. A
/// heuristic, and named as one: the point is a bounded payload an agent can
/// plan around, not an exact accounting of somebody else's tokenizer.
fn approx_tokens(v: &serde_json::Value) -> usize {
    serde_json::to_string(v).map(|s| s.len()).unwrap_or(0) / 4
}

/// Drop whole hits from the END of each lane (fixed lane order, so the
/// lanes that come first keep their hits) until the payload fits `budget`
/// tokens. Returns the trimmed body and the number of hits dropped — the
/// caller reports that count, so a budget cut is never silent.
///
/// Whole hits, never truncated strings: half a snippet is a lie about what
/// the code says, and an agent that re-reads a clipped path wastes exactly
/// the tokens the budget was meant to save.
fn apply_search_budget(mut body: serde_json::Value, budget: usize) -> (serde_json::Value, usize) {
    let mut dropped = 0usize;
    while approx_tokens(&body) > budget {
        // Find the LAST lane that still has a hit to give up.
        let Some(sections) = body.get_mut("sections").and_then(|v| v.as_array_mut()) else {
            break;
        };
        let victim = sections
            .iter_mut()
            .rev()
            .find(|sec| {
                sec.get("results")
                    .and_then(|v| v.as_array())
                    .map(|a| !a.is_empty())
                    .unwrap_or(false)
            })
            .and_then(|sec| sec.get_mut("results"))
            .and_then(|v| v.as_array_mut());
        match victim {
            Some(hits) => {
                hits.pop();
                dropped += 1;
            }
            // Nothing left to drop — the envelope alone is over budget.
            // Returning it whole and saying so beats returning nothing.
            None => break,
        }
    }
    (body, dropped)
}

/// One indented hit line, lane-shaped — see [`search_unified_cmd`]'s doc.
fn print_unified_hit(lane: &str, h: &serde_json::Value) {
    match lane {
        "files" => println!(
            "  {:>10.1}  {:<16} {}",
            h["score"].as_f64().unwrap_or(0.0),
            h["repo"].as_str().unwrap_or("?"),
            h["path"].as_str().unwrap_or("?"),
        ),
        "symbols" => {
            let repo = h["repo"].as_str().unwrap_or("?");
            let path = h["path"].as_str().unwrap_or("?");
            print_symbol_line(&format!("  {repo}:{path}"), h);
        }
        "text" => {
            let path = h["path"].as_str().unwrap_or("?");
            println!("  {path}");
            if let Some(matches) = h.get("matches").and_then(|v| v.as_array()) {
                for m in matches {
                    println!(
                        "    {:>6}  {}",
                        m["line_no"].as_u64().unwrap_or(0),
                        m["line"].as_str().unwrap_or(""),
                    );
                }
            }
        }
        "semantic" => {
            let repo = h["repo"].as_str().unwrap_or("?");
            let path = h["path"].as_str().unwrap_or("?");
            let start = h["span_start"].as_u64().unwrap_or(0);
            let end = h["span_end"].as_u64().unwrap_or(0);
            println!(
                "  {:>8.4}  {repo}:{path}:{start}-{end}",
                h["score"].as_f64().unwrap_or(0.0),
            );
        }
        "sessions" => println!(
            "  {:>8.4}  {:<24} {}",
            h["score"].as_f64().unwrap_or(0.0),
            h["session_id"].as_str().unwrap_or("?"),
            h["title"].as_str().unwrap_or("?"),
        ),
        "transcripts" => {
            let kind = h["kind"].as_str().unwrap_or("?");
            println!(
                "  {:<12} {:<20} {}",
                kind,
                h["session_id"].as_str().unwrap_or("?"),
                h["project_dir"].as_str().unwrap_or("?"),
            );
            println!("      {}", h["snippet"].as_str().unwrap_or(""));
        }
        _ => println!("  {h}"),
    }
}

// --- transcripts (W2.5) ----------------------------------------------------

async fn search_transcripts_cmd(
    daemon: &str,
    q: &str,
    session: Option<&str>,
    kind: Option<&str>,
    limit: Option<usize>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut query: Vec<(&str, &str)> = vec![("q", q)];
    if let Some(s) = session {
        query.push(("session", s));
    }
    if let Some(k) = kind {
        query.push(("kind", k));
    }
    let limit_str = limit.map(|l| l.to_string());
    if let Some(l) = &limit_str {
        query.push(("limit", l));
    }
    let body = get_json(&client, daemon, "/api/search/transcripts", &query).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if let Some(hits) = body.get("hits").and_then(|v| v.as_array()) {
        for h in hits {
            let kind = h["kind"].as_str().unwrap_or("?");
            let tool = h["tool_name"].as_str();
            let kind_label = match tool {
                Some(t) => format!("{kind}:{t}"),
                None => kind.to_string(),
            };
            let sidechain = if h["is_sidechain"].as_bool().unwrap_or(false) {
                " [sidechain]"
            } else {
                ""
            };
            println!(
                "{:<12} {:<20} {}{}",
                kind_label,
                h["session_id"].as_str().unwrap_or("?"),
                h["project_dir"].as_str().unwrap_or("?"),
                sidechain,
            );
            println!("    {}", h["snippet"].as_str().unwrap_or(""));
        }
        if hits.is_empty() {
            println!("(no hits)");
        }
    }
    Ok(())
}

async fn transcripts_status_cmd(daemon: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(&client, daemon, "/api/transcripts/status", &[]).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!("enabled: {}", body["enabled"].as_bool().unwrap_or(false));
    println!("root:    {}", body["root"].as_str().unwrap_or("?"));
    println!("files:   {}", body["files"].as_u64().unwrap_or(0));
    println!("turns:   {}", body["turns"].as_u64().unwrap_or(0));
    println!(
        "indexed: {} bytes",
        body["indexed_bytes"].as_u64().unwrap_or(0)
    );
    Ok(())
}

// --- blame / timeline (W3.1) ------------------------------------------------

async fn blame_cmd(
    daemon: &str,
    repo: &str,
    path: &str,
    rev: Option<&str>,
    line_range: Option<(u32, u32)>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut q: Vec<(&str, &str)> = vec![("repo", repo), ("path", path)];
    if let Some(r) = rev {
        q.push(("ref", r));
    }
    let (start_s, end_s);
    if let Some((s, e)) = line_range {
        start_s = s.to_string();
        end_s = e.to_string();
        q.push(("start", &start_s));
        q.push(("end", &end_s));
    }
    let body = get_json(&client, daemon, "/api/blame", &q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if body["dirty"].as_bool().unwrap_or(false) {
        println!("(uncommitted edits present — blaming the live working-tree content)");
    }
    if let Some(regions) = body.get("regions").and_then(|v| v.as_array()) {
        for r in regions {
            let sha = r["sha"].as_str().unwrap_or("");
            let short_sha = &sha[..sha.len().min(8)];
            let author = truncate(r["author"].as_str().unwrap_or("?"), 16);
            let final_start = r["final_start"].as_u64().unwrap_or(0);
            let count = r["count"].as_u64().unwrap_or(0);
            let subject = r["subject"].as_str().unwrap_or("");
            println!("{short_sha}  {author:<16} L{final_start:<6} (+{count:<3}) {subject}");
        }
    }
    Ok(())
}

/// Truncate `s` to at most `max` chars (UTF-8-aware), appending `…` when it
/// was cut — keeps `blame_cmd`'s fixed-width author column from wrapping a
/// long display name across the whole line.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    } else {
        s.to_string()
    }
}

async fn timeline_cmd(
    daemon: &str,
    repo: &str,
    path: &str,
    line: u32,
    max: Option<usize>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let line_s = line.to_string();
    let mut q: Vec<(&str, &str)> = vec![("repo", repo), ("path", path), ("line", &line_s)];
    let max_s = max.map(|m| m.to_string());
    if let Some(m) = &max_s {
        q.push(("max", m));
    }
    let body = get_json(&client, daemon, "/api/blame/timeline", &q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if let Some(entries) = body.get("entries").and_then(|v| v.as_array()) {
        if entries.is_empty() {
            println!("(no history for this line)");
        }
        for e in entries {
            let sha = e["sha"].as_str().unwrap_or("");
            let short_sha = &sha[..sha.len().min(8)];
            let ts = e["author_time"].as_i64().unwrap_or(0);
            let subject = e["subject"].as_str().unwrap_or("");
            println!("{short_sha}  {ts:<12} {subject}");
        }
    }
    Ok(())
}

/// `kb-code join <sha> --repo <name>` — `GET /api/join/commit`'s join/1
/// body. The human view prints every field the response actually carries
/// (the enrichment fields are independently optional — see
/// `kb_code_server::join::ladder::Attribution`'s doc), never a placeholder
/// for one that's absent.
async fn join_cmd(daemon: &str, repo: &str, sha: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(
        &client,
        daemon,
        "/api/join/commit",
        &[("repo", repo), ("sha", sha)],
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!("sha:        {}", body["sha"].as_str().unwrap_or("?"));
    println!("confidence: {}", body["confidence"].as_str().unwrap_or("?"));
    println!("via:        {}", body["via"].as_str().unwrap_or("?"));
    if let Some(v) = body["session_id"].as_str() {
        println!("session_id: {v}");
    }
    if let Some(v) = body["kb"].as_str() {
        println!("kb:         {v}");
    }
    if let Some(v) = body["display_name"].as_str() {
        println!("title:      {v}");
    }
    if let Some(v) = body["started_at"].as_i64() {
        println!("started_at: {v}");
    }
    Ok(())
}

// --- why / story / provenance-report (W3.3 + W3.4) --------------------------

/// `kb-code why <PATH>[:<LINE>]` — `GET /api/why`. Two response shapes,
/// distinguished by presence of the top-level `line` field (see
/// `kb_code_server::provenance::why`'s module doc): line-grade (a `region`
/// plus `attribution` plus an optional `kb_context`) or file-grade (a
/// ranked `sessions` list plus `uncommitted_lines`).
async fn why_cmd(
    daemon: &str,
    repo: &str,
    path: &str,
    line: Option<u32>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut q: Vec<(&str, &str)> = vec![("repo", repo), ("path", path)];
    let line_s = line.map(|l| l.to_string());
    if let Some(l) = &line_s {
        q.push(("line", l));
    }
    let body = get_json(&client, daemon, "/api/why", &q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    if body.get("line").is_some() {
        print_line_why(&body);
    } else {
        print_file_why(&body);
    }
    Ok(())
}

fn print_line_why(body: &serde_json::Value) {
    let region = &body["region"];
    let attribution = &body["attribution"];
    println!("line:        {}", body["line"].as_u64().unwrap_or(0));
    let sha = region["sha"].as_str().unwrap_or("?");
    println!("region sha:  {}", &sha[..sha.len().min(12)]);
    println!("subject:     {}", region["subject"].as_str().unwrap_or(""));
    println!("author:      {}", region["author"].as_str().unwrap_or("?"));
    println!(
        "confidence:  {}",
        attribution["confidence"].as_str().unwrap_or("?")
    );
    println!(
        "via:         {}",
        attribution["via"].as_str().unwrap_or("?")
    );
    if let Some(sid) = attribution["session_id"].as_str() {
        println!("session_id:  {sid}");
    }
    if let Some(ids) = attribution.get("session_ids").and_then(|v| v.as_array()) {
        if !ids.is_empty() {
            let list: Vec<&str> = ids.iter().filter_map(|v| v.as_str()).collect();
            println!("session_ids: {}", list.join(", "));
        }
    }
    if let Some(dn) = attribution["display_name"].as_str() {
        println!("title:       {dn}");
    }
    if let Some(ctx) = body.get("kb_context") {
        if let Some(prompt) = ctx["prompt_excerpt"].as_str() {
            println!("prompt:      {}", truncate(prompt, 100));
        }
        if let Some(decisions) = ctx.get("decisions").and_then(|v| v.as_array()) {
            for d in decisions {
                println!(
                    "  decision [{}]: {}",
                    d["kind"].as_str().unwrap_or("?"),
                    truncate(d["prompt"].as_str().unwrap_or(""), 80),
                );
            }
        }
    }
    let timeline_available = body["timeline_available"].as_bool().unwrap_or(false);
    println!(
        "timeline:    {}",
        if timeline_available {
            "available"
        } else {
            "n/a"
        }
    );
}

fn print_file_why(body: &serde_json::Value) {
    println!("why · {}", body["path"].as_str().unwrap_or("?"));
    if let Some(sessions) = body.get("sessions").and_then(|v| v.as_array()) {
        if sessions.is_empty() {
            println!("  (no attributed sessions/commits)");
        }
        for s in sessions {
            let ident = s["session_id"]
                .as_str()
                .or_else(|| s["display_name"].as_str())
                .unwrap_or("?");
            println!(
                "  {:<24} {:<10} {:<14} {:>4} line(s) / {:>3} region(s)  {}",
                truncate(ident, 24),
                s["confidence"].as_str().unwrap_or("?"),
                s["via"].as_str().unwrap_or("?"),
                s["lines"].as_u64().unwrap_or(0),
                s["regions"].as_u64().unwrap_or(0),
                s["display_name"].as_str().unwrap_or(""),
            );
        }
    }
    let uncommitted = body["uncommitted_lines"].as_u64().unwrap_or(0);
    if uncommitted > 0 {
        println!("  ({uncommitted} uncommitted line(s), not attributed to any session)");
    }
}

/// `kb-code story <PATH> [--symbol]` — `GET /api/story`.
async fn story_cmd(
    daemon: &str,
    repo: &str,
    path: &str,
    symbol: Option<&str>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut q: Vec<(&str, &str)> = vec![("repo", repo), ("path", path)];
    if let Some(s) = symbol {
        q.push(("symbol", s));
    }
    let body = get_json(&client, daemon, "/api/story", &q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let header = match body["symbol"].as_str() {
        Some(s) => format!("story · {}::{s}", body["path"].as_str().unwrap_or("?")),
        None => format!("story · {}", body["path"].as_str().unwrap_or("?")),
    };
    println!("{header}");
    if let Some(entries) = body.get("entries").and_then(|v| v.as_array()) {
        if entries.is_empty() {
            println!("  (no history)");
        }
        for e in entries {
            // CT-E2 — the attention-gap beat gets its own visually distinct
            // line instead of masquerading as a session row.
            if e["status"].as_str() == Some("gap") {
                println!("  {}", story_gap_line(e));
                continue;
            }
            let ident = e["session_id"]
                .as_str()
                .or_else(|| e["sha"].as_str())
                .unwrap_or("?");
            let label = e["display_name"]
                .as_str()
                .or_else(|| e["subject"].as_str())
                .unwrap_or("");
            println!(
                "  {:<12} {:<24} {:<10} {:<12} {:>4} line(s)  {}",
                e["first_seen"].as_i64().unwrap_or(0),
                truncate(ident, 24),
                e["confidence"].as_str().unwrap_or("?"),
                e["status"].as_str().unwrap_or("?"),
                e["lines_touched"].as_u64().unwrap_or(0),
                truncate(label, 60),
            );
        }
    }
    Ok(())
}

/// One `status: "gap"` story entry (CT-E2's attention-gap beat —
/// `kb_code_server::provenance::story`'s module doc) rendered as a muted
/// divider line. Timestamps stay raw unix seconds, this CLI's recorded
/// convention (no date-formatting dependency); `reason` distinguishes the
/// honest "no captured session recorded" from "session join unavailable"
/// (kb unreachable/disabled — coverage unknown, not known-absent).
fn story_gap_line(e: &serde_json::Value) -> String {
    let count = e["commit_count"].as_u64().unwrap_or(1);
    let commits = if count == 1 { "commit" } else { "commits" };
    let first = e["first_seen"].as_i64().unwrap_or(0);
    let last = e["last_seen"].as_i64().unwrap_or(first);
    let range = if last == first {
        format!("{first}")
    } else {
        format!("{first}..{last}")
    };
    let what = match e["reason"].as_str() {
        Some("join-unavailable") => "session join unavailable",
        _ => "no captured session",
    };
    format!("— {what} for {count} {commits} ({range}) —")
}

/// `kb-code provenance-report` — `GET /api/provenance-report`.
async fn provenance_report_cmd(
    daemon: &str,
    repo: &str,
    max_count: Option<usize>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut q: Vec<(&str, &str)> = vec![("repo", repo)];
    let max_s = max_count.map(|m| m.to_string());
    if let Some(m) = &max_s {
        q.push(("max_count", m));
    }
    let body = get_json(&client, daemon, "/api/provenance-report", &q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    println!(
        "provenance report · {}",
        body["repo"].as_str().unwrap_or("?")
    );
    let truncated = if body["truncated"].as_bool().unwrap_or(false) {
        ", possibly truncated"
    } else {
        ""
    };
    println!(
        "  {} commit(s) walked (max_count={}{truncated})",
        body["total_commits"].as_u64().unwrap_or(0),
        body["max_count"].as_u64().unwrap_or(0),
    );
    println!();
    println!("  by confidence:");
    print_buckets(&body["by_confidence"]);
    println!();
    println!("  by via:");
    print_buckets(&body["by_via"]);
    if let Some(weeks) = body
        .get("trailer_coverage_by_week")
        .and_then(|v| v.as_array())
    {
        if !weeks.is_empty() {
            println!();
            println!("  trailer coverage by week:");
            for w in weeks {
                println!(
                    "    {:<10} {:>4}/{:<4}  ({:>5.1}%)",
                    w["week"].as_str().unwrap_or("?"),
                    w["trailer_commits"].as_u64().unwrap_or(0),
                    w["commits"].as_u64().unwrap_or(0),
                    w["pct"].as_f64().unwrap_or(0.0),
                );
            }
        }
    }
    if let Some(era) = body.get("capture_era") {
        println!();
        println!(
            "  capture-era subset (since {}, {} commit(s)):",
            era["start"].as_i64().unwrap_or(0),
            era["commit_count"].as_u64().unwrap_or(0),
        );
        println!("    by confidence:");
        print_buckets(&era["by_confidence"]);
        println!("    by via:");
        print_buckets(&era["by_via"]);
    }
    Ok(())
}

fn print_buckets(v: &serde_json::Value) {
    if let Some(buckets) = v.as_array() {
        for b in buckets {
            println!(
                "    {:<16} {:>6}  ({:>5.1}%)",
                b["label"].as_str().unwrap_or("?"),
                b["count"].as_u64().unwrap_or(0),
                b["pct"].as_f64().unwrap_or(0.0),
            );
        }
    }
}

// --- session-diff (W3.5) ------------------------------------------------

/// `kb-code session-diff <sid>` — `GET /api/session-diff`'s session-diff/1
/// body (W3.5). The human view renders `segments` in the session's own
/// narrative order: a `prompt` segment as a header line, a `commits`
/// segment as one line per commit (short sha + numstat summary, or "not
/// locally diffed" for one kb knows about but this daemon couldn't diff),
/// an `uncommitted` segment as a bulleted file list. `--json` prints the
/// raw body instead.
async fn session_diff_cmd(
    daemon: &str,
    session: &str,
    repo: Option<&str>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut q: Vec<(&str, &str)> = vec![("session", session)];
    if let Some(r) = repo {
        q.push(("repo", r));
    }
    let body = get_json(&client, daemon, "/api/session-diff", &q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if let Some(name) = body["display_name"].as_str() {
        println!("session:    {name}");
    }
    println!("session_id: {session}");
    if let Some(status) = body["commits_status"]["status"].as_str() {
        if status != "ok" {
            let reason = body["commits_status"]["reason"].as_str().unwrap_or("?");
            println!("commits:    DEGRADED ({reason})");
        }
    }
    let repos = body["repos_touched"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    println!("repos:      {repos}");
    let t = &body["totals"];
    println!(
        "totals:     {} commits ({} diffed), {} files, +{}/-{}",
        t["commits"].as_u64().unwrap_or(0),
        t["commits_diffed"].as_u64().unwrap_or(0),
        t["files"].as_u64().unwrap_or(0),
        t["insertions"].as_u64().unwrap_or(0),
        t["deletions"].as_u64().unwrap_or(0),
    );
    println!();

    let segments = body["segments"].as_array().cloned().unwrap_or_default();
    if segments.is_empty() {
        println!("(no segments)");
    }
    for seg in &segments {
        match seg["kind"].as_str().unwrap_or("?") {
            "prompt" => {
                let text = seg["text"].as_str().unwrap_or("");
                println!("### {text}");
            }
            "commits" => {
                for c in seg["commits"].as_array().cloned().unwrap_or_default() {
                    let sha = c["sha"].as_str().unwrap_or("");
                    let short_sha = &sha[..sha.len().min(8)];
                    let subject = c["subject"].as_str().unwrap_or("(no subject)");
                    if c["diffed"].as_bool().unwrap_or(false) {
                        let ins = c["insertions"].as_u64().unwrap_or(0);
                        let del = c["deletions"].as_u64().unwrap_or(0);
                        let nfiles = c["files"].as_array().map(|a| a.len()).unwrap_or(0);
                        println!(
                            "  commit {short_sha}  {subject}  ({nfiles} files, +{ins}/-{del})"
                        );
                    } else {
                        println!("  commit {short_sha}  {subject}  (not locally diffed)");
                    }
                }
            }
            "uncommitted" => {
                println!("  uncommitted:");
                for f in seg["files"].as_array().cloned().unwrap_or_default() {
                    if let Some(path) = f.as_str() {
                        println!("    - {path}");
                    }
                }
            }
            other => println!("  (unknown segment kind: {other})"),
        }
    }
    Ok(())
}

// --- backfill (W3.6) -------------------------------------------------------

/// `kb-code backfill [--repo NAME] [--json]` — `POST /api/backfill?repo=`
/// (W3.6): the join ladder's precompute. With `--repo`, runs (and prints)
/// exactly that repo's stats. Without it, enumerates every repo the daemon
/// is configured to browse (`GET /api/repos`) and runs the precompute over
/// each in turn, one `POST` per repo — the "or one" contract this verb's own
/// doc promises. Exits non-zero ONLY when kb-code-server itself is
/// unreachable (a hard failure — [`get_json`]/[`post_json`]'s own
/// `.context` surfaces that); an unreachable/disabled FEDERATED kb daemon
/// is a `degraded: true` stat in an otherwise-successful response, never a
/// failure (the trailer arm still resolves purely locally).
async fn backfill_cmd(daemon: &str, repo: Option<&str>, json: bool) -> Result<()> {
    let client = http_client()?;
    let repos: Vec<String> = match repo {
        Some(r) => vec![r.to_string()],
        None => {
            let body = get_json(&client, daemon, "/api/repos", &[]).await?;
            body["repos"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|r| r["name"].as_str().map(str::to_string))
                .collect()
        }
    };
    if repos.is_empty() {
        println!("(no configured repos)");
        return Ok(());
    }

    let mut all = Vec::with_capacity(repos.len());
    for r in &repos {
        let body = post_json(&client, daemon, "/api/backfill", &[("repo", r.as_str())]).await?;
        all.push(body);
    }

    if json {
        if all.len() == 1 {
            println!("{}", serde_json::to_string_pretty(&all[0])?);
        } else {
            println!("{}", serde_json::to_string_pretty(&all)?);
        }
        return Ok(());
    }
    for body in &all {
        print_backfill_stats(body);
    }
    Ok(())
}

async fn hotspots_cmd(
    daemon: &str,
    repo: &str,
    limit: Option<usize>,
    scope: Option<&str>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut q: Vec<(&str, String)> = vec![("repo", repo.to_string())];
    if let Some(n) = limit {
        q.push(("limit", n.to_string()));
    }
    if let Some(s) = scope {
        q.push(("scope", s.to_string()));
    }
    let qref: Vec<(&str, &str)> = q.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let body = get_json(&client, daemon, "/api/behavioral/hotspots", &qref).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "hotspots · {}  (total {}{})",
        body["repo"].as_str().unwrap_or(repo),
        body["total"].as_u64().unwrap_or(0),
        if body["truncated"].as_bool().unwrap_or(false) {
            ", truncated"
        } else {
            ""
        }
    );
    println!(
        "{:<8} {:<8} {:<6} {:<10} {:<8} {:<8} path",
        "score", "churn_rk", "cx_rk", "churn", "revs", "age_d"
    );
    for it in body["items"].as_array().cloned().unwrap_or_default() {
        let score = it["hotspot"]["score"].as_f64().unwrap_or(0.0);
        let cr = it["hotspot"]["terms"]["churn_rank"].as_u64().unwrap_or(0);
        let xr = it["hotspot"]["terms"]["complexity_rank"]
            .as_u64()
            .unwrap_or(0);
        let churn = it["churn"].as_i64().unwrap_or(0);
        let revs = it["revisions"].as_i64().unwrap_or(0);
        let age = it["age_days"]
            .as_f64()
            .map(|d| format!("{d:.0}"))
            .unwrap_or_else(|| "-".into());
        let path = it["path"].as_str().unwrap_or("?");
        println!("{score:<8.4} {cr:<8} {xr:<6} {churn:<10} {revs:<8} {age:<8} {path}");
    }
    Ok(())
}

async fn coupling_cmd(
    daemon: &str,
    repo: &str,
    path: &str,
    limit: Option<usize>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut q: Vec<(&str, String)> = vec![("repo", repo.to_string()), ("path", path.to_string())];
    if let Some(n) = limit {
        q.push(("limit", n.to_string()));
    }
    let qref: Vec<(&str, &str)> = q.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let body = get_json(&client, daemon, "/api/behavioral/coupling", &qref).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "coupling · {} → {}  (partners {}; conf = co/revs(path), asymmetric)",
        body["repo"].as_str().unwrap_or(repo),
        body["path"].as_str().unwrap_or(path),
        body["total"].as_u64().unwrap_or(0),
    );
    println!(
        "{:<10} {:<10} {:<10} partner",
        "confidence", "support", "co_commits"
    );
    for p in body["partners"].as_array().cloned().unwrap_or_default() {
        println!(
            "{:<10.4} {:<10} {:<10} {}",
            p["confidence"].as_f64().unwrap_or(0.0),
            p["support"].as_i64().unwrap_or(0),
            p["co_commits"].as_i64().unwrap_or(0),
            p["path"].as_str().unwrap_or("?"),
        );
    }
    Ok(())
}

async fn owners_cmd(daemon: &str, repo: &str, path: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(
        &client,
        daemon,
        "/api/behavioral/ownership",
        &[("repo", repo), ("path", path)],
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "owners · {} → {}  (total_commits={} ownership={:.3} fragmentation={:.3} major={} minor={})",
        body["repo"].as_str().unwrap_or(repo),
        body["path"].as_str().unwrap_or(path),
        body["total_commits"].as_i64().unwrap_or(0),
        body["ownership"].as_f64().unwrap_or(0.0),
        body["fragmentation"].as_f64().unwrap_or(0.0),
        body["major"].as_u64().unwrap_or(0),
        body["minor"].as_u64().unwrap_or(0),
    );
    println!("{:<10} {:<8} author", "share", "commits");
    for a in body["authors"].as_array().cloned().unwrap_or_default() {
        println!(
            "{:<10.3} {:<8} {}",
            a["share"].as_f64().unwrap_or(0.0),
            a["commits"].as_i64().unwrap_or(0),
            a["author"].as_str().unwrap_or("?"),
        );
    }
    Ok(())
}

async fn age_cmd(daemon: &str, repo: &str, path: &str, json: bool) -> Result<()> {
    // blame can be slow on large files
    let client = client_builder()
        .timeout(Duration::from_secs(120))
        .build()
        .context("build http client")?;
    let body = get_json(
        &client,
        daemon,
        "/api/behavioral/age",
        &[("repo", repo), ("path", path)],
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "age · {} → {}  (lines={} median_age_days={:.1})",
        body["repo"].as_str().unwrap_or(repo),
        body["path"].as_str().unwrap_or(path),
        body["lines"].as_u64().unwrap_or(0),
        body["median_age_days"].as_f64().unwrap_or(0.0),
    );
    println!("{:<8} bucket", "lines");
    for b in body["buckets"].as_array().cloned().unwrap_or_default() {
        println!(
            "{:<8} {}",
            b["lines"].as_u64().unwrap_or(0),
            b["label"].as_str().unwrap_or("?"),
        );
    }
    Ok(())
}

async fn recipes_catalog_cmd(daemon: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(&client, daemon, "/api/recipes", &[]).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "recipes · schema {}",
        body["schema"].as_str().unwrap_or("?")
    );
    println!("{:<24} {:<6} {:<16} description", "name", "ver", "params");
    for r in body["recipes"].as_array().cloned().unwrap_or_default() {
        let name = r["name"].as_str().unwrap_or("?");
        let ver = r["recipe_version"].as_u64().unwrap_or(0);
        let params: Vec<String> = r["params"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|p| {
                let n = p["name"].as_str()?;
                let req = p["required"].as_bool().unwrap_or(false);
                Some(if req { format!("{n}*") } else { n.to_string() })
            })
            .collect();
        let params_s = if params.is_empty() {
            "-".to_string()
        } else {
            params.join(",")
        };
        let desc = r["description"].as_str().unwrap_or("");
        // Truncate long descriptions for the table.
        let desc_short: String = desc.chars().take(72).collect();
        println!("{name:<24} {ver:<6} {params_s:<16} {desc_short}");
    }
    Ok(())
}

async fn recipe_run_cmd(
    daemon: &str,
    name: &str,
    repo: &str,
    since: Option<&str>,
    limit: Option<usize>,
    scope: Option<&str>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut q: Vec<(&str, String)> = vec![("repo", repo.to_string())];
    if let Some(s) = since {
        q.push(("since", s.to_string()));
    }
    if let Some(n) = limit {
        q.push(("limit", n.to_string()));
    }
    if let Some(s) = scope {
        q.push(("scope", s.to_string()));
    }
    let qref: Vec<(&str, &str)> = q.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let path = format!("/api/recipes/{name}");
    let body = get_json(&client, daemon, &path, &qref).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let missing = body["inputs_missing"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let missing_s: Vec<&str> = missing.iter().filter_map(|v| v.as_str()).collect();
    println!(
        "recipe · {} v{} · {}  (total {}{}; inputs_missing=[{}])",
        body["recipe"].as_str().unwrap_or(name),
        body["recipe_version"].as_u64().unwrap_or(0),
        body["repo"].as_str().unwrap_or(repo),
        body["total"].as_u64().unwrap_or(0),
        if body["truncated"].as_bool().unwrap_or(false) {
            ", truncated"
        } else {
            ""
        },
        missing_s.join(", "),
    );
    if let Some(note) = body["note"].as_str() {
        println!("note: {note}");
    }
    // Generic table: path first when present, then score/symbol.
    println!("{:<8} {:<28} {:<20} detail", "score", "path", "symbol");
    for it in body["items"].as_array().cloned().unwrap_or_default() {
        let score = it["score"]
            .as_f64()
            .or_else(|| it["score"].as_i64().map(|n| n as f64))
            .map(|s| format!("{s:.4}"))
            .unwrap_or_else(|| "-".into());
        let path = it["path"].as_str().unwrap_or("-");
        let symbol = it["symbol"].as_str().unwrap_or("-");
        let class = it["class"].as_str().unwrap_or("");
        let terms = it.get("terms").map(|t| t.to_string()).unwrap_or_default();
        let detail = if !class.is_empty() && !terms.is_empty() {
            format!("class={class} {terms}")
        } else if !class.is_empty() {
            format!("class={class}")
        } else {
            terms
        };
        println!("{score:<8} {path:<28} {symbol:<20} {detail}");
    }
    Ok(())
}

async fn behavioral_backfill_cmd(daemon: &str, repo: Option<&str>, json: bool) -> Result<()> {
    let client = client_builder()
        .timeout(Duration::from_secs(600))
        .build()
        .context("build http client")?;
    let repos: Vec<String> = match repo {
        Some(r) => vec![r.to_string()],
        None => {
            let body = get_json(&client, daemon, "/api/repos", &[]).await?;
            body["repos"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|r| r["name"].as_str().map(str::to_string))
                .collect()
        }
    };
    if repos.is_empty() {
        println!("(no configured repos)");
        return Ok(());
    }
    let mut all = Vec::with_capacity(repos.len());
    for r in &repos {
        let body = post_json(
            &client,
            daemon,
            "/api/behavioral/backfill",
            &[("repo", r.as_str())],
        )
        .await?;
        all.push(body);
    }
    if json {
        if all.len() == 1 {
            println!("{}", serde_json::to_string_pretty(&all[0])?);
        } else {
            println!("{}", serde_json::to_string_pretty(&all)?);
        }
        return Ok(());
    }
    for body in &all {
        println!(
            "behavioral backfill · {}  commits={} path_touches={} cochange_updates={} full_rebuild={} {}ms last={}",
            body["repo"].as_str().unwrap_or("?"),
            body["commits"].as_u64().unwrap_or(0),
            body["path_touches"].as_u64().unwrap_or(0),
            body["cochange_updates"].as_u64().unwrap_or(0),
            body["full_rebuild"].as_bool().unwrap_or(false),
            body["duration_ms"].as_u64().unwrap_or(0),
            body["last_commit_sha"].as_str().unwrap_or("-"),
        );
    }
    Ok(())
}

/// V3.4-C1 — `GET /api/behavioral/timeseries`.
async fn behavioral_timeseries_cmd(
    daemon: &str,
    repo: &str,
    path: Option<&str>,
    weeks: Option<u32>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut q: Vec<(&str, String)> = vec![("repo", repo.to_string())];
    if let Some(p) = path {
        q.push(("path", p.to_string()));
    }
    if let Some(w) = weeks {
        q.push(("weeks", w.to_string()));
    }
    let qref: Vec<(&str, &str)> = q.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let body = get_json(&client, daemon, "/api/behavioral/timeseries", &qref).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "timeseries · {}  weeks={}  buckets={}{}",
        body["repo"].as_str().unwrap_or(repo),
        body["weeks"].as_u64().unwrap_or(0),
        body["buckets"].as_array().map(|a| a.len()).unwrap_or(0),
        if body["truncated"].as_bool().unwrap_or(false) {
            "  truncated"
        } else {
            ""
        },
    );
    if let Some(note) = body["note"].as_str() {
        println!("note: {note}");
    }
    println!(
        "{:<14} {:<8} {:<10} {:<8}",
        "week_start", "commits", "churn", "authors"
    );
    for b in body["buckets"].as_array().cloned().unwrap_or_default() {
        println!(
            "{:<14} {:<8} {:<10} {:<8}",
            b["week_start_unix"].as_i64().unwrap_or(0),
            b["commits"].as_u64().unwrap_or(0),
            b["churn"].as_i64().unwrap_or(0),
            b["authors"].as_u64().unwrap_or(0),
        );
    }
    Ok(())
}

/// V3.4-C1 — `GET /api/canvas?repo=` (list only; no create/edit verbs).
async fn canvas_list_cmd(daemon: &str, repo: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(&client, daemon, "/api/canvas", &[("repo", repo)]).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let items = body["items"].as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        println!("(no canvas sets in {repo})");
        return Ok(());
    }
    println!(
        "{:<6} {:<24} {:<10} {:<12} {:<10}",
        "id", "name", "review_id", "updated", "bytes"
    );
    for it in items {
        let review = it["review_id"]
            .as_i64()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".into());
        println!(
            "{:<6} {:<24} {:<10} {:<12} {:<10}",
            it["id"].as_i64().unwrap_or(0),
            it["name"].as_str().unwrap_or("?"),
            review,
            it["updated_unix"].as_i64().unwrap_or(0),
            it["payload_bytes"].as_i64().unwrap_or(0),
        );
    }
    Ok(())
}

// --- DCB W1.C — `kb-code doclens {show,repos,pin}` -------------------------

/// Status-preserving `GET` — the doc-lens routes answer `{"error", "reason"}`
/// on every non-2xx (R12) and the whole point of the `reason` field is that a
/// client can render the DEGRADE STATE, not just an HTTP number. `get_json`'s
/// `error_for_status` would throw that body away.
async fn get_json_raw(
    client: &reqwest::Client,
    daemon: &str,
    path: &str,
    query: &[(&str, &str)],
) -> Result<(reqwest::StatusCode, serde_json::Value)> {
    let url = format!("{}{path}", daemon.trim_end_matches('/'));
    let resp = client
        .get(&url)
        .query(query)
        .send()
        .await
        .with_context(|| format!("GET {url} — is kb-code-server running at {daemon}?"))?;
    let status = resp.status();
    let body = resp
        .json()
        .await
        .with_context(|| format!("parse {url} response as JSON"))?;
    Ok((status, body))
}

/// `PUT` sibling of [`post_json_raw`] — `doclens pin` is the only PUT this
/// CLI drives.
async fn put_json_raw(
    client: &reqwest::Client,
    daemon: &str,
    path: &str,
    body: &serde_json::Value,
) -> Result<(reqwest::StatusCode, serde_json::Value)> {
    let url = format!("{}{path}", daemon.trim_end_matches('/'));
    let resp = client
        .put(&url)
        .json(body)
        .send()
        .await
        .with_context(|| format!("PUT {url} — is kb-code-server running at {daemon}?"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    Ok((status, json_body_or_null(&text)))
}

/// The `{"error", "reason"}` body every non-2xx doc-lens route returns,
/// formatted for a human. `repo_required` additionally prints the scorecard
/// invocation that answers it — the CLI never picks a checkout for you.
fn doclens_api_error(
    status: reqwest::StatusCode,
    body: &serde_json::Value,
    kb: &str,
    doc: &str,
) -> anyhow::Error {
    let msg = body["error"].as_str().unwrap_or("(no error message)");
    let reason = body["reason"].as_str().unwrap_or("http_error");
    let mut out = format!("doclens: {reason} — {msg} (HTTP {})", status.as_u16());
    if reason == "repo_required" {
        out.push_str(&format!(
            "\n  pick one:  kb-code doclens repos --kb {kb} --doc {doc}"
        ));
    }
    anyhow::anyhow!(out)
}

fn doclens_line_cell(r: &serde_json::Value) -> String {
    // W1.C renders three tiers; W2.A adds a fourth keyed on `line_evidence`
    // (`rev_remap`), which is why this reads the EVIDENCE and not just the
    // state even though only one evidence value exists today.
    let state = r["line_state"].as_str().unwrap_or("-");
    let evidence = r["line_evidence"].as_str().unwrap_or("none");
    let delta = r["line_hint_delta"].as_i64().unwrap_or(0);
    match (state, evidence) {
        // `remap_outcome` (kb-code-server) only ever ships `rev_remap` on a
        // `confirmed` line_state — `drifted`+`rev_remap` is not a reachable
        // combination — and an IDENTITY remap (delta 0) is still
        // git-verified evidence with nothing to report as "moved". Parity
        // with the SPA's `LineBadge` (`web-code/src/components/lens/
        // RefRow.tsx`), which gates its "✓ moved … · git-verified" badge on
        // `delta !== 0` and falls back to a plain "✓ confirmed" otherwise.
        ("confirmed", "rev_remap") if delta != 0 => format!("moved {delta:+} (git)"),
        ("confirmed", _) => "confirmed".to_string(),
        ("drifted", _) => format!("drifted {delta:+}"),
        ("unverifiable", _) => "unverifiable".to_string(),
        _ => "—".to_string(),
    }
}

fn doclens_path_cell(r: &serde_json::Value) -> String {
    match r["path_state"].as_str() {
        Some("present") => "present".to_string(),
        Some("ambiguous") => format!("ambig×{}", r["candidate_count"].as_u64().unwrap_or(0)),
        Some("absent") => "absent".to_string(),
        Some("external") => "external".to_string(),
        _ if r["issue"].is_object() => "issue".to_string(),
        _ => "—".to_string(),
    }
}

async fn doclens_show_cmd(
    daemon: &str,
    kb: &str,
    doc: &str,
    repo: Option<&str>,
    group: Option<&str>,
    state: Option<&str>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut query: Vec<(&str, &str)> = vec![("kb", kb), ("doc", doc)];
    if let Some(r) = repo {
        query.push(("repo", r));
    }
    let (status, body) = get_json_raw(&client, daemon, "/api/doc-lens", &query).await?;
    if !status.is_success() {
        return Err(doclens_api_error(status, &body, kb, doc));
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    let repo_obj = &body["repo"];
    let head = repo_obj["head_sha"].as_str().unwrap_or("");
    let short = if head.len() >= 9 { &head[..9] } else { head };
    let dirty = if repo_obj["dirty"].as_bool().unwrap_or(false) {
        " · DIRTY (uncommitted working tree — no sha reproduces this)"
    } else {
        ""
    };
    println!(
        "{} · repo {} @ {} ({}){}",
        body["doc_title"].as_str().unwrap_or("(untitled)"),
        repo_obj["name"].as_str().unwrap_or("?"),
        if short.is_empty() { "—" } else { short },
        repo_obj["source"].as_str().unwrap_or("?"),
        dirty
    );
    if body["never_scanned"].as_bool().unwrap_or(false) {
        println!("\nnot scanned yet — `kb reindex --kb {kb}` to populate.");
        println!("(this is NOT the same as \"no code refs\")");
        return Ok(());
    }
    let c = &body["counts"];
    let n = |k: &str| c[k].as_u64().unwrap_or(0);
    println!(
        "resolved {} · {} refs · {} present / {} ambiguous / {} absent / {} external",
        body["resolved_unix"].as_i64().unwrap_or(0),
        n("total"),
        n("present"),
        n("ambiguous"),
        n("absent"),
        n("external")
    );
    println!(
        "                              · {} confirmed / {} drifted / {} unverifiable",
        n("confirmed"),
        n("drifted"),
        n("unverifiable")
    );
    if body["truncated"].as_bool().unwrap_or(false) {
        println!("  (truncated — showing the first {} refs)", n("resolved"));
    }
    if body["partial"].as_bool().unwrap_or(false) {
        println!("  (PARTIAL — the per-request deadline expired; some line states are unresolved)");
    }

    let refs = body["refs"].as_array().cloned().unwrap_or_default();
    let mut labels: Vec<(String, String)> = body["groups"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|g| {
            (
                g["key"].as_str().unwrap_or("").to_string(),
                g["label"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    labels.push((String::new(), "(ungrouped)".to_string()));

    for (key, label) in labels {
        let rows: Vec<&serde_json::Value> = refs
            .iter()
            .filter(|r| {
                let g = r["group"].as_str().unwrap_or("");
                g == key
                    && group.is_none_or(|want| want == g)
                    && state.is_none_or(|want| r["path_state"].as_str() == Some(want))
            })
            .collect();
        if rows.is_empty() {
            continue;
        }
        println!("\n§ {label}");
        for r in rows {
            let target = r["resolved_path"]
                .as_str()
                .map(|p| match r["resolved_line"].as_u64() {
                    Some(l) => format!("{p}:{l}"),
                    None => p.to_string(),
                })
                .unwrap_or_else(|| r["raw"].as_str().unwrap_or("?").to_string());
            println!(
                "  {:<9} {:<14}  {}",
                doclens_path_cell(r),
                doclens_line_cell(r),
                target
            );
            let token = r["confirm_token"]
                .as_str()
                .map(|t| format!("   [{t}]"))
                .unwrap_or_default();
            println!(
                "                            ← {}{}",
                r["raw"].as_str().unwrap_or("?"),
                token
            );
            if let Some(reason) = r["line_reason"].as_str() {
                if r["line_state"].as_str() == Some("unverifiable") {
                    println!("                              ({reason})");
                }
            }
            if let Some(s) = r["search"].as_object() {
                println!(
                    "                            → kb-code search \"{}\" --repo {}",
                    s["q"].as_str().unwrap_or(""),
                    s["repo"].as_str().unwrap_or("")
                );
            }
            if let Some(i) = r["issue"].as_object() {
                println!(
                    "                            → {}",
                    i["href"].as_str().unwrap_or("")
                );
            }
            if let Some(note) = r["note"].as_str() {
                println!("                              ({note})");
            }
        }
    }
    let ungrouped = body["ungrouped_count"].as_u64().unwrap_or(0);
    if ungrouped > 0 {
        println!("\n({ungrouped} refs before the first heading)");
    }
    Ok(())
}

async fn doclens_repos_cmd(daemon: &str, kb: &str, doc: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let (status, body) = get_json_raw(
        &client,
        daemon,
        "/api/doc-lens/repos",
        &[("kb", kb), ("doc", doc)],
    )
    .await?;
    if !status.is_success() {
        return Err(doclens_api_error(status, &body, kb, doc));
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "{}   ({} refs, resolved {})",
        body["doc_title"].as_str().unwrap_or("(untitled)"),
        body["counted_refs"].as_u64().unwrap_or(0),
        body["resolved_unix"].as_i64().unwrap_or(0)
    );
    if body["never_scanned"].as_bool().unwrap_or(false) {
        println!("  not scanned yet — `kb reindex --kb {kb}` to populate.");
    }
    let pinned = body["pinned_repo"].as_str();
    println!();
    println!(
        "  {:<16}{:<11}{:<12}{:<7}{:<9}{:<7}{:<8}EXT",
        "REPO", "STATE", "HEAD", "DIRTY", "PRESENT", "AMBIG", "ABSENT"
    );
    for r in body["repos"].as_array().cloned().unwrap_or_default() {
        let name = r["name"].as_str().unwrap_or("?");
        let mark = if Some(name) == pinned { "*" } else { " " };
        let head = r["head_sha"].as_str().unwrap_or("");
        let cell = |k: &str| {
            r[k].as_u64()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "—".into())
        };
        println!(
            "{mark} {:<16}{:<11}{:<12}{:<7}{:<9}{:<7}{:<8}{}",
            name,
            r["state"].as_str().unwrap_or("?"),
            if head.len() >= 9 { &head[..9] } else { "—" },
            match r["dirty"].as_bool() {
                Some(true) => "dirty",
                Some(false) => "",
                None => "—",
            },
            cell("present"),
            cell("ambiguous"),
            cell("absent"),
            cell("external"),
        );
        if let Some(reason) = r["reason"].as_str() {
            println!("{:>52}(\"{reason}\")", "");
        }
    }
    println!();
    println!("  * = pinned.  Pick one:  kb-code doclens show --kb {kb} --doc {doc} --repo <NAME>");
    Ok(())
}

async fn doclens_pin_cmd(daemon: &str, kb: &str, doc: &str, repo: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let (status, body) = put_json_raw(
        &client,
        daemon,
        "/api/doc-lens/pin",
        &serde_json::json!({ "kb": kb, "doc": doc, "repo": repo }),
    )
    .await?;
    if !status.is_success() {
        return Err(doclens_api_error(status, &body, kb, doc));
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "pinned {kb}/{doc} → {} ({})",
        body["repo"].as_str().unwrap_or(repo),
        body["repo_root"].as_str().unwrap_or("?")
    );
    Ok(())
}

// --- DCB W2.A — `kb-code doclens {pins,unpin}` -----------------------------

/// `DELETE` sibling of [`get_json_raw`]. Deliberately reads the body as TEXT
/// first: a successful unpin is `204 No Content` (empty body), while every
/// failure carries the `{error, reason}` JSON — `resp.json()` would turn the
/// success case into a parse error.
async fn delete_json_raw(
    client: &reqwest::Client,
    daemon: &str,
    path: &str,
    query: &[(&str, &str)],
) -> Result<(reqwest::StatusCode, serde_json::Value)> {
    let url = format!("{}{path}", daemon.trim_end_matches('/'));
    let resp = client
        .delete(&url)
        .query(query)
        .send()
        .await
        .with_context(|| format!("DELETE {url} — is kb-code-server running at {daemon}?"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let body = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    Ok((status, body))
}

async fn doclens_pins_list(
    client: &reqwest::Client,
    daemon: &str,
    kb: Option<&str>,
) -> Result<serde_json::Value> {
    let mut query: Vec<(&str, &str)> = Vec::new();
    if let Some(k) = kb {
        query.push(("kb", k));
    }
    let (status, body) = get_json_raw(client, daemon, "/api/doc-lens/pins", &query).await?;
    if !status.is_success() {
        return Err(doclens_api_error(status, &body, kb.unwrap_or("-"), "-"));
    }
    Ok(body)
}

async fn doclens_pins_cmd(daemon: &str, kb: Option<&str>, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = doclens_pins_list(&client, daemon, kb).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let pins = body["pins"].as_array().cloned().unwrap_or_default();
    if pins.is_empty() {
        println!("no remembered checkouts. Pin one:  kb-code doclens pin --kb <KB> --doc <DOC> --repo <NAME>");
        return Ok(());
    }
    println!("{:<12}{:<16}{:<16}PINNED", "KB", "DOC", "REPO");
    for p in &pins {
        // Raw unix seconds, this CLI's own convention for every timestamp it
        // renders (`resolved_unix`, `updated_unix`, `week_start_unix`) —
        // kb-code-cli carries no date-formatting dependency and W2.A adds
        // none.
        println!(
            "{:<12}{:<16}{:<16}{}",
            p["kb"].as_str().unwrap_or("?"),
            p["doc_id"].as_str().unwrap_or("?"),
            p["repo"].as_str().unwrap_or("?"),
            p["pinned_at"].as_i64().unwrap_or(0)
        );
        // After a boot prune these never fire; they exist so a pin that went
        // stale WITHIN one daemon lifetime is legible rather than silent.
        if !p["repo_configured"].as_bool().unwrap_or(true) {
            println!("            ↳ repo is no longer configured — this pin will be dropped");
        } else if !p["root_matches"].as_bool().unwrap_or(true) {
            println!(
                "            ↳ recorded root {} is no longer what this repo resolves to",
                p["repo_root"].as_str().unwrap_or("?")
            );
        }
    }
    Ok(())
}

async fn doclens_unpin_cmd(daemon: &str, kb: &str, doc: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    // Asked BEFORE the delete purely so the human output can be honest: the
    // route answers `204` either way (idempotent by design), so there is no
    // other way to distinguish "forgotten" from "there was nothing to
    // forget". A forget verb that FAILED because there was nothing to forget
    // would be the worse contract, so neither case is an error.
    let existed = doclens_pins_list(&client, daemon, Some(kb))
        .await
        .ok()
        .and_then(|b| b["pins"].as_array().cloned())
        .is_some_and(|pins| pins.iter().any(|p| p["doc_id"].as_str() == Some(doc)));

    let (status, body) = delete_json_raw(
        &client,
        daemon,
        "/api/doc-lens/pin",
        &[("kb", kb), ("doc", doc)],
    )
    .await?;
    if !status.is_success() {
        return Err(doclens_api_error(status, &body, kb, doc));
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "kb": kb, "doc": doc, "unpinned": existed
            }))?
        );
        return Ok(());
    }
    if existed {
        println!("unpinned {kb}/{doc}");
    } else {
        println!("no pin for {kb}/{doc}");
    }
    Ok(())
}

// --- DCB W3.A — `kb-code doclens sync` -------------------------------------

/// Render one `doclens-sync/1` body. Every counter is a SKIP an operator can
/// act on, so each one that is non-zero gets its own line rather than being
/// folded into a total — a pass that resolved nothing because the cap was
/// spent and one that resolved nothing because every doc is unpinned are
/// different situations, and the point of printing this at all is to tell
/// them apart.
fn print_doclens_sync_stats(body: &serde_json::Value) -> String {
    let n = |k: &str| body[k].as_u64().unwrap_or(0);
    let mut out = format!(
        "doc-lens sync · {} kb(s) walked · {} doc(s) resolved{}",
        n("kbs_synced"),
        n("docs_resolved"),
        if body["forced"].as_bool().unwrap_or(false) {
            " (--force: cursors reset)"
        } else {
            ""
        }
    );
    for (key, label) in [
        ("docs_skipped_unpinned", "unpinned (never fetched)"),
        ("docs_skipped_cap", "skipped — batch_cap spent"),
        ("docs_dropped_404", "dropped — kb 404s the doc"),
        (
            "docs_skipped_not_allowlisted",
            "skipped — kb not in [doclens] kbs",
        ),
    ] {
        if n(key) > 0 {
            out.push_str(&format!("\n  {:>5}  {label}", n(key)));
        }
    }
    for e in body["errors"].as_array().cloned().unwrap_or_default() {
        out.push_str(&format!("\n  ERROR  {}", e.as_str().unwrap_or("?")));
    }
    out
}

async fn doclens_sync_cmd(daemon: &str, force: bool, json: bool) -> Result<()> {
    let client = http_client()?;
    let (status, body) = post_json_raw(
        &client,
        daemon,
        "/api/doc-lens/sync",
        &serde_json::json!({ "force": force }),
    )
    .await?;
    if !status.is_success() {
        // The route is loopback-only, so the most likely non-2xx here is a
        // 404 from the gate rather than a doc-lens `reason` — say so.
        if status == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!(
                "doclens sync: POST /api/doc-lens/sync answered 404 — that route is \
                 LOOPBACK-ONLY, so this only works against a daemon on this machine \
                 (--daemon {daemon})"
            );
        }
        return Err(doclens_api_error(status, &body, "-", "-"));
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!("{}", print_doclens_sync_stats(&body));
    Ok(())
}

fn print_backfill_stats(body: &serde_json::Value) {
    let degraded = if body["degraded"].as_bool().unwrap_or(false) {
        " (DEGRADED — federated kb daemon unreachable/disabled; trailer arm still resolved)"
    } else {
        ""
    };
    println!("backfill · {}", body["repo"].as_str().unwrap_or("?"));
    println!(
        "  {} commit(s) walked in {}ms{degraded}",
        body["total"].as_u64().unwrap_or(0),
        body["duration_ms"].as_u64().unwrap_or(0),
    );
    if let Some(buckets) = body["resolved_by_confidence"].as_array() {
        for b in buckets {
            println!(
                "    {:<10} {:>6}",
                b["confidence"].as_str().unwrap_or("?"),
                b["count"].as_u64().unwrap_or(0),
            );
        }
    }
    println!(
        "  newly_cached: {}  upgraded: {}",
        body["newly_cached"].as_u64().unwrap_or(0),
        body["upgraded"].as_u64().unwrap_or(0),
    );
}

// --- annotations (W4.6; D3 — full CLI parity for annotations v2) -----------

/// `POST` counterpart to [`post_json`] that sends a JSON BODY rather than
/// query params, RETAINING the response body on a non-2xx status (unlike
/// [`post_json`]'s blanket `error_for_status()`, which discards it) — every
/// annotation-creation kind (D3's `--symbol`/`--sha`/`--to` in particular)
/// needs its daemon-side validation failure's `{"error": …}` text rendered
/// as a friendly message, not swallowed into an opaque reqwest error.
/// Mirrors `checkout_cmd`'s own manual status-check style.
async fn post_json_raw(
    client: &reqwest::Client,
    daemon: &str,
    path: &str,
    body: &serde_json::Value,
) -> Result<(reqwest::StatusCode, serde_json::Value)> {
    let url = format!("{}{path}", daemon.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .json(body)
        .send()
        .await
        .with_context(|| format!("POST {url} — is kb-code-server running at {daemon}?"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    Ok((status, json_body_or_null(&text)))
}

/// `PATCH` sibling of [`post_json_raw`] — every `annotate`
/// {resolve,reopen,edit,set-intent} lifecycle verb PATCHes
/// `/api/annotations/{id}` and needs the same non-2xx body-preserving
/// contract.
async fn patch_json_raw(
    client: &reqwest::Client,
    daemon: &str,
    id: &str,
    body: &serde_json::Value,
) -> Result<(reqwest::StatusCode, serde_json::Value)> {
    let url = format!("{}/api/annotations/{id}", daemon.trim_end_matches('/'));
    let resp = client
        .patch(&url)
        .json(body)
        .send()
        .await
        .with_context(|| format!("PATCH {url} — is kb-code-server running at {daemon}?"))?;
    let status = resp.status();
    let body = resp
        .json()
        .await
        .with_context(|| format!("parse {url} response as JSON"))?;
    Ok((status, body))
}

/// The uniform `{"error": "..."}` body every non-2xx route in this crate's
/// daemon returns (`kb_code_server::routes::ApiError`), formatted for a
/// human. `what` names the failed action, e.g. `"create annotation"` or
/// `"resolve annotation \"ann_xyz\""`.
fn annotation_api_error(
    what: &str,
    status: reqwest::StatusCode,
    body: &serde_json::Value,
) -> anyhow::Error {
    let msg = body["error"].as_str().unwrap_or("(no error message)");
    anyhow::anyhow!("{what} failed ({status}): {msg}")
}

/// Empty / non-JSON HTTP bodies become `Null` so a loopback-gate 404
/// (bare status, no `{"error":…}`) is distinguishable from a JSON
/// not-found.
fn json_body_or_null(text: &str) -> serde_json::Value {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return serde_json::Value::Null;
    }
    serde_json::from_str(trimmed).unwrap_or(serde_json::Value::Null)
}

/// A LOOPBACK-ONLY route answers a bare 404 (empty body) off-loopback.
/// A real not-found carries `{"error":…}`. Never render the gate as a
/// bare 404.
fn loopback_or_api_error(
    what: &str,
    daemon: &str,
    status: reqwest::StatusCode,
    body: &serde_json::Value,
) -> anyhow::Error {
    if status == reqwest::StatusCode::NOT_FOUND
        && body.get("error").and_then(|v| v.as_str()).is_none()
    {
        return anyhow::anyhow!(
            "{what} requires loopback — {daemon} answered 404 \
             (this route is LOOPBACK-ONLY)"
        );
    }
    annotation_api_error(what, status, body)
}

/// Client-side gate for `--intent`/`set-intent <INTENT>` — the SAME vocab
/// check the daemon itself runs (`kb_code_server::annotations::
/// is_valid_intent`), reused rather than duplicated, so catching a typo
/// here can never drift from what the daemon would 400 on. Pure + daemon-
/// free, so it's covered by a plain unit test below.
fn require_valid_intent(intent: &str) -> Result<()> {
    if kb_code_server::annotations::is_valid_intent(intent) {
        Ok(())
    } else {
        anyhow::bail!(
            "invalid intent {intent:?} — expected one of: note, question, todo, \
             flag-for-agent, tour-stop"
        )
    }
}

/// `kb-code annotations <PATH> --repo NAME [--all]` — `GET
/// /api/annotations?repo=&path=` (W4.6; D3 upgrades the display): every
/// annotation on `PATH`, threaded (replies indented under their parent, in
/// creation order — matching `list_annotations`' own `ORDER BY created_at
/// ASC, id ASC`), resolved rows hidden unless `all`. `--json` returns the
/// server's response verbatim, unfiltered — the filtering here is a human-
/// output convenience only.
async fn annotations_cmd(
    daemon: &str,
    repo: &str,
    path: &str,
    all: bool,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let body = get_json(
        &client,
        daemon,
        "/api/annotations",
        &[("repo", repo), ("path", path)],
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let anns = body["annotations"].as_array().cloned().unwrap_or_default();
    if anns.is_empty() {
        println!("(no annotations on {path})");
        return Ok(());
    }

    // Partition into top-level rows + each parent's replies, preserving the
    // server's own creation-order sort — a reply always sorts after its
    // parent (its `created_at` is later), so a stable single pass groups
    // correctly even when replies to different parents interleave.
    let mut top: Vec<&serde_json::Value> = Vec::new();
    let mut replies: std::collections::HashMap<&str, Vec<&serde_json::Value>> =
        std::collections::HashMap::new();
    for a in &anns {
        match a["parent_id"].as_str() {
            Some(pid) => replies.entry(pid).or_default().push(a),
            None => top.push(a),
        }
    }

    let mut shown_any = false;
    for a in &top {
        if !all && a["resolved"].as_bool().unwrap_or(false) {
            continue;
        }
        print_annotation_row(a, "");
        shown_any = true;
        if let Some(id) = a["id"].as_str() {
            if let Some(rs) = replies.get(id) {
                for r in rs {
                    if !all && r["resolved"].as_bool().unwrap_or(false) {
                        continue;
                    }
                    print_annotation_row(r, "    ");
                }
            }
        }
    }
    if !shown_any {
        println!("(no open annotations on {path} — pass --all to include resolved)");
    }
    Ok(())
}

/// `kb-code annotations open --repo R [--intent I] [--path-prefix P]` — D3:
/// `GET /api/annotations/open` verbatim — the repo-wide, UNRESOLVED,
/// TOP-LEVEL listing (replies excluded server-side; each row carries its
/// own `reply_count`). `--json` is this verb's primary consumer (the D4
/// hook shells out to it), so its shape stays exactly the server's own.
async fn annotations_open_cmd(
    daemon: &str,
    repo: &str,
    intent: Option<&str>,
    path_prefix: Option<&str>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut query: Vec<(&str, &str)> = vec![("repo", repo)];
    if let Some(i) = intent {
        query.push(("intent", i));
    }
    if let Some(p) = path_prefix {
        query.push(("path_prefix", p));
    }
    let body = get_json(&client, daemon, "/api/annotations/open", &query).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let anns = body["annotations"].as_array().cloned().unwrap_or_default();
    if anns.is_empty() {
        println!("(no open annotations)");
        return Ok(());
    }
    for a in &anns {
        let path = a["path"].as_str().unwrap_or("?");
        let replies = a["reply_count"].as_i64().unwrap_or(0);
        print_annotation_row(a, "");
        println!("    {path}{}", reply_suffix(replies));
    }
    if body["truncated"].as_bool().unwrap_or(false) {
        println!("… truncated — narrow with --intent/--path-prefix to see the rest");
    }
    Ok(())
}

fn reply_suffix(count: i64) -> String {
    match count {
        0 => String::new(),
        1 => "  (1 reply)".to_string(),
        n => format!("  ({n} replies)"),
    }
}

/// The `anchor-kind` column for one annotation row — see
/// `kb_code_server::routes::AnnotationView`'s doc for the wire fields this
/// reads. A REPLY (`parent_id` set) has no anchor of its own (`line`/
/// `stale` mirror its parent's), so it never reaches this fn — callers
/// branch on `parent_id` first (see [`print_annotation_row`]).
///
/// `"symbol"` is the one kind this can't render richly: the wire
/// `AnnotationView` carries no `anchor2`/`SymbolDescriptor` (no symbol
/// NAME travels over HTTP at all, only the resolved current `line`) — see
/// that struct's own field list. Rather than inventing a fake name, this
/// degrades to the same `L{line}` a plain `line` annotation gets, which is
/// honest about what the CLI actually knows.
fn anchor_kind_chip(a: &serde_json::Value) -> String {
    let line = a["line"].as_u64().unwrap_or(0);
    match a["anchor_kind"].as_str().unwrap_or("line") {
        "range" => {
            let end = a["line_end"].as_u64().unwrap_or(line);
            format!("L{line}\u{2013}{end}")
        }
        "diff" => {
            let sha = a["sha"].as_str().unwrap_or("");
            sha.chars().take(8).collect()
        }
        // "symbol" and the v1 default "line" both land here — see the doc.
        _ => format!("L{line}"),
    }
}

fn intent_chip(a: &serde_json::Value) -> String {
    format!("[{}]", a["intent"].as_str().unwrap_or("note"))
}

/// One annotation (or reply) row, human-formatted: `id  [intent]
/// anchor-or-↳  [STALE] (resolved)  author: body`. `indent` is `""` for a
/// top-level row, `"    "` for a reply (the threaded-display convention —
/// see `annotations_cmd`'s doc).
fn print_annotation_row(a: &serde_json::Value, indent: &str) {
    let id = a["id"].as_str().unwrap_or("?");
    let is_reply = a["parent_id"].is_string();
    let anchor_col = if is_reply {
        "\u{21b3}".to_string()
    } else {
        anchor_kind_chip(a)
    };
    let stale = if a["stale"].as_bool().unwrap_or(false) {
        " [STALE]"
    } else {
        ""
    };
    let resolved = if a["resolved"].as_bool().unwrap_or(false) {
        " (resolved)"
    } else {
        ""
    };
    let author = a["author"].as_str().unwrap_or("?");
    let body = a["body"].as_str().unwrap_or("");
    println!(
        "{indent}{id}  {}  {anchor_col}{stale}{resolved}  {author}: {body}",
        intent_chip(a)
    );
}

/// `kb-code annotate <PATH>:<LINE> -m <body> --repo NAME [--to END|
/// --symbol|--sha REV] [--intent I] [--review ID [--ps N] [--side new|old]]`
/// — `POST /api/annotations` (W4.6; D3 adds the kind flags + intent; V70-A3X
/// adds `--review`/`--ps`/`--side`, wiring `CreateAnnotationBody`'s
/// pre-existing `review_id`/`ps`/`side` fields — the create body already
/// accepted them, but no CLI flag ever set them, so a review-scoped
/// annotation was only reachable via a raw HTTP call). Create an annotation
/// anchored to `LINE`'s current content (`line`/`range`/`symbol`) or a
/// historical blob (`diff`). At most one of `to`/`symbol`/`sha` is set —
/// clap's `conflicts_with_all` on the `Cmd::Annotate` fields already
/// enforces this before we ever get here; `--ps`/`--side` similarly
/// `requires = "review"` (checked by clap) since they're meaningless
/// without a review to scope against.
#[allow(clippy::too_many_arguments)]
async fn annotate_create_cmd(
    daemon: &str,
    repo: &str,
    path: &str,
    line: u32,
    message: &str,
    to: Option<u32>,
    symbol: bool,
    sha: Option<&str>,
    intent: Option<&str>,
    review: Option<i64>,
    ps: Option<i64>,
    side: Option<&str>,
    json: bool,
) -> Result<()> {
    if let Some(i) = intent {
        require_valid_intent(i)?;
    }

    let mut payload = serde_json::json!({
        "repo": repo,
        "path": path,
        "line": line,
        "body": message,
    });
    if let Some(end) = to {
        payload["anchor_kind"] = serde_json::json!(kb_code_server::annotations::ANCHOR_KIND_RANGE);
        payload["line_end"] = serde_json::json!(end);
    } else if symbol {
        payload["anchor_kind"] = serde_json::json!(kb_code_server::annotations::ANCHOR_KIND_SYMBOL);
    } else if let Some(s) = sha {
        payload["anchor_kind"] = serde_json::json!(kb_code_server::annotations::ANCHOR_KIND_DIFF);
        payload["sha"] = serde_json::json!(s);
    }
    if let Some(i) = intent {
        payload["intent"] = serde_json::json!(i);
    }
    if let Some(r) = review {
        payload["review_id"] = serde_json::json!(r);
    }
    if let Some(p) = ps {
        payload["ps"] = serde_json::json!(p);
    }
    if let Some(s) = side {
        payload["side"] = serde_json::json!(s);
    }

    let client = http_client()?;
    let (status, body) = post_json_raw(&client, daemon, "/api/annotations", &payload).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if status != reqwest::StatusCode::CREATED {
        if symbol && status == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!(
                "no enclosing symbol at {path}:{line} — annotate the line instead \
                 (drop --symbol)"
            );
        }
        return Err(annotation_api_error("create annotation", status, &body));
    }
    if !json {
        print!("✓ created ");
        print_annotation_row(&body, "");
    }
    Ok(())
}

/// `kb-code annotate reply <ID> -m BODY --repo R --path P [--intent I]` —
/// D3: `POST /api/annotations` with `parent_id` set. See `AnnotateCmd::
/// Reply`'s doc for why `--repo`/`--path` are required here (unlike every
/// other lifecycle verb).
#[allow(clippy::too_many_arguments)]
async fn annotate_reply_cmd(
    daemon: &str,
    id: &str,
    repo: &str,
    path: &str,
    message: &str,
    intent: Option<&str>,
    json: bool,
) -> Result<()> {
    if let Some(i) = intent {
        require_valid_intent(i)?;
    }
    let mut payload = serde_json::json!({
        "repo": repo,
        "path": path,
        "body": message,
        "parent_id": id,
    });
    if let Some(i) = intent {
        payload["intent"] = serde_json::json!(i);
    }
    let client = http_client()?;
    let (status, body) = post_json_raw(&client, daemon, "/api/annotations", &payload).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if status != reqwest::StatusCode::CREATED {
        return Err(annotation_api_error(
            &format!("reply to {id:?}"),
            status,
            &body,
        ));
    }
    if !json {
        print!("✓ replied ");
        print_annotation_row(&body, "");
    }
    Ok(())
}

/// Shared PATCH body for `resolve`/`reopen`/`edit`/`set-intent` — send
/// `payload`, print the updated row (or the JSON verbatim), and turn a
/// non-2xx status into a friendly [`annotation_api_error`].
async fn annotate_patch_cmd(
    daemon: &str,
    id: &str,
    payload: serde_json::Value,
    what: &str,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let (status, body) = patch_json_raw(&client, daemon, id, &payload).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if !status.is_success() {
        return Err(annotation_api_error(
            &format!("{what} annotation {id:?}"),
            status,
            &body,
        ));
    }
    if !json {
        print!("✓ {what} ");
        print_annotation_row(&body, "");
    }
    Ok(())
}

/// `kb-code annotate resolve <ID>` — D3: `PATCH {resolved:true}`.
async fn annotate_resolve_cmd(daemon: &str, id: &str, json: bool) -> Result<()> {
    annotate_patch_cmd(
        daemon,
        id,
        serde_json::json!({ "resolved": true }),
        "resolved",
        json,
    )
    .await
}

/// `kb-code annotate reopen <ID>` — D3: `PATCH {resolved:false}`.
async fn annotate_reopen_cmd(daemon: &str, id: &str, json: bool) -> Result<()> {
    annotate_patch_cmd(
        daemon,
        id,
        serde_json::json!({ "resolved": false }),
        "reopened",
        json,
    )
    .await
}

/// `kb-code annotate edit <ID> -m BODY` — D3: `PATCH {body:BODY}`.
async fn annotate_edit_cmd(daemon: &str, id: &str, message: &str, json: bool) -> Result<()> {
    annotate_patch_cmd(
        daemon,
        id,
        serde_json::json!({ "body": message }),
        "edited",
        json,
    )
    .await
}

/// `kb-code annotate set-intent <ID> <INTENT>` — D3: `PATCH {intent:
/// INTENT}`, `INTENT` vocab-gated client-side before the round trip.
async fn annotate_set_intent_cmd(daemon: &str, id: &str, intent: &str, json: bool) -> Result<()> {
    require_valid_intent(intent)?;
    annotate_patch_cmd(
        daemon,
        id,
        serde_json::json!({ "intent": intent }),
        "set intent on",
        json,
    )
    .await
}

/// `kb-code annotate delete <ID> [--yes]` — D3: `DELETE
/// /api/annotations/{id}` (cascades to any replies). Prompts on stderr
/// unless `--yes` (mirrors `kb reset`'s own confirm convention: `eprint!`
/// then `io::stderr().flush()` then a bare stdin `read_line`, defaulting to
/// "no" on anything but an explicit `y`/`Y`).
async fn annotate_delete_cmd(daemon: &str, id: &str, yes: bool, json: bool) -> Result<()> {
    if !yes {
        eprint!("delete annotation {id:?}? this cascades to any replies. [y/N] ");
        std::io::stderr().flush().ok();
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).ok();
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("aborted");
            return Ok(());
        }
    }

    let client = http_client()?;
    let url = format!("{}/api/annotations/{id}", daemon.trim_end_matches('/'));
    let resp = client
        .delete(&url)
        .send()
        .await
        .with_context(|| format!("DELETE {url} — is kb-code-server running at {daemon}?"))?;
    let status = resp.status();
    if status == reqwest::StatusCode::NO_CONTENT {
        if json {
            println!("{}", serde_json::json!({ "id": id, "deleted": true }));
        } else {
            println!("✓ deleted {id}");
        }
        return Ok(());
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .with_context(|| format!("parse {url} response as JSON"))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    Err(annotation_api_error(
        &format!("delete annotation {id:?}"),
        status,
        &body,
    ))
}

// --- checkout (W4.7) ---------------------------------------------------

/// `kb-code checkout <ref> --repo NAME` — `POST /api/checkout` (W4.7): the
/// daemon's confirmed, only working-tree mutation. A dirty refusal (409)
/// prints every dirty path and exits non-zero — deliberately NOT routed
/// through [`post_json`] (its blanket `error_for_status()` would surface
/// the refusal as an opaque HTTP-error message instead of the structured
/// path list the route actually returns).
async fn checkout_cmd(daemon: &str, repo: &str, target: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let url = format!("{}/api/checkout", daemon.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "repo": repo, "ref": target }))
        .send()
        .await
        .with_context(|| format!("POST {url} — is kb-code-server running at {daemon}?"))?;
    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .with_context(|| format!("parse {url} response as JSON"))?;

    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }

    if status == reqwest::StatusCode::CONFLICT {
        if !json {
            println!("refused: working tree is dirty");
            if let Some(paths) = body["dirty_paths"].as_array() {
                for p in paths {
                    if let Some(s) = p.as_str() {
                        println!("  {s}");
                    }
                }
            }
        }
        anyhow::bail!("checkout refused — working tree is dirty");
    }
    if !status.is_success() {
        let msg = body["error"].as_str().unwrap_or("checkout failed");
        anyhow::bail!("checkout failed: {msg}");
    }
    if !json {
        let ref_ = body["ref"].as_str().unwrap_or(target);
        if body["detached"].as_bool().unwrap_or(false) {
            println!("✓ checked out {ref_} (detached HEAD)");
        } else {
            println!("✓ switched to {ref_}");
        }
    }
    Ok(())
}

// --- agent context verbs: map/pack/defs/refs/similar/impact (W5.1+W5.2) ----

/// `kb-code map [DIR] --repo NAME [--budget N]` — `GET /api/map`.
async fn map_cmd(
    daemon: &str,
    repo: &str,
    dir: &str,
    budget: Option<usize>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut q: Vec<(&str, &str)> = vec![("repo", repo), ("path", dir)];
    let budget_s = budget.map(|b| b.to_string());
    if let Some(b) = &budget_s {
        q.push(("budget", b));
    }
    let body = get_json(&client, daemon, "/api/map", &q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if let Some(outline) = body["outline"].as_str() {
        print!("{outline}");
    }
    println!(
        "--- {} token(s) / {} budget{} ---",
        body["used_tokens"].as_u64().unwrap_or(0),
        body["budget"].as_u64().unwrap_or(0),
        if body["truncated"].as_bool().unwrap_or(false) {
            ", TRUNCATED"
        } else {
            ""
        },
    );
    Ok(())
}

/// `kb-code pack <PATHS...> --repo NAME [--budget N]` — `GET /api/pack`, OR
/// (Phase E3) `kb-code pack --set <NAME-OR-ID> --repo NAME` — resolves
/// `NAME-OR-ID` to an id first ([`resolve_set_id`]), then `GET
/// /api/pack?set=<id>`.
async fn pack_cmd(
    daemon: &str,
    repo: &str,
    paths: &[String],
    set: Option<&str>,
    budget: Option<usize>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let resolved_set: Option<String> = match set {
        Some(s) => Some(resolve_set_id(&client, daemon, repo, s).await?),
        None => None,
    };
    if resolved_set.is_none() && paths.is_empty() {
        anyhow::bail!("kb-code pack: pass one or more PATHS, or --set NAME-OR-ID");
    }
    let joined = paths.join(",");
    let mut q: Vec<(&str, &str)> = vec![("repo", repo)];
    match &resolved_set {
        Some(id) => q.push(("set", id.as_str())),
        None => q.push(("paths", &joined)),
    }
    let budget_s = budget.map(|b| b.to_string());
    if let Some(b) = &budget_s {
        q.push(("budget", b));
    }
    let body = get_json(&client, daemon, "/api/pack", &q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "pack · {} file(s), {} content token(s) used of {} budget \
         (annotations: {}/{} token(s)){}",
        body["files"].as_array().map(|a| a.len()).unwrap_or(0),
        body["content_tokens_used"].as_u64().unwrap_or(0),
        body["budget"].as_u64().unwrap_or(0),
        body["annotations_tokens_used"].as_u64().unwrap_or(0),
        body["annotations_budget"].as_u64().unwrap_or(0),
        if body["truncated"].as_bool().unwrap_or(false) {
            ", TRUNCATED"
        } else {
            ""
        },
    );
    for f in body["files"].as_array().cloned().unwrap_or_default() {
        println!();
        let lines_suffix = match (f["lines"]["start"].as_u64(), f["lines"]["end"].as_u64()) {
            (Some(s), Some(e)) => format!(" :{s}-{e}"),
            _ => String::new(),
        };
        println!(
            "=== {}{lines_suffix} ===",
            f["path"].as_str().unwrap_or("?")
        );
        if let Some(note) = f["span_note"].as_str() {
            println!("  # {note}");
        }
        if let Some(outline) = f["outline"].as_str() {
            if !outline.is_empty() {
                print!("{outline}");
            }
        }
        if let Some(sessions) = f["provenance"].as_array() {
            for s in sessions {
                let ident = s["session_id"]
                    .as_str()
                    .or_else(|| s["display_name"].as_str())
                    .unwrap_or("?");
                println!(
                    "  why: {} ({}, {} line(s))",
                    truncate(ident, 24),
                    s["confidence"].as_str().unwrap_or("?"),
                    s["lines"].as_u64().unwrap_or(0),
                );
            }
        }
        if let Some(anns) = f["annotations"].as_array() {
            for a in anns {
                let first_line = a["first_line"].as_u64().unwrap_or(0);
                let line_count = a["line_count"].as_u64().unwrap_or(1);
                let loc = if line_count > 1 {
                    format!("{first_line}-{}", first_line + line_count - 1)
                } else {
                    first_line.to_string()
                };
                println!(
                    "  annotation @{loc} [{}]{}: {}",
                    a["intent"].as_str().unwrap_or("?"),
                    if a["stale"].as_bool().unwrap_or(false) {
                        " (stale)"
                    } else {
                        ""
                    },
                    truncate(a["body"].as_str().unwrap_or(""), 80),
                );
            }
        }
        if let Some(entries) = f["story"].as_array() {
            for e in entries {
                // CT-E2 — a collapsed attention-gap beat has no identity to
                // print; render it as its own divider line (same copy as
                // `story_gap_line`, compacted to pack's one-line style).
                if e["status"].as_str() == Some("gap") {
                    println!("  story: {}", story_gap_line(e));
                    continue;
                }
                let ident = e["session_id"]
                    .as_str()
                    .or_else(|| e["sha"].as_str())
                    .unwrap_or("?");
                println!(
                    "  story: {} ({}, {})",
                    truncate(ident, 24),
                    e["status"].as_str().unwrap_or("?"),
                    e["confidence"].as_str().unwrap_or("?"),
                );
            }
        }
        let content = &f["content"];
        match content["status"].as_str().unwrap_or("?") {
            "full" => {
                println!("  --- content (full) ---");
                if let Some(t) = content["text"].as_str() {
                    print!("{t}");
                    if !t.ends_with('\n') {
                        println!();
                    }
                }
            }
            "truncated" => {
                println!("  --- content (TRUNCATED by budget) ---");
                if let Some(t) = content["text"].as_str() {
                    print!("{t}");
                    if !t.ends_with('\n') {
                        println!();
                    }
                }
            }
            "binary" => println!("  --- content: binary, skipped ---"),
            "omitted_budget" => println!("  --- content: omitted (budget exhausted) ---"),
            other => println!("  --- content: unknown status {other:?} ---"),
        }
    }
    Ok(())
}

// --- local reviews (V3.R1) -----------------------------------------------

async fn review_start_cmd(
    daemon: &str,
    repo: &str,
    head_ref: &str,
    base: Option<&str>,
    title: Option<&str>,
    session: Option<&str>,
    json: bool,
) -> Result<()> {
    let mut payload = serde_json::json!({ "repo": repo, "head_ref": head_ref });
    if let Some(b) = base {
        payload["base_ref"] = serde_json::json!(b);
    }
    if let Some(t) = title {
        payload["title"] = serde_json::json!(t);
    }
    if let Some(s) = session {
        payload["session_id"] = serde_json::json!(s);
    }
    let client = http_client()?;
    let (status, body) = post_json_raw(&client, daemon, "/api/reviews", &payload).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if !status.is_success() {
        return Err(annotation_api_error("start review", status, &body));
    }
    if !json {
        println!(
            "✓ review {} on {}..{} (ps{})",
            body["id"],
            body["base_ref"].as_str().unwrap_or("?"),
            body["head_ref"].as_str().unwrap_or("?"),
            body["latest_ps"],
        );
    }
    Ok(())
}

async fn review_list_cmd(daemon: &str, repo: &str, state: Option<&str>, json: bool) -> Result<()> {
    let client = http_client()?;
    let mut q = vec![("repo", repo)];
    if let Some(s) = state {
        q.push(("state", s));
    }
    let body = get_json(&client, daemon, "/api/reviews", &q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let reviews = body["reviews"].as_array().cloned().unwrap_or_default();
    if reviews.is_empty() {
        println!("(no reviews)");
        return Ok(());
    }
    for r in &reviews {
        println!(
            "#{:<4} {:6}  {:20}  ps{}  files={} viewed={} open_ann={}  {}..{}",
            r["id"].as_i64().unwrap_or(0),
            r["state"].as_str().unwrap_or("?"),
            r["title"].as_str().unwrap_or("(untitled)"),
            r["latest_ps"].as_i64().unwrap_or(0),
            r["files_count"].as_u64().unwrap_or(0),
            r["viewed_count"].as_u64().unwrap_or(0),
            r["open_annotations"].as_i64().unwrap_or(0),
            r["base_ref"].as_str().unwrap_or("?"),
            r["head_ref"].as_str().unwrap_or("?"),
        );
    }
    Ok(())
}

async fn review_map_cmd(daemon: &str, id: i64, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(&client, daemon, &format!("/api/reviews/{id}/map"), &[]).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "review-map #{}  ps{}  repo={}",
        id,
        body["ps_number"].as_i64().unwrap_or(0),
        body["repo"].as_str().unwrap_or("?")
    );
    if let Some(missing) = body["inputs_missing"].as_array() {
        if !missing.is_empty() {
            let names: Vec<&str> = missing.iter().filter_map(|v| v.as_str()).collect();
            println!("  inputs_missing: {}", names.join(", "));
        }
    }
    println!("\nnodes:");
    println!(
        "  {:<40} {:<10} {:>8} {:>12}",
        "path", "status", "symbols", "agent"
    );
    for n in body["nodes"].as_array().cloned().unwrap_or_default() {
        let path = n["path"].as_str().unwrap_or("?");
        let status = n["status"].as_str().unwrap_or("?");
        let nsym = n["symbols_changed"]
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0);
        let agent = match n["agent_touched"].as_bool() {
            Some(true) => "yes",
            Some(false) => "no",
            None => "?",
        };
        println!(
            "  {:<40} {:<10} {:>8} {:>12}",
            truncate(path, 40),
            status,
            nsym,
            agent
        );
    }
    println!("\nedges:");
    println!(
        "  {:<6} {:<32} → {:<32} {:<10}",
        "kind", "from", "to", "class"
    );
    for e in body["edges"].as_array().cloned().unwrap_or_default() {
        println!(
            "  {:<6} {:<32} → {:<32} {:<10}",
            e["kind"].as_str().unwrap_or("?"),
            truncate(e["from"].as_str().unwrap_or("?"), 32),
            truncate(e["to"].as_str().unwrap_or("?"), 32),
            e["class"].as_str().unwrap_or("?"),
        );
    }
    Ok(())
}

async fn review_order_cmd(daemon: &str, id: i64, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(
        &client,
        daemon,
        &format!("/api/reviews/{id}/reading-order"),
        &[],
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "review-order #{}  ps{}  repo={}",
        id,
        body["ps_number"].as_i64().unwrap_or(0),
        body["repo"].as_str().unwrap_or("?")
    );
    if let Some(missing) = body["inputs_missing"].as_array() {
        if !missing.is_empty() {
            let names: Vec<&str> = missing.iter().filter_map(|v| v.as_str()).collect();
            println!("  inputs_missing: {}", names.join(", "));
        }
    }
    println!("\n  {:>3}  {:<40} {:<28} cycle", "#", "path", "reason");
    for (i, s) in body["stops"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let cycle = if s["cycle"].as_bool().unwrap_or(false) {
            "yes"
        } else {
            ""
        };
        println!(
            "  {:>3}  {:<40} {:<28} {}",
            i + 1,
            truncate(s["path"].as_str().unwrap_or("?"), 40),
            truncate(s["reason"].as_str().unwrap_or("?"), 28),
            cycle
        );
    }
    Ok(())
}

async fn review_risk_cmd(daemon: &str, id: i64, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(&client, daemon, &format!("/api/reviews/{id}/risk"), &[]).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "review-risk #{}  ps{}  (attention only — terms are the substance)",
        id,
        body["ps_number"].as_i64().unwrap_or(0)
    );
    if let Some(note) = body["note"].as_str() {
        println!("  note: {note}");
    }
    println!(
        "{:<40} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "path", "churn", "minor", "hotspot", "agent1st", "pain", "score"
    );
    let files = body["files"].as_array().cloned().unwrap_or_default();
    for f in &files {
        let path = f["path"].as_str().unwrap_or("?");
        let risk = &f["risk"];
        if risk.is_null() {
            println!(
                "{:<40} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
                truncate(path, 40),
                "—",
                "—",
                "—",
                "—",
                "—",
                "null"
            );
            continue;
        }
        let terms = &risk["terms"];
        let fmt = |v: &serde_json::Value| -> String {
            if v.is_null() {
                "—".into()
            } else if let Some(b) = v.as_bool() {
                if b {
                    "yes".into()
                } else {
                    "no".into()
                }
            } else if let Some(n) = v.as_f64() {
                format!("{n:.3}")
            } else {
                "—".into()
            }
        };
        println!(
            "{:<40} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
            truncate(path, 40),
            fmt(&terms["relative_churn"]),
            fmt(&terms["ownership_minor"]),
            fmt(&terms["hotspot_rank"]),
            fmt(&terms["agent_first_touch"]),
            fmt(&terms["session_pain"]),
            risk["score"]
                .as_f64()
                .map(|s| format!("{s:.3}"))
                .unwrap_or_else(|| "—".into()),
        );
    }
    Ok(())
}

async fn review_show_cmd(daemon: &str, id: i64, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(&client, daemon, &format!("/api/reviews/{id}"), &[]).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "review #{}  {}  {}..{}",
        body["id"],
        body["state"].as_str().unwrap_or("?"),
        body["base_ref"].as_str().unwrap_or("?"),
        body["head_ref"].as_str().unwrap_or("?"),
    );
    if let Some(t) = body["title"].as_str() {
        println!("  title: {t}");
    }
    let pss = body["patchsets"].as_array().cloned().unwrap_or_default();
    for ps in &pss {
        println!(
            "  ps{:<3}  {}  commits={}  captured={}",
            ps["ps_number"],
            ps["tip_sha"].as_str().unwrap_or("?"),
            ps["commit_count"],
            ps["captured_at"],
        );
    }
    Ok(())
}

async fn review_snapshot_cmd(daemon: &str, id: i64, json: bool) -> Result<()> {
    let client = http_client()?;
    let (status, body) = post_json_raw(
        &client,
        daemon,
        &format!("/api/reviews/{id}/snapshot"),
        &serde_json::json!({}),
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if !status.is_success() {
        return Err(annotation_api_error("snapshot review", status, &body));
    }
    if !json {
        println!(
            "✓ captured ps{} tip={}",
            body["ps_number"],
            body["tip_sha"]
                .as_str()
                .unwrap_or("?")
                .chars()
                .take(12)
                .collect::<String>(),
        );
    }
    Ok(())
}

async fn review_files_cmd(daemon: &str, id: i64, ps: Option<i64>, json: bool) -> Result<()> {
    let client = http_client()?;
    let path = format!("/api/reviews/{id}/files");
    let body = match ps {
        Some(n) => {
            let s = n.to_string();
            get_json(&client, daemon, &path, &[("ps", s.as_str())]).await?
        }
        None => get_json(&client, daemon, &path, &[]).await?,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "review #{}  ps{}  {}..{}",
        id,
        body["ps_number"],
        body["base_sha"]
            .as_str()
            .unwrap_or("?")
            .chars()
            .take(12)
            .collect::<String>(),
        body["tip_sha"]
            .as_str()
            .unwrap_or("?")
            .chars()
            .take(12)
            .collect::<String>(),
    );
    let files = body["files"].as_array().cloned().unwrap_or_default();
    for f in &files {
        let mark = match (f["viewed"].as_bool(), f["viewed_stale"].as_bool()) {
            (Some(true), Some(true)) => "S", // stale
            (Some(true), _) => "V",
            _ => " ",
        };
        println!(
            "  [{mark}] {}{}  +{} -{}  ann={}",
            f["status"].as_str().unwrap_or("?"),
            f["path"].as_str().unwrap_or("?"),
            f["additions"],
            f["deletions"],
            f["open_annotations"],
        );
    }
    Ok(())
}

async fn review_interdiff_cmd(daemon: &str, id: i64, from: i64, to: i64, json: bool) -> Result<()> {
    let client = http_client()?;
    let from_s = from.to_string();
    let to_s = to.to_string();
    let body = get_json(
        &client,
        daemon,
        &format!("/api/reviews/{id}/interdiff"),
        &[("from", from_s.as_str()), ("to", to_s.as_str())],
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!("interdiff review #{id}  ps{from}..ps{to}");
    println!("files:");
    for f in body["files"].as_array().cloned().unwrap_or_default() {
        println!(
            "  {} {}  +{} -{}",
            f["status"].as_str().unwrap_or("?"),
            f["path"].as_str().unwrap_or("?"),
            f["additions"],
            f["deletions"],
        );
    }
    println!("range-diff:");
    for p in body["range_diff"]["pairs"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        println!(
            "  {:8}  {} → {}  {}",
            p["disposition"].as_str().unwrap_or("?"),
            p["old_sha"].as_str().unwrap_or("-"),
            p["new_sha"].as_str().unwrap_or("-"),
            p["old_subject"]
                .as_str()
                .or_else(|| p["new_subject"].as_str())
                .unwrap_or(""),
        );
    }
    Ok(())
}

async fn review_viewed_cmd(
    daemon: &str,
    id: i64,
    path: &str,
    unset: bool,
    blob_sha: Option<&str>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    if unset {
        let enc = urlencoding_path(path);
        let url = format!(
            "{}/api/reviews/{id}/viewed/{enc}",
            daemon.trim_end_matches('/')
        );
        let resp = client
            .delete(&url)
            .send()
            .await
            .with_context(|| format!("DELETE {url}"))?;
        let status = resp.status();
        if status == reqwest::StatusCode::NO_CONTENT {
            if json {
                println!("{}", serde_json::json!({ "path": path, "unset": true }));
            } else {
                println!("✓ unset viewed {path}");
            }
            return Ok(());
        }
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        return Err(annotation_api_error("unset viewed", status, &body));
    }
    let blob = if let Some(b) = blob_sha {
        b.to_string()
    } else {
        // Look up the current blob from the files list.
        let files = get_json(&client, daemon, &format!("/api/reviews/{id}/files"), &[]).await?;
        files["files"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|f| f["path"].as_str() == Some(path))
            .and_then(|f| f["blob_sha"].as_str().map(|s| s.to_string()))
            .ok_or_else(|| anyhow::anyhow!("path {path:?} not in latest patchset change set"))?
    };
    let payload = serde_json::json!({ "path": path, "blob_sha": blob });
    let url = format!("{}/api/reviews/{id}/viewed", daemon.trim_end_matches('/'));
    let resp = client
        .put(&url)
        .json(&payload)
        .send()
        .await
        .with_context(|| format!("PUT {url}"))?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if !status.is_success() {
        return Err(annotation_api_error("mark viewed", status, &body));
    }
    if !json {
        println!("✓ viewed {path}");
    }
    Ok(())
}

/// Percent-encode a path for a single URL path segment (slashes → %2F).
fn urlencoding_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len() * 3);
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

async fn review_close_cmd(daemon: &str, id: i64, json: bool) -> Result<()> {
    let client = http_client()?;
    let (status, body) = patch_json_path(
        &client,
        daemon,
        &format!("/api/reviews/{id}"),
        &serde_json::json!({ "state": "closed" }),
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if !status.is_success() {
        return Err(annotation_api_error("close review", status, &body));
    }
    if !json {
        println!("✓ closed review #{id}");
    }
    Ok(())
}

async fn review_gc_cmd(daemon: &str, review: Option<i64>, json: bool) -> Result<()> {
    let client = http_client()?;
    let mut payload = serde_json::json!({});
    if let Some(id) = review {
        payload["review_id"] = serde_json::json!(id);
    }
    let (status, body) = post_json_raw(&client, daemon, "/api/reviews/gc", &payload).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if !status.is_success() {
        return Err(annotation_api_error("review gc", status, &body));
    }
    if !json {
        println!(
            "✓ gc deleted {} patchset(s) (max={})",
            body["deleted"], body["max_patchsets"]
        );
    }
    Ok(())
}

// --- V4.P1 branch / compare / merge-check / repo-state / comments / -----
//     verdict / suggest / batch ------------------------------------------

async fn branches_cmd(daemon: &str, repo: &str, sort: &str, json: bool) -> Result<()> {
    if sort != "name" && sort != "suggested" {
        anyhow::bail!("--sort must be name or suggested, got {sort:?}");
    }
    let client = http_client()?;
    let body = get_json(
        &client,
        daemon,
        "/api/branches",
        &[("repo", repo), ("sort", sort)],
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    print_branches_human(&body, sort == "suggested");
    Ok(())
}

fn print_branches_human(body: &serde_json::Value, show_terms: bool) {
    let default = body["default"].as_str();
    let branches = body["branches"].as_array().cloned().unwrap_or_default();
    if branches.is_empty() {
        println!("(no branches)");
        if body["truncated"].as_bool().unwrap_or(false) {
            println!("… truncated");
        }
        return;
    }
    for b in &branches {
        let name = b["name"].as_str().unwrap_or("?");
        let is_default = default == Some(name);
        let head = if b["is_head"].as_bool().unwrap_or(false) {
            "*"
        } else {
            " "
        };
        let mut chips: Vec<String> = Vec::new();
        if is_default {
            chips.push("(default)".into());
        }
        if let Some(remote) = b["remote"].as_str() {
            chips.push(remote.to_string());
        }
        if b["has_open_review"].as_bool().unwrap_or(false) {
            chips.push("[review]".into());
        }
        let ahead = b["ahead"].as_u64().unwrap_or(0);
        let behind = b["behind"].as_u64().unwrap_or(0);
        if !is_default || ahead > 0 || behind > 0 {
            chips.push(format!("+{ahead} -{behind}"));
        }
        if show_terms {
            if let Some(terms) = format_suggest_terms(b) {
                chips.push(terms);
            }
        }
        let extra = if chips.is_empty() {
            String::new()
        } else {
            format!("  {}", chips.join("  "))
        };
        println!("{head} {name}{extra}");
    }
    if body["truncated"].as_bool().unwrap_or(false) {
        println!("… truncated (more branches exist)");
    }
}

/// Compact `recency 0.82 · review · ahead 3` line from `suggest.terms`
/// plus the branch's own ahead/behind counts (the example in the brief
/// prints the integer, not the score weight).
fn format_suggest_terms(branch: &serde_json::Value) -> Option<String> {
    let terms = branch.get("suggest")?.get("terms")?.as_object()?;
    let mut parts: Vec<String> = Vec::new();
    if let Some(r) = terms.get("recency").and_then(|v| v.as_f64()) {
        parts.push(format!("recency {r:.2}"));
    }
    if terms.contains_key("has_open_review") {
        parts.push("review".into());
    }
    if let Some(a) = terms.get("attribution").and_then(|v| v.as_f64()) {
        parts.push(format!("attribution {a:.2}"));
    }
    if terms.contains_key("ahead") {
        parts.push(format!("ahead {}", branch["ahead"].as_u64().unwrap_or(0)));
    }
    if terms.contains_key("behind") {
        parts.push(format!("behind {}", branch["behind"].as_u64().unwrap_or(0)));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" · "))
    }
}

async fn compare_cmd(
    daemon: &str,
    repo: &str,
    from: &str,
    to: &str,
    three_dot: bool,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let three = if three_dot { "true" } else { "false" };
    let body = get_json(
        &client,
        daemon,
        "/api/compare",
        &[
            ("repo", repo),
            ("from", from),
            ("to", to),
            ("three_dot", three),
        ],
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    print_compare_human(&body);
    Ok(())
}

fn print_compare_human(body: &serde_json::Value) {
    let from = body["from"].as_str().unwrap_or("?");
    let to = body["to"].as_str().unwrap_or("?");
    let dots = if body["three_dot"].as_bool().unwrap_or(false) {
        "..."
    } else {
        ".."
    };
    let totals = &body["totals"];
    println!(
        "compare {from}{dots}{to}  {} commit(s), {} file(s)  +{} -{}",
        body["commits"].as_array().map(|a| a.len()).unwrap_or(0),
        totals["files"].as_u64().unwrap_or(0),
        totals["insertions"].as_u64().unwrap_or(0),
        totals["deletions"].as_u64().unwrap_or(0),
    );
    if body["commits_truncated"].as_bool().unwrap_or(false) {
        println!("  (commit list truncated)");
    }
    for c in body["commits"].as_array().cloned().unwrap_or_default() {
        let summary = if c.get("summary").is_some() {
            &c["summary"]
        } else {
            &c
        };
        let sha = summary["sha"].as_str().unwrap_or("");
        let short: String = sha.chars().take(8).collect();
        println!("  {}  {}", short, summary["subject"].as_str().unwrap_or(""));
    }
    println!("files:");
    for f in body["files"].as_array().cloned().unwrap_or_default() {
        let status = f["status"].as_str().unwrap_or("?");
        let path = f["path"].as_str().unwrap_or("?");
        let bin = if f["binary"].as_bool().unwrap_or(false) {
            "  (binary)"
        } else {
            ""
        };
        println!(
            "  {status}  {path}  +{} -{}{bin}",
            f["insertions"].as_u64().unwrap_or(0),
            f["deletions"].as_u64().unwrap_or(0),
        );
    }
}

async fn merge_check_cmd(
    daemon: &str,
    repo: &str,
    from: Option<&str>,
    to: &str,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let from_owned;
    let from = match from {
        Some(f) => f,
        None => {
            let branches = get_json(&client, daemon, "/api/branches", &[("repo", repo)]).await?;
            from_owned = branches["default"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("no default branch for repo {repo}"))?
                .to_string();
            from_owned.as_str()
        }
    };
    let body = get_json(
        &client,
        daemon,
        "/api/merge-check",
        &[("repo", repo), ("from", from), ("to", to)],
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if body["clean"].as_bool().unwrap_or(false) {
        println!(
            "clean  {from}..{to}  ahead {}  behind {}",
            body["ahead"].as_u64().unwrap_or(0),
            body["behind"].as_u64().unwrap_or(0),
        );
    } else {
        println!(
            "conflicts  {from}..{to}  ahead {}  behind {}",
            body["ahead"].as_u64().unwrap_or(0),
            body["behind"].as_u64().unwrap_or(0),
        );
        for c in body["conflicts"].as_array().cloned().unwrap_or_default() {
            let path = c.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            println!("  {path}");
        }
    }
    Ok(())
}

async fn repo_state_cmd(daemon: &str, repo: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(&client, daemon, "/api/repo-state", &[("repo", repo)]).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let op = body["op"].as_str().unwrap_or("none");
    let detail = &body["detail"];
    let op_extra = match (
        detail.get("step").and_then(|v| v.as_u64()),
        detail.get("total").and_then(|v| v.as_u64()),
    ) {
        (Some(s), Some(t)) => format!(" {s}/{t}"),
        _ => String::new(),
    };
    let dirty = if body["dirty"].as_bool().unwrap_or(false) {
        "dirty"
    } else {
        "clean"
    };
    println!("op: {op}{op_extra}  {dirty}");
    let conflicts = body["conflicted"].as_array().cloned().unwrap_or_default();
    if conflicts.is_empty() {
        println!("conflicts: (none)");
    } else {
        println!("conflicts:");
        for c in conflicts {
            if let Some(p) = c.as_str() {
                println!("  {p}");
            }
        }
    }
    Ok(())
}

async fn review_comments_cmd(
    daemon: &str,
    id: i64,
    ps: Option<&str>,
    all: bool,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut query: Vec<(&str, &str)> = Vec::new();
    if let Some(p) = ps {
        query.push(("ps", p));
    }
    if all {
        query.push(("all", "true"));
    }
    let body = get_json(
        &client,
        daemon,
        &format!("/api/reviews/{id}/comments"),
        &query,
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    print_review_comments_human(&body);
    Ok(())
}

fn print_review_comments_human(body: &serde_json::Value) {
    let groups = body["groups"].as_array().cloned().unwrap_or_default();
    if groups.is_empty() {
        println!("(no comments)");
        return;
    }
    for g in &groups {
        println!("{}", g["path"].as_str().unwrap_or("?"));
        for c in g["comments"].as_array().cloned().unwrap_or_default() {
            println!("  {}", format_review_comment_line(&c));
        }
    }
}

fn format_review_comment_line(c: &serde_json::Value) -> String {
    let intent = c["intent"].as_str().unwrap_or("note");
    let author = c["author"].as_str().unwrap_or("?");
    let resolved = c["resolved"].as_bool().unwrap_or(false);
    let resolution = &c["resolution"];
    let orphaned = resolution["orphaned"].as_bool().unwrap_or(false);
    let loc = if orphaned {
        let orig = &resolution["original"];
        format!(
            "⚠ orphaned (was ps{}:L{})",
            orig["ps"].as_i64().unwrap_or(0),
            orig["line"].as_u64().unwrap_or(0)
        )
    } else {
        match resolution["line"].as_u64() {
            Some(n) => format!("L{n}"),
            None => "L?".into(),
        }
    };
    let body_head = c["body"]
        .as_str()
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("")
        .trim();
    let replies = c["replies"].as_array().map(|a| a.len()).unwrap_or(0);
    let reply_bit = match replies {
        0 => String::new(),
        1 => "  (1 reply)".into(),
        n => format!("  ({n} replies)"),
    };
    let sug = match c.get("suggestion") {
        Some(s) if !s.is_null() => {
            if s["applied"].as_bool().unwrap_or(false) {
                "  suggestion: applied"
            } else {
                "  suggestion pending"
            }
        }
        _ => "",
    };
    let resolved_bit = if resolved { "  resolved" } else { "" };
    format!("{intent}  {author}  {loc}{resolved_bit}  {body_head}{reply_bit}{sug}")
}

const VERDICT_STATES: &[&str] = &["approve", "request-changes", "comment"];

async fn review_verdict_cmd(
    daemon: &str,
    id: i64,
    state: Option<&str>,
    note: Option<&str>,
    clear: bool,
    json: bool,
) -> Result<()> {
    if clear && state.is_some() {
        anyhow::bail!("review verdict: pass a state or --clear, not both");
    }
    if !clear && state.is_none() {
        anyhow::bail!("review verdict: pass approve|request-changes|comment, or --clear");
    }
    let client = http_client()?;
    let path = format!("/api/reviews/{id}/verdict");
    let (status, body) = if clear {
        delete_json_raw(&client, daemon, &path, &[]).await?
    } else {
        let state = state.expect("checked above");
        if !VERDICT_STATES.contains(&state) {
            anyhow::bail!("verdict state must be approve|request-changes|comment, got {state:?}");
        }
        let mut payload = serde_json::json!({ "state": state });
        if let Some(n) = note {
            payload["note"] = serde_json::json!(n);
        }
        put_json_raw(&client, daemon, &path, &payload).await?
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if !status.is_success() {
        return Err(loopback_or_api_error(
            "review verdict",
            daemon,
            status,
            &body,
        ));
    }
    if !json {
        let changed = body["changed"].as_bool().unwrap_or(false);
        if clear {
            println!("verdict cleared  changed={changed}");
        } else {
            println!("verdict {}  changed={changed}", state.unwrap_or("?"));
        }
    }
    Ok(())
}

/// `kb-code review distill <ID> [--json]` — CT-E7: `GET
/// /api/reviews/{id}/distill`. `--json` prints the document verbatim
/// (the shape an agent pipes into `kb notes new`/`kb remember`); human
/// mode prints a compact summary — meta, verdict, thread/suggestion
/// counts — never the full thread/suggestion bodies (`review comments`
/// already owns that view).
async fn review_distill_cmd(daemon: &str, id: i64, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(&client, daemon, &format!("/api/reviews/{id}/distill"), &[]).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let review = &body["review"];
    println!(
        "review #{}  {}  {}..{}",
        review["id"],
        review["state"].as_str().unwrap_or("?"),
        review["base_ref"].as_str().unwrap_or("?"),
        review["head_ref"].as_str().unwrap_or("?"),
    );
    if let Some(t) = review["title"].as_str() {
        println!("  title: {t}");
    }
    println!(
        "  latest ps{}  patchsets={}  files={}",
        body["latest_ps"],
        body["patchsets"].as_array().map(|a| a.len()).unwrap_or(0),
        body["files"].as_array().map(|a| a.len()).unwrap_or(0),
    );
    if body["verdict"].is_null() {
        println!("  verdict: (none)");
    } else {
        let stale = if body["verdict_stale"].as_bool().unwrap_or(false) {
            "  (stale)"
        } else {
            ""
        };
        println!(
            "  verdict: {} (ps{}){stale}",
            body["verdict"]["state"].as_str().unwrap_or("?"),
            body["verdict"]["ps"].as_i64().unwrap_or(0),
        );
    }
    println!(
        "  {} threads ({} unresolved), {} suggestions ({} applied)",
        body["thread_count"],
        body["unresolved_count"],
        body["suggestions"].as_array().map(|a| a.len()).unwrap_or(0),
        body["suggestions_applied_count"],
    );
    Ok(())
}

// --- PRR-R2: review start-pr / report / artifact, pr {list,show,checks,
// comments,fetch} -----------------------------------------------------------

/// `kb-code review start-pr --repo R --pr N [--base][--title][--session]
/// [--json]` — `POST /api/reviews/pr` (design doc §2 row 1). LOOPBACK-ONLY.
#[allow(clippy::too_many_arguments)]
async fn review_start_pr_cmd(
    daemon: &str,
    repo: &str,
    pr_number: u32,
    base: Option<&str>,
    title: Option<&str>,
    session: Option<&str>,
    json: bool,
) -> Result<()> {
    let mut payload = serde_json::json!({ "repo": repo, "pr_number": pr_number });
    if let Some(b) = base {
        payload["base_ref"] = serde_json::json!(b);
    }
    if let Some(t) = title {
        payload["title"] = serde_json::json!(t);
    }
    if let Some(s) = session {
        payload["session_id"] = serde_json::json!(s);
    }
    let client = http_client()?;
    let (status, body) = post_json_raw(&client, daemon, "/api/reviews/pr", &payload).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if !status.is_success() {
        if status == reqwest::StatusCode::CONFLICT {
            return Err(anyhow::anyhow!(
                "review start-pr failed (409): {} — existing review id {}",
                body["error"].as_str().unwrap_or("already bound"),
                body["existing_review_id"]
            ));
        }
        return Err(loopback_or_api_error(
            "review start-pr",
            daemon,
            status,
            &body,
        ));
    }
    if !json {
        let meta_note = if body["pr_meta"].is_null() {
            format!(
                " (metadata unavailable: {})",
                body["pr_meta_unavailable_reason"].as_str().unwrap_or("?")
            )
        } else {
            String::new()
        };
        println!(
            "✓ review {} bound to {}#{} (ps{}){meta_note}",
            body["id"], body["pr_repo_slug"], body["pr_number"], body["latest_ps"],
        );
    }
    Ok(())
}

/// `kb-code review report ID [--json]` — `GET /api/reviews/{id}/report`
/// (design doc §2 row 4). `kb-code review report ID --set --from-file FILE
/// [--json]` — `PUT /api/reviews/{id}/report` (design doc §2 row 5),
/// LOOPBACK-ONLY.
async fn review_report_cmd(
    daemon: &str,
    id: i64,
    set: bool,
    from_file: Option<&Path>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let path = format!("/api/reviews/{id}/report");
    if set {
        let Some(p) = from_file else {
            anyhow::bail!("review report --set: pass --from-file FILE");
        };
        let text = std::fs::read_to_string(p)
            .with_context(|| format!("read --from-file {}", p.display()))?;
        let payload: serde_json::Value = serde_json::from_str(&text)
            .with_context(|| format!("parse {} as JSON", p.display()))?;
        let (status, body) = put_json_raw(&client, daemon, &path, &payload).await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        if !status.is_success() {
            return Err(loopback_or_api_error(
                "review report --set",
                daemon,
                status,
                &body,
            ));
        }
        if !json {
            println!(
                "✓ report set on review {id}  generated_at={}",
                body["generated_at"]
            );
        }
        return Ok(());
    }
    let body = get_json(&client, daemon, &path, &[]).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if body.get("report").map(|v| v.is_null()).unwrap_or(false)
        && body.as_object().map(|o| o.len()) == Some(1)
    {
        println!("review {id}: no report authored yet");
        return Ok(());
    }
    println!(
        "review {id} report  risk={}  ps{}",
        body["risk_score"], body["ps_number"],
    );
    if let Some(s) = body["summary"].as_str() {
        println!("  summary: {s}");
    }
    Ok(())
}

/// `kb-code review artifact ID [--json]` — `GET /api/reviews/{id}/artifact`
/// (design doc §2 row 7). Live, unpersisted.
async fn review_artifact_cmd(daemon: &str, id: i64, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(&client, daemon, &format!("/api/reviews/{id}/artifact"), &[]).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if body["hint"].is_null() {
        println!("review {id}: no artifact hint set");
        return Ok(());
    }
    let verified = body["verified"].as_bool().unwrap_or(false);
    println!(
        "review {id} artifact hint: {}/{}  verified={verified}",
        body["hint"]["kb"].as_str().unwrap_or("?"),
        body["hint"]["id"].as_str().unwrap_or("?"),
    );
    if let Some(r) = body["unavailable_reason"].as_str() {
        println!("  unavailable: {r}");
    }
    if verified {
        println!("  title: {}", body["doc"]["title"].as_str().unwrap_or("?"));
    }
    Ok(())
}

/// `kb-code review set-artifact ID KB DOC_ID [--json]` — V70-A3X: `PATCH
/// /api/reviews/{id}` with `{artifact_hint_kb, artifact_hint_id}`
/// (`reviews::PatchReviewBody`'s doc — the two fields are written together
/// as a pair, never independently). `review artifact ID` is the follow-up
/// verification step.
async fn review_set_artifact_cmd(
    daemon: &str,
    id: i64,
    kb: &str,
    doc_id: &str,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let payload = serde_json::json!({
        "artifact_hint_kb": kb,
        "artifact_hint_id": doc_id,
    });
    let (status, body) =
        patch_json_path(&client, daemon, &format!("/api/reviews/{id}"), &payload).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if !status.is_success() {
        return Err(loopback_or_api_error(
            "review set-artifact",
            daemon,
            status,
            &body,
        ));
    }
    if !json {
        println!("✓ review {id} artifact hint set to {kb}/{doc_id}");
    }
    Ok(())
}

// --- PRR-R3: review findings import/list/add, disposition ------------------

/// `kb-code review findings import ID {--from-file FILE|--stdin}
/// [--mode full|additive] [--json]` — `POST
/// /api/reviews/{id}/findings/import` (design doc §2 row 8,
/// `kbc-findings/1`). LOOPBACK-ONLY. `--mode` (when given) is spliced into
/// the payload's top-level `mode` field — a bare pass-through, validated
/// client-side against the SAME `full|additive` vocab the daemon enforces
/// so a typo is caught before the round trip.
async fn review_findings_import_cmd(
    daemon: &str,
    id: i64,
    from_file: Option<&Path>,
    stdin: bool,
    mode: Option<&str>,
    json: bool,
) -> Result<()> {
    if from_file.is_some() == stdin {
        anyhow::bail!("review findings import: pass exactly one of --from-file FILE or --stdin");
    }
    let text = if stdin {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
            .context("read findings payload from stdin")?;
        buf
    } else {
        let p = from_file.expect("checked above");
        std::fs::read_to_string(p).with_context(|| format!("read --from-file {}", p.display()))?
    };
    let mut payload: serde_json::Value =
        serde_json::from_str(&text).context("parse findings payload as JSON")?;
    if let Some(m) = mode {
        if m != "full" && m != "additive" {
            anyhow::bail!("review findings import: --mode must be full|additive, got {m:?}");
        }
        payload["mode"] = serde_json::json!(m);
    }
    let client = http_client()?;
    let path = format!("/api/reviews/{id}/findings/import");
    let (status, body) = post_json_raw(&client, daemon, &path, &payload).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if !status.is_success() {
        return Err(loopback_or_api_error(
            "review findings import",
            daemon,
            status,
            &body,
        ));
    }
    if !json {
        println!(
            "✓ findings import batch {}  created={} updated={} superseded={} unchanged={}",
            body["import_batch_id"].as_str().unwrap_or("?"),
            body["created"].as_array().map(|a| a.len()).unwrap_or(0),
            body["updated"].as_array().map(|a| a.len()).unwrap_or(0),
            body["superseded"].as_array().map(|a| a.len()).unwrap_or(0),
            body["unchanged"].as_array().map(|a| a.len()).unwrap_or(0),
        );
    }
    Ok(())
}

/// `kb-code review compose ID {--from-file FILE|--stdin} [--json]` —
/// V70-R: `POST /api/reviews/{id}/compose` (design doc D9 scoped to v0).
/// The payload is a `kbc-compose/1` object — `summary` (required) plus the
/// SAME `kbc-findings/1` shape `review findings import` accepts, nested
/// under `findings` — the "today's findings JSON + summary, one
/// transaction" v0 the milestone plan names.
async fn review_compose_cmd(
    daemon: &str,
    id: i64,
    from_file: Option<&Path>,
    stdin: bool,
    json: bool,
) -> Result<()> {
    if from_file.is_some() == stdin {
        anyhow::bail!("review compose: pass exactly one of --from-file FILE or --stdin");
    }
    let text = if stdin {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
            .context("read compose payload from stdin")?;
        buf
    } else {
        let p = from_file.expect("checked above");
        std::fs::read_to_string(p).with_context(|| format!("read --from-file {}", p.display()))?
    };
    let payload: serde_json::Value =
        serde_json::from_str(&text).context("parse compose payload as JSON")?;
    let client = http_client()?;
    let path = format!("/api/reviews/{id}/compose");
    let (status, body) = post_json_raw(&client, daemon, &path, &payload).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if !status.is_success() {
        return Err(loopback_or_api_error(
            "review compose",
            daemon,
            status,
            &body,
        ));
    }
    if !json {
        let f = &body["findings"];
        println!(
            "✓ composed review {}  findings: created={} updated={} superseded={} unchanged={}  \
             report_set={} verdict_changed={}",
            body["review_id"].as_i64().unwrap_or(id),
            f["created"].as_array().map(|a| a.len()).unwrap_or(0),
            f["updated"].as_array().map(|a| a.len()).unwrap_or(0),
            f["superseded"].as_array().map(|a| a.len()).unwrap_or(0),
            f["unchanged"].as_array().map(|a| a.len()).unwrap_or(0),
            body["report_set"].as_bool().unwrap_or(false),
            body["verdict_changed"].as_bool().unwrap_or(false),
        );
    }
    Ok(())
}

/// `kb-code review findings list ID [--ps][--disposition][--all] [--json]`
/// — `GET /api/reviews/{id}/findings` (design doc §2 row 9). `--all`
/// includes superseded (tombstoned) findings.
async fn review_findings_list_cmd(
    daemon: &str,
    id: i64,
    ps: Option<&str>,
    disposition: Option<&str>,
    all: bool,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut query: Vec<(&str, &str)> = Vec::new();
    if let Some(p) = ps {
        query.push(("ps", p));
    }
    if let Some(d) = disposition {
        query.push(("disposition", d));
    }
    if all {
        query.push(("include_superseded", "true"));
    }
    let body = get_json(
        &client,
        daemon,
        &format!("/api/reviews/{id}/findings"),
        &query,
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let findings = body["findings"].as_array().cloned().unwrap_or_default();
    if findings.is_empty() {
        println!("(no findings)");
        return Ok(());
    }
    for f in &findings {
        println!("{}", format_finding_line(f));
    }
    Ok(())
}

fn format_finding_line(f: &serde_json::Value) -> String {
    let slug = f["slug"].as_str().unwrap_or("?");
    let severity = f["severity"].as_str().unwrap_or("?");
    let category = f["category"].as_str().unwrap_or("?");
    let path = f["location"]["path"].as_str().unwrap_or("?");
    let resolution = &f["resolution"];
    let loc = if resolution["orphaned"].as_bool().unwrap_or(false) {
        "⚠ orphaned".to_string()
    } else {
        match resolution["line"].as_u64() {
            Some(n) => format!("L{n}"),
            None => path.to_string(),
        }
    };
    let disp = match f["disposition"].as_object() {
        Some(d) => d["state"].as_str().unwrap_or("?").to_string(),
        None => "undecided".to_string(),
    };
    let origin = f["origin"].as_str().unwrap_or("?");
    let superseded = if f["superseded"].as_bool().unwrap_or(false) {
        "  [superseded]"
    } else {
        ""
    };
    let title = f["title"].as_str().unwrap_or("");
    format!(
        "{slug}  [{severity}]  {category}  {path}:{loc}  disposition={disp}  \
         origin={origin}{superseded}  {title}"
    )
}

/// `kb-code review findings add ID --severity S --category C --path P
/// {--line N|--lines A-B|--whole-file} -m TITLE --rationale R
/// [--recommendation ...] [--slug ...] [--evidence FILE
/// [--evidence-lang LANG]] [--json]` — `POST /api/reviews/{id}/findings`
/// (addendum §E — a single human-authored finding). LOOPBACK-ONLY.
/// `rationale` is REQUIRED here (not bracketed optional, despite the
/// milestone plan's own CLI sketch) because addendum §E's wire body lists
/// `rationale` with no `?` — the same field `recommendation` DOES carry
/// one; this CLI verb follows the stricter, authoritative route contract
/// rather than the plan's shorthand. `--evidence` (V70-A3X) reads FILE as
/// UTF-8 text and sends it as `evidence.source` — the SAME
/// `FindingEvidenceBody{lang, source}` shape `findings import` already
/// wires per-finding, now reachable for a manual `add` too.
#[allow(clippy::too_many_arguments)]
async fn review_findings_add_cmd(
    daemon: &str,
    id: i64,
    severity: &str,
    category: &str,
    path: &str,
    line: Option<i64>,
    lines: Option<&str>,
    whole_file: bool,
    removed: bool,
    title: &str,
    rationale: &str,
    recommendation: Option<&str>,
    slug: Option<&str>,
    evidence: Option<&Path>,
    evidence_lang: Option<&str>,
    json: bool,
) -> Result<()> {
    let picked = [line.is_some(), lines.is_some(), whole_file]
        .iter()
        .filter(|b| **b)
        .count();
    if picked != 1 {
        anyhow::bail!(
            "review findings add: pass exactly one of --line N, --lines A-B, or --whole-file"
        );
    }
    let (kind, lines_json): (&str, Option<Vec<i64>>) = if whole_file {
        ("whole_file", None)
    } else if let Some(n) = line {
        ("single", Some(vec![n]))
    } else {
        let spec = lines.expect("checked above (exactly one of the three is set)");
        let (a, b) = spec.split_once('-').ok_or_else(|| {
            anyhow::anyhow!("review findings add: --lines must look like A-B, got {spec:?}")
        })?;
        let a: i64 = a
            .trim()
            .parse()
            .with_context(|| format!("--lines start {a:?} is not a number"))?;
        let b: i64 = b
            .trim()
            .parse()
            .with_context(|| format!("--lines end {b:?} is not a number"))?;
        ("range", Some(vec![a, b]))
    };

    let mut payload = serde_json::json!({
        "severity": severity,
        "category": category,
        "location": {
            "path": path,
            "kind": kind,
            "lines": lines_json,
            "removed": removed,
        },
        "title": title,
        "rationale": rationale,
    });
    if let Some(r) = recommendation {
        payload["recommendation"] = serde_json::json!(r);
    }
    if let Some(s) = slug {
        payload["slug"] = serde_json::json!(s);
    }
    if let Some(evidence_path) = evidence {
        let source = std::fs::read_to_string(evidence_path).with_context(|| {
            format!("review findings add: failed to read --evidence file {evidence_path:?}")
        })?;
        let mut ev = serde_json::json!({ "source": source });
        if let Some(lang) = evidence_lang {
            ev["lang"] = serde_json::json!(lang);
        }
        payload["evidence"] = ev;
    }

    let client = http_client()?;
    let route_path = format!("/api/reviews/{id}/findings");
    let (status, body) = post_json_raw(&client, daemon, &route_path, &payload).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if !status.is_success() {
        if status == reqwest::StatusCode::CONFLICT {
            return Err(anyhow::anyhow!(
                "review findings add failed (409): {}",
                body["error"].as_str().unwrap_or("slug already exists")
            ));
        }
        return Err(loopback_or_api_error(
            "review findings add",
            daemon,
            status,
            &body,
        ));
    }
    if !json {
        println!(
            "✓ finding {} added to review {id}",
            body["slug"].as_str().unwrap_or("?")
        );
    }
    Ok(())
}

const FINDING_DISPOSITIONS: &[&str] = &["agree", "dispute", "waive", "fix-later"];

/// `kb-code review disposition ID SLUG {agree,dispute,waive,fix-later,clear}
/// [-m NOTE] [--json]` — `PUT`/`DELETE
/// /api/reviews/{id}/findings/{slug}/disposition` (design doc §2 rows
/// 10-11). LOOPBACK-ONLY. `clear` issues the `DELETE`; every other action
/// issues the `PUT`.
async fn review_disposition_cmd(
    daemon: &str,
    id: i64,
    slug: &str,
    action: &str,
    note: Option<&str>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let path = format!("/api/reviews/{id}/findings/{slug}/disposition");
    let (status, body) = if action == "clear" {
        delete_json_raw(&client, daemon, &path, &[]).await?
    } else {
        if !FINDING_DISPOSITIONS.contains(&action) {
            anyhow::bail!(
                "review disposition: action must be agree|dispute|waive|fix-later|clear, \
                 got {action:?}"
            );
        }
        let mut payload = serde_json::json!({ "disposition": action });
        if let Some(n) = note {
            payload["note"] = serde_json::json!(n);
        }
        put_json_raw(&client, daemon, &path, &payload).await?
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if !status.is_success() {
        return Err(loopback_or_api_error(
            "review disposition",
            daemon,
            status,
            &body,
        ));
    }
    if !json {
        if action == "clear" {
            println!("✓ disposition cleared on finding {slug} (review {id})");
        } else {
            println!("✓ disposition {action} set on finding {slug} (review {id})");
        }
    }
    Ok(())
}

// --- PRR-R4: review pr-status / inbox / timeline ----------------------------

/// `kb-code review pr-status ID [--json]` — `GET /api/reviews/{id}/pr-status`
/// (design doc §2 row 12). The LOCAL half always prints; the LIVE half
/// prints `unavailable (<reason>)` when the daemon reports one.
async fn review_pr_status_cmd(daemon: &str, id: i64, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(
        &client,
        daemon,
        &format!("/api/reviews/{id}/pr-status"),
        &[],
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "review {id}  pr#{}  local_matches_pr={}  stale={}",
        body["pr_number"], body["local_matches_pr"], body["stale"],
    );
    println!(
        "  review_snapshot_head_sha={}",
        body["review_snapshot_head_sha"]
            .as_str()
            .unwrap_or("(none)"),
    );
    println!(
        "  latest_local_ps_tip_sha={}",
        body["latest_local_ps_tip_sha"].as_str().unwrap_or("?"),
    );
    match body["unavailable_reason"].as_str() {
        Some(r) => println!("  live: unavailable ({r})"),
        None => println!(
            "  live pr_head_sha={}  commits_behind={}",
            body["pr_head_sha"].as_str().unwrap_or("?"),
            body["commits_behind"]
                .as_i64()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
        ),
    }
    Ok(())
}

/// `kb-code review inbox [--repo R|--all-repos] [--state open][--limit N]
/// [--json]` — `GET /api/reviews/inbox` (design doc §2 row 13).
/// `--repo`/`--all-repos` are mutually exclusive; exactly one is required
/// (a CLI-side guard — the route itself treats an absent `repo` param as
/// "scan every configured repo," so this stops a caller from getting that
/// by accident rather than by an explicit `--all-repos`).
async fn review_inbox_cmd(
    daemon: &str,
    repo: Option<&str>,
    all_repos: bool,
    state: Option<&str>,
    limit: Option<usize>,
    json: bool,
) -> Result<()> {
    if repo.is_some() == all_repos {
        anyhow::bail!("review inbox: pass exactly one of --repo NAME or --all-repos");
    }
    let client = http_client()?;
    let mut query: Vec<(&str, &str)> = Vec::new();
    if let Some(r) = repo {
        query.push(("repo", r));
    }
    if let Some(s) = state {
        query.push(("state", s));
    }
    let limit_str = limit.map(|l| l.to_string());
    if let Some(ref s) = limit_str {
        query.push(("limit", s.as_str()));
    }
    let body = get_json(&client, daemon, "/api/reviews/inbox", &query).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let rows = body["reviews"].as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("(inbox empty)");
        return Ok(());
    }
    for r in &rows {
        let pr = match r["pr_number"].as_i64() {
            Some(n) => format!("#{n}"),
            None => "-".to_string(),
        };
        println!(
            "review {:<5} {:10} {pr:<6} unanswered={:<3} unresolved={:<3} drift={}  {}",
            r["review_id"],
            r["repo"].as_str().unwrap_or("?"),
            r["unanswered_questions"],
            r["unresolved_findings"],
            r["pr_head_drift"],
            r["title"].as_str().unwrap_or(""),
        );
    }
    Ok(())
}

/// `kb-code review timeline ID [--json]` — `GET /api/reviews/{id}/timeline`
/// (milestone plan arbitration #7).
async fn review_timeline_cmd(daemon: &str, id: i64, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(&client, daemon, &format!("/api/reviews/{id}/timeline"), &[]).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let events = body["events"].as_array().cloned().unwrap_or_default();
    if events.is_empty() {
        println!("(no timeline events)");
        return Ok(());
    }
    for e in &events {
        println!(
            "{:<12} {:<18} {}",
            e["at"],
            e["kind"].as_str().unwrap_or("?"),
            format_timeline_detail(e),
        );
    }
    Ok(())
}

fn format_timeline_detail(e: &serde_json::Value) -> String {
    match e["kind"].as_str().unwrap_or("") {
        "review_created" => format!("review {} ({})", e["review_id"], e["repo"]),
        "pr_bound" => format!(
            "pr#{} {}",
            e["pr_number"],
            e["pr_repo_slug"].as_str().unwrap_or("?")
        ),
        "patchset" => format!(
            "ps{}  {}",
            e["ps_number"],
            e["tip_sha"].as_str().unwrap_or("?")
        ),
        "findings_import" => format!(
            "{} findings imported (batch {})",
            e["count"],
            e["import_batch_id"].as_str().unwrap_or("?"),
        ),
        "finding_added" => format!(
            "{}  {}",
            e["slug"].as_str().unwrap_or("?"),
            e["title"].as_str().unwrap_or(""),
        ),
        "disposition" => format!(
            "{} -> {}",
            e["slug"].as_str().unwrap_or("?"),
            e["state"].as_str().unwrap_or("?"),
        ),
        "verdict" => format!("verdict={}", e["state"].as_str().unwrap_or("?")),
        "finding_published" => format!("{} published", e["slug"].as_str().unwrap_or("?")),
        "verdict_published" => "verdict published".to_string(),
        "comment" => format!(
            "{} by {} on {}",
            if e["is_reply"].as_bool().unwrap_or(false) {
                "reply"
            } else {
                "comment"
            },
            e["author"].as_str().unwrap_or("?"),
            e["path"].as_str().unwrap_or(""),
        ),
        other => other.to_string(),
    }
}

// --- PRR-R7: GitHub thread import --------------------------------------------

/// `kb-code review github-threads ID [--json]` — `GET /api/reviews/{id}/
/// github-threads` (design-addendum-2.md §A). GitHub stays the source of
/// truth (nothing persisted); the human-output tail prints an `unavailable`
/// line when the daemon reports one, same convention as `review pr-status`.
async fn review_github_threads_cmd(daemon: &str, id: i64, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(
        &client,
        daemon,
        &format!("/api/reviews/{id}/github-threads"),
        &[],
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "review {id}  pr#{}  ps={}  fetched_at={}",
        body["pr_number"], body["ps"], body["fetched_at"],
    );
    if let Some(r) = body["unavailable_reason"].as_str() {
        println!("  github: unavailable ({r})");
    }
    let threads = body["threads"].as_array().cloned().unwrap_or_default();
    if threads.is_empty() {
        println!("  (no github threads)");
        return Ok(());
    }
    for t in &threads {
        println!("  {}", format_github_thread_line(t));
        for r in t["replies"].as_array().cloned().unwrap_or_default() {
            println!("      -> {}", format_github_thread_line(&r));
        }
    }
    Ok(())
}

fn format_github_thread_line(t: &serde_json::Value) -> String {
    let position = if t["general"].as_bool().unwrap_or(false) {
        "general".to_string()
    } else if t["orphaned"].as_bool().unwrap_or(false) {
        "orphaned".to_string()
    } else if let Some(r) = t.get("resolved") {
        format!(
            "resolved line={} {}",
            r["line"],
            r["confidence"].as_str().unwrap_or("?"),
        )
    } else {
        "-".to_string()
    };
    let first_line = t["body"]
        .as_str()
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("");
    format!(
        "[{position}] {}  {}  {first_line}",
        t["author"].as_str().unwrap_or("?"),
        t["path"].as_str().unwrap_or(""),
    )
}

// --- PRR-R8: stale-backlog sweep --------------------------------------------

/// `kb-code review sweep [--repo R | --all-repos] [--include-closed]
/// [--json]` — `POST /api/reviews/sweep` (design-addendum-2 §B).
/// LOOPBACK-ONLY. `--repo`/`--all-repos` are mutually exclusive; exactly
/// one is required (same CLI-side guard as `review inbox`).
async fn review_sweep_cmd(
    daemon: &str,
    repo: Option<&str>,
    all_repos: bool,
    include_closed: bool,
    json: bool,
) -> Result<()> {
    if repo.is_some() == all_repos {
        anyhow::bail!("review sweep: pass exactly one of --repo NAME or --all-repos");
    }
    let mut payload =
        serde_json::json!({ "all_repos": all_repos, "include_closed": include_closed });
    if let Some(r) = repo {
        payload["repo"] = serde_json::json!(r);
    }
    let client = http_client()?;
    let (status, body) = post_json_raw(&client, daemon, "/api/reviews/sweep", &payload).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if !status.is_success() {
        return Err(loopback_or_api_error("review sweep", daemon, status, &body));
    }
    if json {
        return Ok(());
    }
    let summary = &body["summary"];
    println!(
        "swept={}  refreshed={}  unavailable={}  suggest_close={}",
        summary["swept"], summary["refreshed"], summary["unavailable"], summary["suggest_close"],
    );
    let rows = body["rows"].as_array().cloned().unwrap_or_default();
    for r in &rows {
        if let Some(reason) = r["unavailable_reason"].as_str() {
            println!(
                "review {:<5} pr#{:<6} unavailable ({reason})",
                r["review_id"], r["pr_number"],
            );
            continue;
        }
        let checks = &r["checks"];
        let flag = if r["suggest_close"].as_bool().unwrap_or(false) {
            "  SUGGEST-CLOSE"
        } else {
            ""
        };
        println!(
            "review {:<5} pr#{:<6} {:<8} drift={:<5} new_commits={:<4} checks[pass={} fail={} warn={} pending={}] decision={:<18} unanswered={:<3} verdict_stale={}{flag}",
            r["review_id"],
            r["pr_number"],
            r["pr_state"].as_str().unwrap_or("?"),
            r["head_drift"],
            r["new_head_commits"]
                .as_i64()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "?".to_string()),
            checks["pass"], checks["fail"], checks["warn"], checks["pending"],
            r["review_decision"].as_str().unwrap_or("-"),
            r["unanswered_questions"],
            r["verdict_stale"],
        );
    }
    Ok(())
}

// --- PRR-R9: disposition analytics ------------------------------------------

/// `kb-code review analytics [--repo R] [--from UNIX] [--to UNIX] [--json]`
/// — `GET /api/reviews/analytics` (design-addendum-2 §C).
async fn review_analytics_cmd(
    daemon: &str,
    repo: Option<&str>,
    from: Option<i64>,
    to: Option<i64>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut query: Vec<(&str, &str)> = Vec::new();
    if let Some(r) = repo {
        query.push(("repo", r));
    }
    let from_str = from.map(|n| n.to_string());
    if let Some(ref s) = from_str {
        query.push(("from", s.as_str()));
    }
    let to_str = to.map(|n| n.to_string());
    if let Some(ref s) = to_str {
        query.push(("to", s.as_str()));
    }
    let body = get_json(&client, daemon, "/api/reviews/analytics", &query).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "findings: {} (superseded: {})",
        body["total_findings"], body["superseded_count"],
    );
    for a in body["acceptance"].as_array().cloned().unwrap_or_default() {
        let rate = match a["rate"].as_f64() {
            Some(r) => format!("{:.0}%", r * 100.0),
            None => "n/a".to_string(),
        };
        println!(
            "  {:<10} accepted={:<3} rejected={:<3} risk_accepted={:<3} undecided={:<3} rate={rate}",
            a["severity"].as_str().unwrap_or("?"),
            a["accepted"], a["rejected"], a["risk_accepted"], a["undecided"],
        );
    }
    println!(
        "latency: n={} median={}s p90={}s",
        body["latency"]["n"],
        body["latency"]["median_secs"]
            .as_i64()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "n/a".to_string()),
        body["latency"]["p90_secs"]
            .as_i64()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "n/a".to_string()),
    );
    println!(
        "publish: published={} unpublished={}",
        body["publish"]["published"], body["publish"]["unpublished"],
    );
    let by_category = body["by_category"].as_array().cloned().unwrap_or_default();
    if !by_category.is_empty() {
        println!("by_category (top {}):", by_category.len());
        for c in &by_category {
            println!(
                "  {:<24} count={}",
                c["category"].as_str().unwrap_or("?"),
                c["count"],
            );
        }
    }
    let recurrence = body["recurrence"].as_array().cloned().unwrap_or_default();
    if !recurrence.is_empty() {
        println!("recurrence ({} pairs):", recurrence.len());
        for r in &recurrence {
            println!(
                "  {} @ {}  seen in {} reviews ({} findings)",
                r["category"].as_str().unwrap_or("?"),
                r["location_path"].as_str().unwrap_or("?"),
                r["review_count"],
                r["finding_count"],
            );
        }
    }
    Ok(())
}

// --- PRR-R5: GitHub-shaped export + publish recording -----------------------

/// `kb-code review export-github ID [--finding SLUG]... [--include-waived]
/// [--include-orphaned-as-general] [--json]` — `GET
/// /api/reviews/{id}/export/github` (design doc §2 row 14 / §3.2). The
/// human-output tail prints a LOUD warning + recommendation when
/// `stale_export` is `true` — this CLI never blocks the export on it (the
/// daemon hands back `commit_id` regardless, honestly labeled, per §3.2
/// point 4).
async fn review_export_github_cmd(
    daemon: &str,
    id: i64,
    finding: &[String],
    include_waived: bool,
    include_orphaned_as_general: bool,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let finding_slugs = if finding.is_empty() {
        None
    } else {
        Some(finding.join(","))
    };
    let mut query: Vec<(&str, &str)> = Vec::new();
    if let Some(s) = finding_slugs.as_deref() {
        query.push(("finding_slugs", s));
    }
    if include_waived {
        query.push(("include_waived", "true"));
    }
    if include_orphaned_as_general {
        query.push(("include_orphaned_as_general", "true"));
    }
    let body = get_json(
        &client,
        daemon,
        &format!("/api/reviews/{id}/export/github"),
        &query,
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    match body["event"].as_str() {
        Some(e) => println!("verdict event: {e}"),
        None => println!(
            "verdict event: none ({})",
            body["event_reason"].as_str().unwrap_or("no_verdict_set")
        ),
    }
    println!("commit_id: {}", body["commit_id"].as_str().unwrap_or("?"));
    let comments = body["comments"].as_array().cloned().unwrap_or_default();
    println!("comments: {}", comments.len());
    for c in &comments {
        println!(
            "  {}:{} [{}]  {}",
            c["path"].as_str().unwrap_or("?"),
            c["line"]
                .as_u64()
                .map(|n| n.to_string())
                .unwrap_or_default(),
            c["side"].as_str().unwrap_or("?"),
            c["finding_slug"].as_str().unwrap_or("?"),
        );
    }
    let general = body["general_comments"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if !general.is_empty() {
        println!("general_comments: {}", general.len());
        for g in &general {
            println!("  {}", g["finding_slug"].as_str().unwrap_or("?"));
        }
    }
    let skipped = body["skipped_orphaned"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if !skipped.is_empty() {
        println!("skipped_orphaned: {}", skipped.len());
        for s in &skipped {
            println!(
                "  {}  reason={}  (was ps{} line {})",
                s["finding_slug"].as_str().unwrap_or("?"),
                s["reason"].as_str().unwrap_or("?"),
                s["original"]["ps"].as_i64().unwrap_or(-1),
                s["original"]["line"]
                    .as_i64()
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "-".to_string()),
            );
        }
    }
    if body["stale_export"].as_bool().unwrap_or(false) {
        println!(
            "\n⚠ STALE EXPORT — this review's local patchset does not match the last-known \
             GitHub PR head. Run `kb-code review pr-status {id}` and, if needed, `kb-code \
             review snapshot {id}` BEFORE publishing any of the comments above."
        );
    }
    Ok(())
}

/// `kb-code review publish ID SLUG --url URL [--comment-id ID] [--json]` /
/// `kb-code review publish ID --verdict --url URL [--review-id ID] [--json]`
/// — `POST /api/reviews/{id}/findings/{slug}/published` /
/// `POST /api/reviews/{id}/verdict/published` (design doc §2 rows 15-16).
/// LOOPBACK-ONLY. Exactly one of `slug` / `--verdict` must be given.
#[allow(clippy::too_many_arguments)]
async fn review_publish_cmd(
    daemon: &str,
    id: i64,
    slug: Option<&str>,
    verdict: bool,
    url: &str,
    comment_id: Option<&str>,
    review_id: Option<&str>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    if verdict {
        if slug.is_some() {
            anyhow::bail!("review publish: pass SLUG or --verdict, not both");
        }
        let mut payload = serde_json::json!({ "github_review_url": url });
        if let Some(rid) = review_id {
            payload["github_review_id"] = serde_json::json!(rid);
        }
        let path = format!("/api/reviews/{id}/verdict/published");
        let (status, body) = post_json_raw(&client, daemon, &path, &payload).await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        if !status.is_success() {
            return Err(loopback_or_api_error(
                "review publish --verdict",
                daemon,
                status,
                &body,
            ));
        }
        if !json {
            println!("✓ verdict publish recorded for review {id}");
        }
    } else {
        let Some(slug) = slug else {
            anyhow::bail!("review publish: SLUG is required unless --verdict is set");
        };
        let mut payload = serde_json::json!({ "github_comment_url": url });
        if let Some(cid) = comment_id {
            payload["github_comment_id"] = serde_json::json!(cid);
        }
        let path = format!("/api/reviews/{id}/findings/{slug}/published");
        let (status, body) = post_json_raw(&client, daemon, &path, &payload).await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        if !status.is_success() {
            return Err(loopback_or_api_error(
                "review publish",
                daemon,
                status,
                &body,
            ));
        }
        if !json {
            println!("✓ publish recorded for finding {slug} (review {id})");
        }
    }
    Ok(())
}

/// `kb-code pr list --repo R [--json]` — `GET /api/prs` (existing route;
/// zero CLI coverage before PRR-R2).
async fn pr_list_cmd(daemon: &str, repo: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(&client, daemon, "/api/prs", &[("repo", repo)]).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if let Some(r) = body["unavailable_reason"].as_str() {
        println!("(unavailable: {r})");
        return Ok(());
    }
    for pr in body["prs"].as_array().cloned().unwrap_or_default() {
        println!(
            "#{:<5} {:40}  {} -> {}  by {}",
            pr["number"].as_i64().unwrap_or(0),
            truncate(pr["title"].as_str().unwrap_or("?"), 40),
            pr["head_ref"].as_str().unwrap_or("?"),
            pr["base_ref"].as_str().unwrap_or("?"),
            pr["author"].as_str().unwrap_or("?"),
        );
    }
    Ok(())
}

/// `kb-code pr show N --repo R [--json]` — `GET /api/prs/{n}` (PRR-R2,
/// design doc §2 row 2).
async fn pr_show_cmd(daemon: &str, repo: &str, number: u64, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(
        &client,
        daemon,
        &format!("/api/prs/{number}"),
        &[("repo", repo)],
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if let Some(r) = body["unavailable_reason"].as_str() {
        println!("(unavailable: {r})");
        return Ok(());
    }
    let pr = &body["pr"];
    println!(
        "#{}  {}  [{}{}]",
        pr["number"],
        pr["title"].as_str().unwrap_or("?"),
        pr["state"].as_str().unwrap_or("?"),
        if pr["merged"].as_bool().unwrap_or(false) {
            ", merged"
        } else {
            ""
        },
    );
    println!(
        "  {} -> {}  by {}",
        pr["head_ref"].as_str().unwrap_or("?"),
        pr["base_ref"].as_str().unwrap_or("?"),
        pr["author"].as_str().unwrap_or("?"),
    );
    if let Some(m) = pr["merge_state_status"].as_str() {
        println!("  merge_state_status: {m}");
    }
    let labels: Vec<&str> = pr["labels"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    if !labels.is_empty() {
        println!("  labels: {}", labels.join(", "));
    }
    Ok(())
}

/// `kb-code pr checks N --repo R [--json]` — `GET /api/prs/{n}/checks`
/// (PRR-R2, design doc §2 row 3).
async fn pr_checks_cmd(daemon: &str, repo: &str, number: u64, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(
        &client,
        daemon,
        &format!("/api/prs/{number}/checks"),
        &[("repo", repo)],
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if let Some(r) = body["unavailable_reason"].as_str() {
        println!("(unavailable: {r})");
        return Ok(());
    }
    let checks = body["checks"].as_array().cloned().unwrap_or_default();
    if checks.is_empty() {
        println!("(no checks)");
        return Ok(());
    }
    for c in &checks {
        println!(
            "{:8}  {:30}  {}",
            c["status"].as_str().unwrap_or("?"),
            c["name"].as_str().unwrap_or("?"),
            c["note"].as_str().unwrap_or(""),
        );
    }
    Ok(())
}

/// `kb-code pr comments N --repo R [--json]` — `GET /api/prs/{n}/comments`
/// (existing route; zero CLI coverage before PRR-R2).
async fn pr_comments_cmd(daemon: &str, repo: &str, number: u64, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(
        &client,
        daemon,
        &format!("/api/prs/{number}/comments"),
        &[("repo", repo)],
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if let Some(r) = body["unavailable_reason"].as_str() {
        println!("(unavailable: {r})");
        return Ok(());
    }
    for c in body["comments"].as_array().cloned().unwrap_or_default() {
        let loc = match (c["path"].as_str(), c["line"].as_i64()) {
            (Some(p), Some(l)) => format!("{p}:{l}"),
            (Some(p), None) => p.to_string(),
            _ => "(general)".to_string(),
        };
        println!(
            "{:20}  {:24}  {}",
            c["author"].as_str().unwrap_or("?"),
            loc,
            truncate(c["body"].as_str().unwrap_or(""), 60),
        );
    }
    Ok(())
}

/// `kb-code pr fetch N --repo R [--json]` — `POST /api/prs/fetch` (existing
/// route; zero CLI coverage before PRR-R2). LOOPBACK-ONLY.
async fn pr_fetch_cmd(daemon: &str, repo: &str, number: u32, json: bool) -> Result<()> {
    let client = http_client()?;
    let (status, body) = post_json_raw(
        &client,
        daemon,
        "/api/prs/fetch",
        &serde_json::json!({ "repo": repo, "number": number }),
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if !status.is_success() {
        return Err(loopback_or_api_error("pr fetch", daemon, status, &body));
    }
    if !json {
        println!(
            "✓ fetched {} -> {}",
            body["ref"].as_str().unwrap_or("?"),
            body["sha"].as_str().unwrap_or("?"),
        );
    }
    Ok(())
}

async fn suggest_put_cmd(
    daemon: &str,
    id: &str,
    message: Option<&str>,
    from_file: Option<&Path>,
    json: bool,
) -> Result<()> {
    let replacement = match (message, from_file) {
        (Some(m), None) => m.to_string(),
        (None, Some(p)) => std::fs::read_to_string(p)
            .with_context(|| format!("read --from-file {}", p.display()))?,
        _ => anyhow::bail!("kb-code suggest: pass -m TEXT or --from-file F"),
    };
    let client = http_client()?;
    let (status, body) = put_json_raw(
        &client,
        daemon,
        &format!("/api/annotations/{id}/suggestion"),
        &serde_json::json!({ "replacement": replacement }),
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if !status.is_success() {
        return Err(annotation_api_error(
            &format!("put suggestion on {id:?}"),
            status,
            &body,
        ));
    }
    if !json {
        let applied = body["applied"].as_bool().unwrap_or(false);
        println!(
            "✓ suggestion on {id}  applied={applied}  {} byte(s)",
            replacement.len()
        );
    }
    Ok(())
}

async fn suggest_list_cmd(daemon: &str, review: i64, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(
        &client,
        daemon,
        &format!("/api/reviews/{review}/comments"),
        &[("all", "true")],
    )
    .await?;
    let suggestions = suggestions_from_comments(&body);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "review_id": review,
                "suggestions": suggestions,
            }))?
        );
        return Ok(());
    }
    if suggestions.is_empty() {
        println!("(no suggestions on review {review})");
        return Ok(());
    }
    for s in &suggestions {
        let status = if s["applied"].as_bool().unwrap_or(false) {
            "applied"
        } else {
            "pending"
        };
        println!(
            "{}  {}:{}  {status}  {}",
            s["id"].as_str().unwrap_or("?"),
            s["path"].as_str().unwrap_or("?"),
            s["line"]
                .as_u64()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "?".into()),
            s["replacement"]
                .as_str()
                .unwrap_or("")
                .lines()
                .next()
                .unwrap_or(""),
        );
    }
    Ok(())
}

/// Comments that carry a non-null `suggestion`. Stable `--json` field
/// names for LLM consumers.
fn suggestions_from_comments(body: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for g in body["groups"].as_array().cloned().unwrap_or_default() {
        let path = g["path"].as_str().unwrap_or("").to_string();
        for c in g["comments"].as_array().cloned().unwrap_or_default() {
            let Some(sug) = c.get("suggestion") else {
                continue;
            };
            if sug.is_null() {
                continue;
            }
            out.push(serde_json::json!({
                "id": c["id"],
                "path": c.get("path").cloned().unwrap_or(serde_json::json!(path)),
                "line": c["resolution"]["line"],
                "author": c["author"],
                "intent": c["intent"],
                "body": c["body"],
                "replacement": sug["replacement"],
                "original": sug["original"],
                "applied": sug["applied"],
                "applied_at": sug["applied_at"],
            }));
        }
    }
    out
}

async fn suggest_apply_cmd(daemon: &str, id: &str, resolve: bool, json: bool) -> Result<()> {
    let client = http_client()?;
    let (status, body) = post_json_raw(
        &client,
        daemon,
        &format!("/api/annotations/{id}/apply"),
        &serde_json::json!({ "resolve": resolve }),
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if status == reqwest::StatusCode::CONFLICT {
        if !json {
            print!("{}", format_apply_drift(&body));
        }
        anyhow::bail!("suggestion apply conflict on {id}");
    }
    if !status.is_success() {
        return Err(loopback_or_api_error(
            &format!("apply suggestion {id:?}"),
            daemon,
            status,
            &body,
        ));
    }
    if body["already_applied"].as_bool().unwrap_or(false) {
        if !json {
            println!("suggestion already applied (no change)");
        }
        return Ok(());
    }
    if !json {
        println!(
            "✓ applied {id}  {}{}",
            body["path"].as_str().unwrap_or("?"),
            body["line"]
                .as_u64()
                .map(|n| format!(":{n}"))
                .unwrap_or_default(),
        );
    }
    Ok(())
}

/// 409 expected/found, line-by-line, ≤20 lines each side.
fn format_apply_drift(body: &serde_json::Value) -> String {
    let expected = body["expected"].as_str().unwrap_or("");
    let found = body["found"].as_str().unwrap_or("");
    let line = body["resolved_line"].as_u64().unwrap_or(0);
    let error = body["error"].as_str().unwrap_or("working-tree drift");
    let mut out = format!("suggestion apply conflict at line {line}: {error}\n");
    out.push_str("--- expected\n");
    out.push_str(&format_capped_lines(expected, 20));
    out.push_str("--- found\n");
    out.push_str(&format_capped_lines(found, 20));
    out
}

fn format_capped_lines(text: &str, cap: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let shown = lines.len().min(cap);
    let mut out = String::new();
    for line in lines.iter().take(shown) {
        out.push_str(line);
        out.push('\n');
    }
    if lines.len() > cap {
        out.push_str(&format!(
            "… truncated ({} more line(s))\n",
            lines.len() - cap
        ));
    }
    out
}

async fn suggest_drop_cmd(daemon: &str, id: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let (status, body) = delete_json_raw(
        &client,
        daemon,
        &format!("/api/annotations/{id}/suggestion"),
        &[],
    )
    .await?;
    if json && !body.is_null() {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if !status.is_success() {
        return Err(annotation_api_error(
            &format!("drop suggestion on {id:?}"),
            status,
            &body,
        ));
    }
    if json && body.is_null() {
        println!("{}", serde_json::json!({ "id": id, "dropped": true }));
    }
    if !json {
        println!("✓ dropped suggestion on {id}");
    }
    Ok(())
}

/// `kb-code suggest apply-batch ID... [--resolve] [--json]` — PRR-R10.
/// `POST /api/annotations/apply-batch`. A 409 (verify-phase failure)
/// prints every id's verdict and exits non-zero with NOTHING written; a
/// mid-batch IO failure (rare — every id already passed verify) prints
/// the honest `{applied, restored, failed}` record.
async fn suggest_apply_batch_cmd(
    daemon: &str,
    ids: &[String],
    resolve: bool,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let (status, body) = post_json_raw(
        &client,
        daemon,
        "/api/annotations/apply-batch",
        &serde_json::json!({ "annotation_ids": ids, "resolve_threads": resolve }),
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if status == reqwest::StatusCode::CONFLICT {
        if !json {
            print_batch_verdicts(&body);
        }
        anyhow::bail!("apply-batch verify failed — nothing written");
    }
    if let Some(failed) = body.get("failed").filter(|v| !v.is_null()) {
        if !json {
            println!(
                "✗ apply-batch failed mid-batch on {} ({}): {}",
                failed["id"].as_str().unwrap_or("?"),
                failed["path"].as_str().unwrap_or("?"),
                failed["error"].as_str().unwrap_or("?"),
            );
            for r in body["restored"].as_array().cloned().unwrap_or_default() {
                println!("  restored {}", r.as_str().unwrap_or("?"));
            }
            for a in body["applied"].as_array().cloned().unwrap_or_default() {
                println!(
                    "  still on disk (restore failed): {} {}",
                    a["id"].as_str().unwrap_or("?"),
                    a["path"].as_str().unwrap_or("?"),
                );
            }
        }
        anyhow::bail!("apply-batch failed mid-batch");
    }
    if !status.is_success() {
        return Err(loopback_or_api_error("apply-batch", daemon, status, &body));
    }
    if !json {
        let applied = body["applied"].as_array().cloned().unwrap_or_default();
        println!("✓ applied {} suggestion(s)", applied.len());
        for a in &applied {
            println!(
                "  {}  {}{}",
                a["id"].as_str().unwrap_or("?"),
                a["path"].as_str().unwrap_or("?"),
                a["line"]
                    .as_u64()
                    .map(|n| format!(":{n}"))
                    .unwrap_or_default(),
            );
        }
    }
    Ok(())
}

/// Per-id verdict lines for `suggest_apply_batch_cmd`'s 409 path.
fn print_batch_verdicts(body: &serde_json::Value) {
    for v in body["verdicts"].as_array().cloned().unwrap_or_default() {
        let id = v["id"].as_str().unwrap_or("?");
        if v["ok"].as_bool().unwrap_or(false) {
            println!("  ok    {id}");
        } else {
            let kind = v["error"]["kind"].as_str().unwrap_or("?");
            let detail = v["error"]["detail"].as_str().unwrap_or("");
            println!("  FAIL  {id}  {kind}: {detail}");
        }
    }
}

async fn annotate_batch_cmd(
    daemon: &str,
    repo: &str,
    file: Option<&Path>,
    json: bool,
) -> Result<()> {
    let raw = match file {
        Some(p) => std::fs::read_to_string(p)
            .with_context(|| format!("read batch file {}", p.display()))?,
        None => {
            let mut s = String::new();
            std::io::stdin()
                .read_to_string(&mut s)
                .context("read batch ops from stdin")?;
            s
        }
    };
    let parsed: serde_json::Value = serde_json::from_str(&raw).context("parse batch ops JSON")?;
    let ops = if parsed.is_array() {
        parsed
    } else if parsed.get("ops").map(|v| v.is_array()).unwrap_or(false) {
        parsed["ops"].clone()
    } else {
        anyhow::bail!("batch file must be a JSON array of ops or {{\"ops\": […]}}");
    };
    let client = http_client()?;
    let (status, body) = post_json_raw(
        &client,
        daemon,
        "/api/annotations/batch",
        &serde_json::json!({ "repo": repo, "ops": ops }),
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if !status.is_success() {
        return Err(annotation_api_error("annotation batch", status, &body));
    }
    if !json {
        let applied = body["applied"].as_u64().unwrap_or(0);
        let changed = body["changed"].as_bool().unwrap_or(false);
        let created = body["created_ids"].as_array().map(|a| a.len()).unwrap_or(0);
        println!("applied={applied}  created_ids={created}  changed={changed}");
        if let Some(ids) = body["created_ids"].as_array() {
            for id in ids {
                if let Some(s) = id.as_str() {
                    println!("  {s}");
                }
            }
        }
    }
    Ok(())
}

// --- reading sets (Phase E3) ---------------------------------------------

/// Generic `PATCH` counterpart to [`post_json_raw`] — takes a full PATH
/// (unlike [`patch_json_raw`], hardcoded to `/api/annotations/{id}` for its
/// one existing caller); `kb-code set rm`'s span-removal PATCH needs its
/// own path (`/api/sets/{id}`).
async fn patch_json_path(
    client: &reqwest::Client,
    daemon: &str,
    path: &str,
    body: &serde_json::Value,
) -> Result<(reqwest::StatusCode, serde_json::Value)> {
    let url = format!("{}{path}", daemon.trim_end_matches('/'));
    let resp = client
        .patch(&url)
        .json(body)
        .send()
        .await
        .with_context(|| format!("PATCH {url} — is kb-code-server running at {daemon}?"))?;
    let status = resp.status();
    let body = resp
        .json()
        .await
        .with_context(|| format!("parse {url} response as JSON"))?;
    Ok((status, body))
}

/// Build one `SpanInput`-shaped JSON object (`kb_code_server::
/// reading_sets::SpanInput`'s wire shape) from a parsed
/// `(path, line_start, line_end)` triple plus optional `note`/`ref`.
fn span_json(
    path: &str,
    line_start: Option<u32>,
    line_end: Option<u32>,
    git_ref: Option<&str>,
    note: Option<&str>,
) -> serde_json::Value {
    let mut v = serde_json::json!({ "path": path });
    if let Some(s) = line_start {
        v["line_start"] = serde_json::json!(s);
    }
    if let Some(e) = line_end {
        v["line_end"] = serde_json::json!(e);
    }
    if let Some(r) = git_ref {
        v["ref"] = serde_json::json!(r);
    }
    if let Some(n) = note {
        v["note"] = serde_json::json!(n);
    }
    v
}

/// Resolve a `NAME-OR-ID` (every `kb-code set {show,add,rm,delete}` and
/// `kb-code pack --set`'s common argument) against `GET /api/sets?repo=`:
/// an EXACT name match wins first, else a UNIQUE id PREFIX match — the same
/// "exact then prefix" ladder philosophy `join::ladder`'s own sha
/// resolution follows, applied here to a human-typed identifier instead of
/// a commit sha.
async fn resolve_set_id(
    client: &reqwest::Client,
    daemon: &str,
    repo: &str,
    name_or_id: &str,
) -> Result<String> {
    let body = get_json(client, daemon, "/api/sets", &[("repo", repo)]).await?;
    let sets = body["sets"].as_array().cloned().unwrap_or_default();
    if let Some(s) = sets.iter().find(|s| s["name"].as_str() == Some(name_or_id)) {
        return Ok(s["id"].as_str().unwrap_or_default().to_string());
    }
    let prefix_matches: Vec<&serde_json::Value> = sets
        .iter()
        .filter(|s| {
            s["id"]
                .as_str()
                .is_some_and(|id| id.starts_with(name_or_id))
        })
        .collect();
    match prefix_matches.len() {
        1 => Ok(prefix_matches[0]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string()),
        0 => anyhow::bail!("no reading set named or id-prefixed {name_or_id:?} in repo {repo:?}"),
        n => {
            anyhow::bail!("{name_or_id:?} matches {n} reading sets by id prefix — be more specific")
        }
    }
}

// --- Phase N: bookmarks + todos -----------------------------------------

/// `kb-code bookmarks [--repo R]` — `GET /api/bookmarks?repo=`. When
/// `--repo` is omitted, list every configured repo's bookmarks (one
/// `GET /api/repos` then one bookmarks call per name).
async fn bookmarks_list_cmd(daemon: &str, repo: Option<&str>, json: bool) -> Result<()> {
    let client = http_client()?;
    let repos: Vec<String> = match repo {
        Some(r) => vec![r.to_string()],
        None => {
            let body = get_json(&client, daemon, "/api/repos", &[]).await?;
            body["repos"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r["name"].as_str().map(|s| s.to_string()))
                .collect()
        }
    };
    if repos.is_empty() {
        println!("(no repos configured)");
        return Ok(());
    }
    let mut all = Vec::new();
    for r in &repos {
        let body = get_json(&client, daemon, "/api/bookmarks", &[("repo", r)]).await?;
        if let Some(arr) = body["bookmarks"].as_array() {
            for b in arr {
                all.push(b.clone());
            }
        }
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "bookmarks": all }))?
        );
        return Ok(());
    }
    if all.is_empty() {
        println!("(no bookmarks)");
        return Ok(());
    }
    println!("{:<6} {:<4} {:<40} note", "id", "mn", "path:line");
    for b in &all {
        let id = b["id"].as_i64().unwrap_or(0);
        let mn = b["mnemonic"].as_str().unwrap_or("-");
        let path = b["path"].as_str().unwrap_or("?");
        let line = b["line"].as_i64().unwrap_or(0);
        let note = b["note"].as_str().unwrap_or("");
        println!("{id:<6} {mn:<4} {path}:{line:<5} {note}");
    }
    Ok(())
}

/// Parse `PATH:LINE` for `kb-code bookmark`.
fn parse_bookmark_target(raw: &str) -> Result<(String, u32)> {
    let Some((path, line_s)) = raw.rsplit_once(':') else {
        anyhow::bail!("bookmark target must be PATH:LINE, got {raw:?}");
    };
    if path.is_empty() {
        anyhow::bail!("bookmark path must not be empty");
    }
    let line: u32 = line_s
        .parse()
        .with_context(|| format!("bookmark line must be an integer, got {line_s:?}"))?;
    if line < 1 {
        anyhow::bail!("bookmark line must be >= 1 (1-based)");
    }
    Ok((path.to_string(), line))
}

/// `kb-code bookmark <PATH>:<LINE> --repo R [--mnemonic c] [--note t]`.
async fn bookmark_create_cmd(
    daemon: &str,
    repo: &str,
    target: &str,
    mnemonic: Option<&str>,
    note: Option<&str>,
    json: bool,
) -> Result<()> {
    let (path, line) = parse_bookmark_target(target)?;
    let mut payload = serde_json::json!({ "repo": repo, "path": path, "line": line });
    if let Some(m) = mnemonic {
        payload["mnemonic"] = serde_json::json!(m);
    }
    if let Some(n) = note {
        payload["note"] = serde_json::json!(n);
    }
    let client = http_client()?;
    let (status, body) = post_json_raw(&client, daemon, "/api/bookmarks", &payload).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if status != reqwest::StatusCode::CREATED {
        return Err(annotation_api_error("create bookmark", status, &body));
    }
    if !json {
        let id = body["id"].as_i64().unwrap_or(0);
        let mn = body["mnemonic"]
            .as_str()
            .map(|m| format!(" mnemonic={m}"))
            .unwrap_or_default();
        println!("✓ bookmarked {path}:{line} (id={id}{mn})");
    }
    Ok(())
}

/// `kb-code bookmark rm <ID-or-mnemonic> [--repo R]`.
async fn bookmark_rm_cmd(
    daemon: &str,
    repo: Option<&str>,
    id_or_mnemonic: &str,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let id = if let Ok(n) = id_or_mnemonic.parse::<i64>() {
        n
    } else {
        // Mnemonic path — need a repo, list + find.
        let repo =
            repo.ok_or_else(|| anyhow::anyhow!("--repo is required when deleting by mnemonic"))?;
        let body = get_json(&client, daemon, "/api/bookmarks", &[("repo", repo)]).await?;
        let found = body["bookmarks"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|b| b["mnemonic"].as_str() == Some(id_or_mnemonic));
        match found {
            Some(b) => b["id"]
                .as_i64()
                .ok_or_else(|| anyhow::anyhow!("bookmark id missing in list response"))?,
            None => anyhow::bail!("no bookmark with mnemonic {id_or_mnemonic:?} in repo {repo:?}"),
        }
    };
    let url = format!("{}/api/bookmarks/{id}", daemon.trim_end_matches('/'));
    let resp = client
        .delete(&url)
        .send()
        .await
        .with_context(|| format!("DELETE {url} — is kb-code-server running at {daemon}?"))?;
    let status = resp.status();
    if status == reqwest::StatusCode::NO_CONTENT {
        if json {
            println!("{}", serde_json::json!({ "id": id, "deleted": true }));
        } else {
            println!("✓ deleted bookmark {id}");
        }
        return Ok(());
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .with_context(|| format!("parse {url} response as JSON"))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    Err(annotation_api_error(
        &format!("delete bookmark {id_or_mnemonic}"),
        status,
        &body,
    ))
}

/// `kb-code todos [--repo R] [--marker M] [--path-prefix P]`.
async fn todos_list_cmd(
    daemon: &str,
    repo: Option<&str>,
    marker: Option<&str>,
    path_prefix: Option<&str>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let repos: Vec<String> = match repo {
        Some(r) => vec![r.to_string()],
        None => {
            let body = get_json(&client, daemon, "/api/repos", &[]).await?;
            body["repos"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r["name"].as_str().map(|s| s.to_string()))
                .collect()
        }
    };
    if repos.is_empty() {
        println!("(no repos configured)");
        return Ok(());
    }
    let mut all = Vec::new();
    for r in &repos {
        let mut q: Vec<(&str, &str)> = vec![("repo", r.as_str())];
        if let Some(m) = marker {
            q.push(("marker", m));
        }
        if let Some(p) = path_prefix {
            q.push(("path_prefix", p));
        }
        let body = get_json(&client, daemon, "/api/todos", &q).await?;
        if let Some(arr) = body["items"].as_array() {
            for item in arr {
                let mut row = item.clone();
                row["repo"] = serde_json::json!(r);
                all.push(row);
            }
        }
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "items": all }))?
        );
        return Ok(());
    }
    if all.is_empty() {
        println!("(no todos)");
        return Ok(());
    }
    for item in &all {
        let path = item["path"].as_str().unwrap_or("?");
        let line = item["line"].as_i64().unwrap_or(0);
        let marker = item["marker"].as_str().unwrap_or("?");
        let text = item["text"].as_str().unwrap_or("");
        println!("{path}:{line}\t{marker}\t{text}");
    }
    Ok(())
}

/// `kb-code set list --repo R` — `GET /api/sets?repo=`.
async fn set_list_cmd(daemon: &str, repo: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(&client, daemon, "/api/sets", &[("repo", repo)]).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let sets = body["sets"].as_array().cloned().unwrap_or_default();
    if sets.is_empty() {
        println!("(no reading sets in {repo})");
        return Ok(());
    }
    for s in &sets {
        println!(
            "{:<14} {:<30} {:>3} span(s)  {}",
            s["id"].as_str().unwrap_or("?"),
            s["name"].as_str().unwrap_or("?"),
            s["span_count"].as_u64().unwrap_or(0),
            s["description"].as_str().unwrap_or(""),
        );
    }
    Ok(())
}

/// One `SetView` (`GET`/`POST`/`PATCH /api/sets[/{id}]`'s shared response
/// shape), human-formatted.
fn print_set_view(body: &serde_json::Value) {
    println!(
        "{}  ({})",
        body["name"].as_str().unwrap_or("?"),
        body["id"].as_str().unwrap_or("?"),
    );
    if let Some(d) = body["description"].as_str() {
        if !d.is_empty() {
            println!("  {d}");
        }
    }
    for s in body["spans"].as_array().cloned().unwrap_or_default() {
        let ordinal = s["ordinal"].as_u64().unwrap_or(0);
        let path = s["path"].as_str().unwrap_or("?");
        let lines = match (s["line_start"].as_u64(), s["line_end"].as_u64()) {
            (Some(a), Some(b)) if a == b => format!(":{a}"),
            (Some(a), Some(b)) => format!(":{a}-{b}"),
            _ => String::new(),
        };
        let note = s["note"]
            .as_str()
            .map(|n| format!("  # {n}"))
            .unwrap_or_default();
        println!("  [{ordinal}] {path}{lines}{note}");
    }
}

/// `kb-code set show <NAME-OR-ID> --repo R` — `GET /api/sets/{id}`.
async fn set_show_cmd(daemon: &str, repo: &str, name_or_id: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let id = resolve_set_id(&client, daemon, repo, name_or_id).await?;
    let body = get_json(&client, daemon, &format!("/api/sets/{id}"), &[]).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    print_set_view(&body);
    Ok(())
}

/// `kb-code set create <NAME> --repo R [-d DESC] [--span PATH[:START[-END]]]...`
/// — `POST /api/sets`.
async fn set_create_cmd(
    daemon: &str,
    repo: &str,
    name: &str,
    description: Option<&str>,
    spans: &[String],
    json: bool,
) -> Result<()> {
    let span_values: Vec<serde_json::Value> = spans
        .iter()
        .map(|raw| {
            let (path, start, end) = parse_span_arg(raw);
            span_json(&path, start, end, None, None)
        })
        .collect();
    let mut payload = serde_json::json!({ "repo": repo, "name": name, "spans": span_values });
    if let Some(d) = description {
        payload["description"] = serde_json::json!(d);
    }
    let client = http_client()?;
    let (status, body) = post_json_raw(&client, daemon, "/api/sets", &payload).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if status != reqwest::StatusCode::CREATED {
        return Err(annotation_api_error("create reading set", status, &body));
    }
    if !json {
        println!(
            "✓ created set {name:?} ({})",
            body["id"].as_str().unwrap_or("?")
        );
    }
    Ok(())
}

/// `kb-code set add <NAME-OR-ID> PATH[:START[-END]] [--note N] [--ref REV]`
/// — `POST /api/sets/{id}/spans`.
async fn set_add_cmd(
    daemon: &str,
    repo: &str,
    name_or_id: &str,
    span: &str,
    note: Option<&str>,
    git_ref: Option<&str>,
    json: bool,
) -> Result<()> {
    let (path, start, end) = parse_span_arg(span);
    let client = http_client()?;
    let id = resolve_set_id(&client, daemon, repo, name_or_id).await?;
    let payload = span_json(&path, start, end, git_ref, note);
    let (status, body) =
        post_json_raw(&client, daemon, &format!("/api/sets/{id}/spans"), &payload).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if status != reqwest::StatusCode::CREATED {
        return Err(annotation_api_error("add span", status, &body));
    }
    if !json {
        println!("✓ added {path} to {name_or_id}");
    }
    Ok(())
}

/// `kb-code set rm <NAME-OR-ID> <ORDINAL>` — reads the current spans,
/// drops the one at `ORDINAL`, sends the rest back as a full-replacement
/// `PATCH` (there is no dedicated `DELETE /api/sets/{id}/spans/{ordinal}`
/// route — the wire only offers append + full-replace, see
/// `reading_sets`'s module doc).
async fn set_rm_cmd(
    daemon: &str,
    repo: &str,
    name_or_id: &str,
    ordinal: usize,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let id = resolve_set_id(&client, daemon, repo, name_or_id).await?;
    let current = get_json(&client, daemon, &format!("/api/sets/{id}"), &[]).await?;
    let mut spans: Vec<serde_json::Value> =
        current["spans"].as_array().cloned().unwrap_or_default();
    let before = spans.len();
    spans.retain(|s| s["ordinal"].as_u64() != Some(ordinal as u64));
    if spans.len() == before {
        anyhow::bail!("{name_or_id}: no span at ordinal {ordinal}");
    }
    let payload = serde_json::json!({ "spans": spans });
    let (status, body) =
        patch_json_path(&client, daemon, &format!("/api/sets/{id}"), &payload).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if !status.is_success() {
        return Err(annotation_api_error("remove span", status, &body));
    }
    if !json {
        println!("✓ removed span {ordinal} from {name_or_id}");
    }
    Ok(())
}

/// `kb-code set delete <NAME-OR-ID> [--yes]` — `DELETE /api/sets/{id}`
/// (cascades to spans). Prompts for confirmation unless `yes` — same
/// convention as `annotate_delete_cmd`.
async fn set_delete_cmd(
    daemon: &str,
    repo: &str,
    name_or_id: &str,
    yes: bool,
    json: bool,
) -> Result<()> {
    if !yes {
        eprint!("delete reading set {name_or_id:?}? [y/N] ");
        std::io::stderr().flush().ok();
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).ok();
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("aborted");
            return Ok(());
        }
    }

    let client = http_client()?;
    let id = resolve_set_id(&client, daemon, repo, name_or_id).await?;
    let url = format!("{}/api/sets/{id}", daemon.trim_end_matches('/'));
    let resp = client
        .delete(&url)
        .send()
        .await
        .with_context(|| format!("DELETE {url} — is kb-code-server running at {daemon}?"))?;
    let status = resp.status();
    if status == reqwest::StatusCode::NO_CONTENT {
        if json {
            println!("{}", serde_json::json!({ "id": id, "deleted": true }));
        } else {
            println!("✓ deleted {name_or_id} ({id})");
        }
        return Ok(());
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .with_context(|| format!("parse {url} response as JSON"))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    Err(annotation_api_error(
        &format!("delete set {name_or_id:?}"),
        status,
        &body,
    ))
}

/// `kb-code set from-session <SESSION_ID> --repo R [--name N]` — `POST
/// /api/sets/from-session`. LOOPBACK-ONLY (same gate as
/// `session-diff`/`checkout`) — only works pointed at a daemon this CLI
/// can reach as a loopback peer.
async fn set_from_session_cmd(
    daemon: &str,
    repo: &str,
    session_id: &str,
    name: Option<&str>,
    json: bool,
) -> Result<()> {
    let mut payload = serde_json::json!({ "repo": repo, "session_id": session_id });
    if let Some(n) = name {
        payload["name"] = serde_json::json!(n);
    }
    let client = http_client()?;
    let (status, body) = post_json_raw(&client, daemon, "/api/sets/from-session", &payload).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if status != reqwest::StatusCode::CREATED {
        return Err(annotation_api_error(
            "materialize set from session",
            status,
            &body,
        ));
    }
    if !json {
        println!(
            "✓ created set {:?} ({}) from session {session_id}",
            body["name"].as_str().unwrap_or("?"),
            body["id"].as_str().unwrap_or("?"),
        );
        print_set_view(&body);
    }
    Ok(())
}

/// `kb-code set from-doc <KB> <DOC> --repo R [--name N]` — `POST
/// /api/sets/from-doc` (DCB-W3.C/R23). LOOPBACK-ONLY (same gate as
/// `from-session`/`checkout`) — only works pointed at a daemon this CLI can
/// reach as a loopback peer.
async fn set_from_doc_cmd(
    daemon: &str,
    repo: &str,
    kb: &str,
    doc: &str,
    name: Option<&str>,
    json: bool,
) -> Result<()> {
    let mut payload = serde_json::json!({ "repo": repo, "kb": kb, "doc": doc });
    if let Some(n) = name {
        payload["name"] = serde_json::json!(n);
    }
    let client = http_client()?;
    let (status, body) = post_json_raw(&client, daemon, "/api/sets/from-doc", &payload).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if status != reqwest::StatusCode::CREATED {
        return Err(annotation_api_error(
            "materialize set from doc",
            status,
            &body,
        ));
    }
    if !json {
        println!(
            "✓ created set {:?} ({}) from {kb}/{doc}",
            body["name"].as_str().unwrap_or("?"),
            body["id"].as_str().unwrap_or("?"),
        );
        print_set_view(&body);
    }
    Ok(())
}

// --- S1: `kb-code scip ingest` ---------------------------------------------

/// The structured outcome of one `scip ingest` run — [`scip_ingest_core`]'s
/// return type. Factored out of `scip_ingest_cmd` (PRR-N12, N2) so `scip
/// run` can fold a chained ingest's REAL counts into its own aggregate
/// report instead of relying on `scip_ingest_cmd`'s side-effecting
/// `println!`s (which would otherwise emit one JSON document PER repo under
/// `--all --json`, breaking single-document output).
struct ScipIngestOutcome {
    docs_received: u64,
    docs_accepted: u64,
    occurrences_written: u64,
    cli_skipped_unreadable: usize,
    skipped: Vec<serde_json::Value>,
}

/// Parses INDEX (a `.scip` protobuf file, `::scip::types::Index`), reads
/// each document's CURRENT file bytes off the SAME working tree the index
/// was generated against (resolved via the index's own
/// `Metadata.project_root` — see [`scip_project_root`]), hashes them the
/// same git-compatible way the daemon itself does
/// (`kb_code_server::ingest::git_blob_hash`), and POSTs batches of `{path,
/// blob_hash, occurrences}` to `POST /api/scip/ingest`. Pure data in, data
/// out — no printing; see `scip_ingest_cmd` (the `Ingest` subcommand's own
/// thin printer) and `scip_run_cmd` (`Run`'s chained caller) for the two
/// consumers.
async fn scip_ingest_core(
    daemon: &str,
    repo: &str,
    index_path: &Path,
    batch_size: usize,
) -> Result<ScipIngestOutcome> {
    let bytes =
        std::fs::read(index_path).with_context(|| format!("read {}", index_path.display()))?;
    let index: ::scip::types::Index = protobuf::Message::parse_from_bytes(&bytes)
        .with_context(|| format!("parse {} as a SCIP index", index_path.display()))?;
    let project_root = scip_project_root(&index).ok_or_else(|| {
        anyhow::anyhow!(
            "{}: no metadata.project_root — can't locate the working tree it was generated against",
            index_path.display()
        )
    })?;

    let mapped = scip_map::map_index(&index);
    let mut docs_with_hash: Vec<serde_json::Value> = Vec::with_capacity(mapped.len());
    let mut cli_skipped_unreadable = 0usize;
    for doc in mapped {
        let abs = project_root.join(&doc.path);
        let Ok(src) = std::fs::read(&abs) else {
            cli_skipped_unreadable += 1;
            continue;
        };
        let blob_hash = kb_code_server::ingest::git_blob_hash(&src);
        docs_with_hash.push(serde_json::json!({
            "path": doc.path,
            "blob_hash": blob_hash,
            "occurrences": doc.occurrences,
        }));
    }

    let client = http_client()?;
    let mut docs_received = 0u64;
    let mut docs_accepted = 0u64;
    let mut occurrences_written = 0u64;
    let mut skipped: Vec<serde_json::Value> = Vec::new();
    for batch in docs_with_hash.chunks(batch_size.max(1)) {
        let payload = serde_json::json!({ "repo": repo, "docs": batch });
        let (status, body) = post_json_raw(&client, daemon, "/api/scip/ingest", &payload).await?;
        if !status.is_success() {
            let msg = body["error"].as_str().unwrap_or("scip ingest failed");
            anyhow::bail!("scip ingest failed: {msg}");
        }
        docs_received += body["docs_received"].as_u64().unwrap_or(0);
        docs_accepted += body["docs_accepted"].as_u64().unwrap_or(0);
        occurrences_written += body["occurrences_written"].as_u64().unwrap_or(0);
        if let Some(arr) = body["skipped"].as_array() {
            skipped.extend(arr.iter().cloned());
        }
    }

    Ok(ScipIngestOutcome {
        docs_received,
        docs_accepted,
        occurrences_written,
        cli_skipped_unreadable,
        skipped,
    })
}

/// `kb-code scip ingest <INDEX> --repo NAME` — see `Cmd::Scip`'s own doc for
/// the `rust-analyzer scip .` / `scip-typescript index` generation
/// commands. Thin printer over [`scip_ingest_core`]: prints the
/// accepted/skipped counts HONESTLY — a document unreadable from THIS
/// machine (deleted/moved since the index was generated) is counted
/// separately from a document the DAEMON itself skipped as
/// stale/untracked/unsupported-lang, since the two are different failure
/// points.
async fn scip_ingest_cmd(
    daemon: &str,
    repo: &str,
    index_path: &Path,
    batch_size: usize,
    json: bool,
) -> Result<()> {
    let outcome = scip_ingest_core(daemon, repo, index_path, batch_size).await?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "repo": repo,
                "docs_received": outcome.docs_received,
                "docs_accepted": outcome.docs_accepted,
                "occurrences_written": outcome.occurrences_written,
                "cli_skipped_unreadable": outcome.cli_skipped_unreadable,
                "skipped": outcome.skipped,
            }))?
        );
        return Ok(());
    }

    println!(
        "✓ scip ingest · {repo}: {}/{} document(s) accepted, {} occurrence(s) written",
        outcome.docs_accepted, outcome.docs_received, outcome.occurrences_written
    );
    if outcome.cli_skipped_unreadable > 0 {
        println!(
            "  {} document(s) skipped locally (unreadable on this machine)",
            outcome.cli_skipped_unreadable
        );
    }
    if !outcome.skipped.is_empty() {
        println!(
            "  {} document(s) skipped by the daemon:",
            outcome.skipped.len()
        );
        for s in &outcome.skipped {
            println!(
                "    {} — {}",
                s["path"].as_str().unwrap_or("?"),
                s["reason"].as_str().unwrap_or("?"),
            );
        }
    }
    Ok(())
}

// --- PRR-N12 (N2): `kb-code scip run` --------------------------------------

/// Spawn `command` (argv array — `command[0]` is the executable, the rest
/// its arguments; NEVER a shell string) with `cwd` as its working directory,
/// and wait up to `timeout` for it to exit. A manual poll-then-kill loop
/// (rather than a blocking `Child::wait()`) so a runaway indexer can't hang
/// this call forever — `Cmd::Scip::Run`'s own `--timeout-secs` safety valve.
/// Runs on the calling (blocking) thread; callers invoke this inside
/// `tokio::task::spawn_blocking`.
fn run_indexer_blocking(
    command: &[String],
    cwd: &Path,
    timeout: Duration,
) -> Result<std::process::ExitStatus> {
    let mut cmd = std::process::Command::new(&command[0]);
    cmd.args(&command[1..]).current_dir(cwd);
    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn {command:?} (cwd={})", cwd.display()))?;
    let start = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait().context("poll indexer subprocess")? {
            return Ok(status);
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!(
                "{command:?} timed out after {}s (cwd={})",
                timeout.as_secs(),
                cwd.display()
            );
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// `kb-code scip run [--repo NAME | --all] [--dry-run] [--timeout-secs N]`
/// — see `ScipCmd::Run`'s own doc for the full contract. Reads
/// `GET /api/repos` ONCE for every target repo's `scip` config (command/
/// output/path) — the CLI has no `kb-code.toml` of its own, so the daemon's
/// echoed config (N1's `ScipStatus`) is the only source of truth for what
/// to spawn.
async fn scip_run_cmd(
    daemon: &str,
    repo: Option<&str>,
    all: bool,
    dry_run: bool,
    timeout_secs: u64,
    json: bool,
) -> Result<()> {
    match (repo, all) {
        (None, false) => anyhow::bail!("kb-code scip run: pass --repo NAME or --all"),
        (Some(_), true) => {
            anyhow::bail!("kb-code scip run: --repo and --all are mutually exclusive")
        }
        _ => {}
    }

    let client = http_client()?;
    let body = get_json(&client, daemon, "/api/repos", &[]).await?;
    let repos = body["repos"].as_array().cloned().unwrap_or_default();

    let targets: Vec<serde_json::Value> = if let Some(name) = repo {
        let found = repos
            .into_iter()
            .find(|r| r["name"].as_str() == Some(name))
            .ok_or_else(|| anyhow::anyhow!("no such repo: {name:?}"))?;
        vec![found]
    } else {
        repos
            .into_iter()
            .filter(|r| r["scip"]["configured"].as_bool() == Some(true))
            .collect()
    };

    if targets.is_empty() {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({ "results": [] }))?
            );
        } else {
            println!("no repos with a [[scip.repos]] entry configured");
        }
        return Ok(());
    }

    let timeout = Duration::from_secs(timeout_secs);
    let mut results: Vec<serde_json::Value> = Vec::with_capacity(targets.len());
    let mut any_failed = false;

    for r in &targets {
        let name = r["name"].as_str().unwrap_or("?").to_string();
        let repo_path = r["path"].as_str().unwrap_or("").to_string();
        let scip = &r["scip"];

        if scip["configured"].as_bool() != Some(true) {
            let msg = format!("{name}: no [[scip.repos]] entry configured on the daemon");
            if !json {
                println!("✗ {msg}");
            }
            results.push(serde_json::json!({"repo": name, "ok": false, "error": msg}));
            any_failed = true;
            continue;
        }
        let command: Vec<String> = scip["command"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let output = scip["output"].as_str().unwrap_or("").to_string();
        if command.is_empty() {
            let msg = format!("{name}: [[scip.repos]] entry has an empty command");
            if !json {
                println!("✗ {msg}");
            }
            results.push(serde_json::json!({"repo": name, "ok": false, "error": msg}));
            any_failed = true;
            continue;
        }

        if dry_run {
            // V70-A8 (D20 secret hygiene) — scrub this CLI's own resolved
            // bearer token (if any) out of the echoed invocation BEFORE it
            // is ever formatted into a printed string or a `--json` field.
            // See `redact.rs`'s module doc for the exact scope of this
            // guarantee.
            let token = token::resolve_bearer_token();
            let joined = redact::scrub(&command.join(" "), token.as_deref());
            let repo_path_redacted = redact::scrub(&repo_path, token.as_deref());
            let output_redacted = redact::scrub(&output, token.as_deref());
            if !json {
                println!("{name}: (dry-run) {joined}   [cwd={repo_path_redacted}]");
            }
            results.push(serde_json::json!({
                "repo": name,
                "ok": true,
                "dry_run": true,
                "argv": command
                    .iter()
                    .map(|a| redact::scrub(a, token.as_deref()))
                    .collect::<Vec<_>>(),
                "cwd": repo_path_redacted,
                "output": output_redacted,
            }));
            continue;
        }

        let spawn_command = command.clone();
        let spawn_cwd = PathBuf::from(&repo_path);
        let run_result = tokio::task::spawn_blocking(move || {
            run_indexer_blocking(&spawn_command, &spawn_cwd, timeout)
        })
        .await
        .context("indexer subprocess task panicked");

        let status = match run_result {
            Ok(Ok(status)) => status,
            Ok(Err(e)) | Err(e) => {
                let msg = format!("{name}: {e:#}");
                if !json {
                    println!("✗ {msg}");
                }
                results.push(serde_json::json!({"repo": name, "ok": false, "error": msg}));
                any_failed = true;
                continue;
            }
        };
        if !status.success() {
            let msg = format!("{name}: indexer exited with {status}");
            if !json {
                println!("✗ {msg}");
            }
            results.push(serde_json::json!({"repo": name, "ok": false, "error": msg}));
            any_failed = true;
            continue;
        }

        // Chain into the EXISTING `scip ingest` code path (the DATA half,
        // `scip_ingest_core` — not `scip_ingest_cmd`'s printer, which would
        // emit its own JSON/text block per repo) against
        // `<repo_path>/<output>`.
        let index_path = PathBuf::from(&repo_path).join(&output);
        match scip_ingest_core(daemon, &name, &index_path, 200).await {
            Ok(outcome) => {
                if !json {
                    println!(
                        "✓ {name}: {}/{} document(s) accepted, {} occurrence(s) written",
                        outcome.docs_accepted, outcome.docs_received, outcome.occurrences_written
                    );
                }
                results.push(serde_json::json!({
                    "repo": name,
                    "ok": true,
                    "docs_received": outcome.docs_received,
                    "docs_accepted": outcome.docs_accepted,
                    "occurrences_written": outcome.occurrences_written,
                    "cli_skipped_unreadable": outcome.cli_skipped_unreadable,
                    "skipped": outcome.skipped,
                }));
            }
            Err(e) => {
                let msg = format!("{name}: indexer succeeded but ingest failed: {e:#}");
                if !json {
                    println!("✗ {msg}");
                }
                results.push(serde_json::json!({"repo": name, "ok": false, "error": msg}));
                any_failed = true;
            }
        }
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "results": results }))?
        );
    } else if !any_failed && !dry_run {
        println!(
            "✓ scip run · {}/{} repo(s) succeeded",
            targets.len(),
            targets.len()
        );
    }

    if any_failed {
        anyhow::bail!("kb-code scip run: one or more repos failed (see above)");
    }
    Ok(())
}

/// `kb-code stacks --repo R [--all] [--json]` — `GET /api/stacks`.
async fn stacks_list_cmd(daemon: &str, repo: &str, all: bool, json: bool) -> Result<()> {
    let client = http_client()?;
    let mut query: Vec<(&str, &str)> = vec![("repo", repo)];
    if all {
        query.push(("all", "true"));
    }
    let body = get_json(&client, daemon, "/api/stacks", &query).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let stacks = body["stacks"].as_array().cloned().unwrap_or_default();
    if body["truncated"].as_bool().unwrap_or(false) {
        println!("(truncated — repo exceeds the branch cap; detection saw the lex-first subset)");
    }
    if stacks.is_empty() {
        println!("no dependent-branch stacks detected");
        if !all {
            println!("(hint: pass --all to include single-layer branches based on the default)");
        }
        return Ok(());
    }
    let default = body["default_branch"].as_str().unwrap_or("?");
    println!("default: {default}");
    for (i, stack) in stacks.iter().enumerate() {
        if i > 0 {
            println!();
        }
        println!("stack:");
        for layer in stack["layers"].as_array().cloned().unwrap_or_default() {
            let branch = layer["branch"].as_str().unwrap_or("?");
            let base = layer["base"].as_str().unwrap_or("?");
            let ahead = layer["ahead"].as_u64().unwrap_or(0);
            let behind = layer["behind"].as_u64().unwrap_or(0);
            let mut marks = Vec::new();
            if layer["stale"].as_bool() == Some(true) {
                marks.push("stale");
            }
            if layer["tip_shared"].as_bool() == Some(true) {
                marks.push("tip_shared");
            }
            if layer["unresolved"].as_bool() == Some(true) {
                marks.push("unresolved");
            }
            let mark = if marks.is_empty() {
                String::new()
            } else {
                format!("  [{}]", marks.join(","))
            };
            let subject = layer["tip"]["subject"].as_str().unwrap_or("");
            let sha = layer["tip"]["sha"].as_str().unwrap_or("");
            let short: String = sha.chars().take(8).collect();
            println!(
                "  {branch:<20} base={base:<16} +{ahead:<4} -{behind:<4}  {short}  {subject}{mark}"
            );
        }
    }
    Ok(())
}

/// `kb-code stacks diff --branch B --repo R [--json]` —
/// `GET /api/stacks/layer-diff`.
async fn stacks_diff_cmd(daemon: &str, repo: &str, branch: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let body = get_json(
        &client,
        daemon,
        "/api/stacks/layer-diff",
        &[("repo", repo), ("branch", branch)],
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "layer-diff  {branch}  base={}{}  {}..{}",
        body["base"].as_str().unwrap_or("?"),
        if body["stale"].as_bool() == Some(true) {
            "  [stale]"
        } else {
            ""
        },
        body["base_tip"]
            .as_str()
            .unwrap_or("?")
            .chars()
            .take(8)
            .collect::<String>(),
        body["tip"]
            .as_str()
            .unwrap_or("?")
            .chars()
            .take(8)
            .collect::<String>(),
    );
    let files = body["files"].as_array().cloned().unwrap_or_default();
    if files.is_empty() {
        println!("  (no files)");
    } else {
        for f in &files {
            println!(
                "  {}  {}  +{} -{}",
                f["status"].as_str().unwrap_or("?"),
                f["path"].as_str().unwrap_or("?"),
                f["insertions"].as_u64().unwrap_or(0),
                f["deletions"].as_u64().unwrap_or(0),
            );
        }
    }
    if let Some(t) = body.get("totals") {
        println!(
            "  totals: files={}  +{} -{}",
            t["files"].as_u64().unwrap_or(0),
            t["insertions"].as_u64().unwrap_or(0),
            t["deletions"].as_u64().unwrap_or(0),
        );
    }
    Ok(())
}

/// The index's `metadata.project_root` (a `file://`-prefixed, percent-
/// (URI-)encoded absolute path per the SCIP spec) as a plain filesystem
/// `PathBuf` — the working-tree root every document's `relative_path` is
/// relative to. `None` when the field is empty (no metadata at all, or an
/// indexer that left it unset).
fn scip_project_root(index: &::scip::types::Index) -> Option<PathBuf> {
    let raw = index.metadata.project_root.as_str();
    if raw.is_empty() {
        return None;
    }
    let stripped = raw.strip_prefix("file://").unwrap_or(raw);
    Some(PathBuf::from(percent_decode(stripped)))
}

/// Minimal percent-decoding (`%XX` → the raw byte, `+` left AS-IS — this is
/// a `file://` PATH, not a `application/x-www-form-urlencoded` query
/// string, so `+` must NOT become a space): just enough to correctly
/// resolve a `project_root` containing spaces/unicode (the common real-
/// world case), without pulling in a whole URL crate for one field. Any
/// `%` not followed by two valid hex digits is left untouched rather than
/// erroring — a defensive, honest degrade for a malformed/unexpected
/// encoding, not a hard failure over one path field.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `kb-code defs <SYMBOL> [--repo NAME]` — `GET /api/defs`.
async fn defs_cmd(
    daemon: &str,
    repo: Option<&str>,
    symbol: &str,
    limit: Option<usize>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut q: Vec<(&str, &str)> = vec![("symbol", symbol)];
    if let Some(r) = repo {
        q.push(("repo", r));
    }
    let limit_s = limit.map(|l| l.to_string());
    if let Some(l) = &limit_s {
        q.push(("limit", l));
    }
    let body = get_json(&client, daemon, "/api/defs", &q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let exact = body["exact"].as_bool().unwrap_or(false);
    println!(
        "defs · {symbol} ({})",
        if exact { "exact" } else { "fuzzy fallback" }
    );
    let results = body["results"].as_array().cloned().unwrap_or_default();
    if results.is_empty() {
        println!("  (no matches)");
    }
    for r in &results {
        let approx = if r["approximate"].as_bool().unwrap_or(false) {
            "~"
        } else {
            " "
        };
        let class = r["class"].as_str().unwrap_or("candidate");
        println!(
            "  {approx} {class:<10} {:<10} {:<20} {}:{}-{}",
            r["repo"].as_str().unwrap_or("?"),
            r["kind"].as_str().unwrap_or("?"),
            r["path"].as_str().unwrap_or("?"),
            r["line_start"].as_u64().unwrap_or(0),
            r["line_end"].as_u64().unwrap_or(0),
        );
    }
    Ok(())
}

/// `kb-code xrefs <SYMBOL> --repo NAME` — `GET /api/xrefs`.
async fn xrefs_cmd(
    daemon: &str,
    repo: &str,
    symbol: &str,
    limit: Option<usize>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut q: Vec<(&str, &str)> = vec![("repo", repo), ("symbol", symbol)];
    let limit_s = limit.map(|l| l.to_string());
    if let Some(l) = &limit_s {
        q.push(("limit", l));
    }
    let body = get_json(&client, daemon, "/api/xrefs", &q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "refs · {symbol} (approximate — {})",
        body["note"].as_str().unwrap_or("")
    );
    let results = body["results"].as_array().cloned().unwrap_or_default();
    if results.is_empty() {
        println!("  (no matches)");
    }
    for r in &results {
        let class = r["class"].as_str().unwrap_or("candidate");
        println!(
            "  {class:<10} {}:{}: {}",
            r["path"].as_str().unwrap_or("?"),
            r["line"].as_u64().unwrap_or(0),
            truncate(r["text"].as_str().unwrap_or(""), 100),
        );
    }
    if body["truncated"].as_bool().unwrap_or(false) {
        println!("  (truncated — more results exist)");
    }
    if body["time_budget_exceeded"].as_bool().unwrap_or(false) {
        println!("  (time budget exceeded — some files may not have been searched)");
    }
    Ok(())
}

/// `kb-code similar <PATH>:<START>-<END> --repo NAME` — `GET /api/similar`.
#[allow(clippy::too_many_arguments)]
async fn similar_cmd(
    daemon: &str,
    repo: &str,
    path: &str,
    start: u32,
    end: u32,
    limit: Option<u32>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let start_s = start.to_string();
    let end_s = end.to_string();
    let mut q: Vec<(&str, &str)> = vec![
        ("repo", repo),
        ("path", path),
        ("start", &start_s),
        ("end", &end_s),
    ];
    let limit_s = limit.map(|l| l.to_string());
    if let Some(l) = &limit_s {
        q.push(("limit", l));
    }
    let body = get_json(&client, daemon, "/api/similar", &q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!("similar · {path}:{start}-{end}");
    let hits = body["hits"].as_array().cloned().unwrap_or_default();
    if hits.is_empty() {
        println!("  (no hits)");
    }
    for h in &hits {
        println!(
            "  {:.3}  {}:{}-{}",
            h["score"].as_f64().unwrap_or(0.0),
            h["path"].as_str().unwrap_or("?"),
            h["span_start"].as_u64().unwrap_or(0),
            h["span_end"].as_u64().unwrap_or(0),
        );
    }
    Ok(())
}

/// `kb-code impact <PATH> --repo NAME` — `GET /api/impact` (co-change).
async fn impact_cmd(
    daemon: &str,
    repo: &str,
    path: &str,
    limit: Option<usize>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut q: Vec<(&str, &str)> = vec![("repo", repo), ("path", path)];
    let limit_s = limit.map(|l| l.to_string());
    if let Some(l) = &limit_s {
        q.push(("limit", l));
    }
    let body = get_json(&client, daemon, "/api/impact", &q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "impact · {path} ({} commit(s) walked{}, approximate)",
        body["commits_walked"].as_u64().unwrap_or(0),
        if body["truncated"].as_bool().unwrap_or(false) {
            ", truncated"
        } else {
            ""
        },
    );
    let results = body["results"].as_array().cloned().unwrap_or_default();
    if results.is_empty() {
        println!("  (no neighbors found)");
    }
    for r in &results {
        println!(
            "  {:<40} co_changes={:<4} mentions={}",
            r["path"].as_str().unwrap_or("?"),
            r["co_changes"].as_u64().unwrap_or(0),
            r["mentions"].as_u64().unwrap_or(0),
        );
    }
    Ok(())
}

/// `kb-code impact <PATH>:<LINE>:<COL>` — V3.1-H2 compositional analysis.
#[allow(clippy::too_many_arguments)]
async fn impact_analysis_cmd(
    daemon: &str,
    repo: &str,
    path: &str,
    line: u32,
    col: u32,
    limit: Option<usize>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let line_s = line.to_string();
    let col_s = col.to_string();
    let mut q: Vec<(&str, &str)> = vec![
        ("repo", repo),
        ("path", path),
        ("line", &line_s),
        ("col", &col_s),
    ];
    let limit_s = limit.map(|l| l.to_string());
    if let Some(l) = &limit_s {
        q.push(("limit", l));
    }
    let body = get_json(&client, daemon, "/api/impact/analysis", &q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let name = body["symbol"]["name"].as_str().unwrap_or("?");
    let kind = body["symbol"]["kind"].as_str().unwrap_or("-");
    println!("impact analysis · {name} ({kind})  {path}:{line}:{col}");
    for bucket in [
        "direct_exact",
        "direct_likely",
        "transitive",
        "imports",
        "tests",
    ] {
        let rows = body[bucket].as_array().cloned().unwrap_or_default();
        if rows.is_empty() {
            continue;
        }
        println!("  [{bucket}] ({})", rows.len());
        for r in &rows {
            let p = r["path"].as_str().unwrap_or("?");
            let ln = r["line"].as_u64().unwrap_or(0);
            let c = r["col"].as_u64().unwrap_or(0);
            let class = r["class"].as_str().unwrap_or("?");
            let k = r["kind"].as_str().unwrap_or("?");
            let depth = r["depth"]
                .as_u64()
                .map(|d| format!(" d={d}"))
                .unwrap_or_default();
            let n = r["name"].as_str().unwrap_or("");
            let n_s = if n.is_empty() {
                String::new()
            } else {
                format!(" · {n}")
            };
            println!("    {class:<10} {k:<12} {p}:{ln}:{c}{depth}{n_s}");
        }
        // Provenance summary line for direct buckets.
        if matches!(bucket, "direct_exact" | "direct_likely") {
            if let Some(prov) = body["provenance"][bucket].as_object() {
                println!(
                    "    provenance: rows_with_session={} distinct_sessions={}",
                    prov.get("rows_with_session")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                    prov.get("distinct_sessions")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0),
                );
            } else {
                println!("    provenance: (none)");
            }
        }
    }
    if let Some(note) = body["note"].as_str() {
        println!();
        println!("{note}");
    }
    Ok(())
}

/// `kb-code lenses <PATH> --repo NAME` — V3.1-H2 Code Vision lenses.
async fn lenses_cmd(
    daemon: &str,
    repo: &str,
    path: &str,
    rev: Option<&str>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut q: Vec<(&str, &str)> = vec![("repo", repo), ("path", path)];
    if let Some(r) = rev {
        q.push(("ref", r));
    }
    let body = get_json(&client, daemon, "/api/lenses", &q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let total = body["total"].as_u64().unwrap_or(0);
    let trunc = body["truncated"].as_bool().unwrap_or(false);
    println!(
        "lenses · {path} ({} declaration(s){})",
        total,
        if trunc { ", truncated" } else { "" }
    );
    println!(
        "  {:<6} {:<8} {:<20} {:>6} {:>6} {:>6} {:>6}  author",
        "line", "kind", "name", "exact", "likely", "cand", "impl"
    );
    for d in body["declarations"].as_array().into_iter().flatten() {
        let line = d["line"].as_u64().unwrap_or(0);
        let kind = d["kind"].as_str().unwrap_or("?");
        let name = d["name"].as_str().unwrap_or("?");
        let ex = d["usages"]["exact"].as_u64().unwrap_or(0);
        let lk = d["usages"]["likely"].as_u64().unwrap_or(0);
        let ca = d["usages"]["candidate"].as_u64().unwrap_or(0);
        let im = d["implementors"]
            .as_u64()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".into());
        let author = d["author"].as_object().map_or_else(
            || "-".into(),
            |a| {
                format!(
                    "{}:{}",
                    a.get("kind").and_then(|v| v.as_str()).unwrap_or("?"),
                    a.get("label").and_then(|v| v.as_str()).unwrap_or("?")
                )
            },
        );
        println!("  {line:<6} {kind:<8} {name:<20} {ex:>6} {lk:>6} {ca:>6} {im:>6}  {author}");
    }
    Ok(())
}

// --- resolve (B3 — position-based lookup, the peek panel's PRIMARY path) ---

/// `kb-code resolve <PATH>:<LINE>:<COL> --repo <NAME>` — `GET /api/resolve`
/// (B3). Human view: an `ident (role)` header, one line per candidate
/// (`precision  path:line  kind  container  — signature`, a cross-repo
/// candidate's `path:line` prefixed `repo/`), then the server's own honesty
/// `note` as a trailing line — this crate has no ANSI/color convention to
/// literally "dim" it (see `provenance_report_cmd`/`why_cmd` for the same
/// plain-trailing-line precedent), so it prints as its own final line.
#[allow(clippy::too_many_arguments)]
async fn resolve_cmd(
    daemon: &str,
    repo: &str,
    path: &str,
    line: u32,
    col: u32,
    rev: Option<&str>,
    limit: Option<usize>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let line_s = line.to_string();
    let col_s = col.to_string();
    let mut q: Vec<(&str, &str)> = vec![
        ("repo", repo),
        ("path", path),
        ("line", &line_s),
        ("col", &col_s),
    ];
    if let Some(r) = rev {
        q.push(("ref", r));
    }
    let limit_s = limit.map(|l| l.to_string());
    if let Some(l) = &limit_s {
        q.push(("limit", l));
    }
    let body = get_json(&client, daemon, "/api/resolve", &q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    let ident = body["ident"].as_str().unwrap_or("?");
    let role = body["role"].as_str().unwrap_or("-");
    println!("resolve · {ident} ({role})");
    let candidates = body["candidates"].as_array().cloned().unwrap_or_default();
    if candidates.is_empty() {
        println!("  (no candidates)");
    }
    for c in &candidates {
        // V3.G1: `class precision path:line` (e.g. `exact locals src/foo.rs:12`).
        let class = c["class"].as_str().unwrap_or("?");
        let precision = c["precision"].as_str().unwrap_or("?");
        let c_repo = c["repo"].as_str().unwrap_or("?");
        let c_path = c["path"].as_str().unwrap_or("?");
        let c_line = c["line"].as_u64().unwrap_or(0);
        let kind = c["kind"].as_str().unwrap_or("-");
        let container = c["container"].as_str().unwrap_or("-");
        let signature = c["signature"].as_str().unwrap_or("");
        let loc = if c_repo == repo {
            format!("{c_path}:{c_line}")
        } else {
            format!("{c_repo}/{c_path}:{c_line}")
        };
        println!(
            "  {class:<10} {precision:<14} {loc:<40} {kind:<10} {container:<16} — {signature}"
        );
    }
    if let Some(note) = body["note"].as_str() {
        println!();
        println!("{note}");
    }
    Ok(())
}

/// `kb-code usages <PATH>:<LINE>:<COL> --repo NAME` — V3.G2 classified usages.
#[allow(clippy::too_many_arguments)]
/// The route + query `usages_cmd` sends, as a pure function.
///
/// Split out on purpose: `usages_verb_sends_every_flag_it_declares` (in
/// this file's `tests` module) cross-joins THIS against the clap tree, so
/// a flag that is declared and then never sent — the v7.0 defect class,
/// which cost `review impact` its required param — fails the build instead
/// of silently doing nothing.
fn usages_request(
    repo: &str,
    path: &str,
    line: u32,
    col: u32,
    rev: Option<&str>,
    limit: Option<usize>,
    v2: bool,
) -> (&'static str, Vec<(&'static str, String)>) {
    let mut q: Vec<(&'static str, String)> = vec![
        ("repo", repo.to_string()),
        ("path", path.to_string()),
        ("line", line.to_string()),
        ("col", col.to_string()),
    ];
    if let Some(r) = rev {
        q.push(("ref", r.to_string()));
    }
    if let Some(l) = limit {
        q.push(("limit", l.to_string()));
    }
    let route = if v2 { "/api/usages/2" } else { "/api/usages" };
    (route, q)
}

#[allow(clippy::too_many_arguments)]
async fn usages_cmd(
    daemon: &str,
    repo: &str,
    path: &str,
    line: u32,
    col: u32,
    rev: Option<&str>,
    limit: Option<usize>,
    json: bool,
    v2: bool,
) -> Result<()> {
    let client = http_client()?;
    let (route, owned) = usages_request(repo, path, line, col, rev, limit, v2);
    let q: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let body = get_json(&client, daemon, route, &q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if v2 {
        return print_usages_v2(&body);
    }

    let name = body["symbol"]["name"].as_str().unwrap_or("?");
    let kind = body["symbol"]["kind"].as_str().unwrap_or("-");
    let cdef = body["class_of_definition"].as_str().unwrap_or("?");
    println!("usages · {name} ({kind})  def_class={cdef}");
    for group in ["exact", "likely", "candidate"] {
        let rows = body[group].as_array().cloned().unwrap_or_default();
        if rows.is_empty() {
            continue;
        }
        println!("  [{group}] ({})", rows.len());
        for r in &rows {
            let p = r["path"].as_str().unwrap_or("?");
            let ln = r["line"].as_u64().unwrap_or(0);
            let c = r["col"].as_u64().unwrap_or(0);
            let k = r["kind"].as_str().unwrap_or("?");
            let access = r["access"].as_str().unwrap_or("-");
            let ctx = r["context"].as_str().unwrap_or("");
            println!("    {k:<4} {access:<5} {p}:{ln}:{c}  {ctx}");
        }
    }
    if body["truncated"].as_bool().unwrap_or(false) {
        println!(
            "  (truncated — totals exact={} likely={} candidate={})",
            body["total_exact"].as_u64().unwrap_or(0),
            body["total_likely"].as_u64().unwrap_or(0),
            body["total_candidate"].as_u64().unwrap_or(0),
        );
    }
    Ok(())
}

/// Human rendering of the `usages/2` body (V71-E1). Every number printed
/// here is the SERVER's: the group counts come from `totals`, the cap line
/// from `capped` — the CLI never derives a total from the rows it happens
/// to have been handed, which is exactly how a silent cap reads as a
/// complete answer.
fn print_usages_v2(body: &serde_json::Value) -> Result<()> {
    let name = body["symbol"]["name"].as_str().unwrap_or("?");
    let kind = body["symbol"]["kind"].as_str().unwrap_or("-");
    let cdef = body["class_of_definition"].as_str().unwrap_or("?");
    let all = body["totals"]["all"].as_u64().unwrap_or(0);
    println!("usages/2 · {name} ({kind})  def_class={cdef}  total={all}");
    if let Some(rs) = body.get("ruby_strict").filter(|v| !v.is_null()) {
        let verdict = rs["verdict"].as_str().unwrap_or("?");
        let exact = rs["exact"].as_bool().unwrap_or(false);
        let hier: Vec<&str> = rs["hierarchy"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        println!(
            "  ruby-strict: {} ({verdict}){}",
            if exact { "exact" } else { "likely" },
            if hier.is_empty() {
                String::new()
            } else {
                format!("  hierarchy: {}", hier.join(" < "))
            }
        );
    }
    for group in ["exact", "likely", "candidate"] {
        let rows = body[group].as_array().cloned().unwrap_or_default();
        let total = body["totals"][group].as_u64().unwrap_or(0);
        if total == 0 {
            continue;
        }
        println!("  [{group}] ({} of {total})", rows.len());
        for r in &rows {
            let p = r["path"].as_str().unwrap_or("?");
            let ln = r["line"].as_u64().unwrap_or(0);
            let c = r["col"].as_u64().unwrap_or(0);
            let k = r["kind"].as_str().unwrap_or("?");
            let prec = r["precision"].as_str().unwrap_or("?");
            let enc = r["enclosing"]["name"].as_str().unwrap_or("");
            let roles: Vec<&str> = r["role_names"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            let ctx = r["context"].as_str().unwrap_or("");
            println!(
                "    {k:<14} {prec:<20} {p}:{ln}:{c}{}{}  {ctx}",
                if enc.is_empty() {
                    String::new()
                } else {
                    format!("  in {enc}")
                },
                if roles.is_empty() {
                    String::new()
                } else {
                    format!("  [{}]", roles.join(","))
                }
            );
        }
    }
    if let Some(kinds) = body["kind_totals"].as_object() {
        if !kinds.is_empty() {
            let line: Vec<String> = kinds
                .iter()
                .map(|(k, v)| format!("{k} {}", v.as_u64().unwrap_or(0)))
                .collect();
            println!("  kinds: {}", line.join(" · "));
        }
    }
    for cap in body["capped"].as_array().cloned().unwrap_or_default() {
        println!(
            "  capped: {} showing {} of {} ({})",
            cap["group"].as_str().unwrap_or("?"),
            cap["returned"].as_u64().unwrap_or(0),
            cap["total"].as_u64().unwrap_or(0),
            cap["reason"].as_str().unwrap_or("?"),
        );
    }
    Ok(())
}

/// `kb-code hover <PATH>:<LINE>:<COL> --repo NAME` — PRR-N5.
async fn hover_cmd(
    daemon: &str,
    repo: &str,
    path: &str,
    line: u32,
    col: u32,
    rev: Option<&str>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let line_s = line.to_string();
    let col_s = col.to_string();
    let mut q: Vec<(&str, &str)> = vec![
        ("repo", repo),
        ("path", path),
        ("line", &line_s),
        ("col", &col_s),
    ];
    if let Some(r) = rev {
        q.push(("ref", r));
    }
    let body = get_json(&client, daemon, "/api/hover", &q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    let precision = body["precision"].as_str().unwrap_or("-");
    let trust = body["trust"].as_str().unwrap_or("-");
    println!("hover · {path}:{line}:{col}  [{trust} {precision}]");
    if let Some(sym) = body.get("symbol").filter(|v| !v.is_null()) {
        let kind = sym["kind"].as_str().unwrap_or("?");
        let name = sym["name"].as_str().unwrap_or("?");
        let container = sym["container"].as_str().unwrap_or("-");
        let signature = sym["signature"].as_str().unwrap_or("");
        println!("  symbol   {kind:<10} {container}::{name} — {signature}");
        if let Some(doc) = sym["doc"].as_str() {
            println!("  doc      {doc}");
        }
    }
    if let Some(def) = body.get("defsite").filter(|v| !v.is_null()) {
        let p = def["path"].as_str().unwrap_or("?");
        let l = def["line"].as_u64().unwrap_or(0);
        println!("  defsite  {p}:{l}");
    }
    if let Some(fw) = body.get("framework").filter(|v| !v.is_null()) {
        let kind = fw["kind"].as_str().unwrap_or("?");
        let dst_kind = fw["dst_kind"].as_str().unwrap_or("-");
        let dst_path = fw["dst_path"].as_str().unwrap_or("-");
        let ftrust = fw["trust"].as_str().unwrap_or("?");
        println!("  framework {kind} → {dst_kind} {dst_path} [{ftrust}]");
    }
    Ok(())
}

// --- PRR-L2 (append-only fn; delimited from concurrent edits elsewhere in
// this file — see the Cmd::Diagnostics variant's own doc).

/// `kb-code diagnostics PATH --repo R [--json]` — PRR-L2:
/// `GET /api/diagnostics`. `diagnostics: null` (no provider configured, or
/// the round trip failed) is rendered distinctly from an empty, clean
/// result — never conflated (design-addendum-2.md §D).
async fn diagnostics_cmd(daemon: &str, repo: &str, path: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let q: Vec<(&str, &str)> = vec![("repo", repo), ("path", path)];
    let body = get_json(&client, daemon, "/api/diagnostics", &q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    let Some(diags) = body.get("diagnostics").filter(|v| !v.is_null()) else {
        let reason = body["unavailable_reason"].as_str().unwrap_or("unavailable");
        println!("diagnostics · {path}  unavailable ({reason})");
        return Ok(());
    };
    let diags = diags.as_array().cloned().unwrap_or_default();
    let provider = body["provider"].as_str().unwrap_or("?");
    println!("diagnostics · {path}  [{provider}]  ({})", diags.len());
    for d in &diags {
        let line = d["line"].as_u64().unwrap_or(0);
        let col = d["col"].as_u64().unwrap_or(0);
        let severity = d["severity"]
            .as_i64()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "-".to_string());
        let message = d["message"].as_str().unwrap_or("");
        println!("  {line}:{col}  sev={severity}  {message}");
    }
    Ok(())
}

// --- end PRR-L2 append -------------------------------------------------

// --- S2-B1 (design-s2.md § S2-C; append-only fns, delimited from
// concurrent edits elsewhere in this file).

/// `kb-code code-actions PATH:LINE[:COL] --repo R [...]` — S2-C:
/// `POST /api/code-actions`. `unavailable_reason`'s honest degrade (never
/// an empty list standing in for "nothing configured") mirrors
/// `diagnostics_cmd` above. `--suggest N` hands off to
/// [`code_actions_suggest_cmd`] once the list is fetched.
#[allow(clippy::too_many_arguments)]
async fn code_actions_cmd(
    daemon: &str,
    repo: &str,
    path: &str,
    start_line: u32,
    start_col: u32,
    end: Option<(u32, u32)>,
    kinds: Option<&[String]>,
    suggest: Option<usize>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut body = serde_json::json!({
        "repo": repo,
        "path": path,
        "start_line": start_line,
        "start_col": start_col,
    });
    if let Some((end_line, end_col)) = end {
        body["end_line"] = serde_json::json!(end_line);
        body["end_col"] = serde_json::json!(end_col);
    }
    if let Some(k) = kinds {
        if !k.is_empty() {
            body["kinds"] = serde_json::json!(k);
        }
    }
    let (status, resp) = post_json_raw(&client, daemon, "/api/code-actions", &body).await?;
    if !status.is_success() {
        return Err(annotation_api_error("code actions", status, &resp));
    }

    if !resp["available"].as_bool().unwrap_or(false) {
        if json {
            println!("{}", serde_json::to_string_pretty(&resp)?);
            return Ok(());
        }
        let reason = resp["reason"].as_str().unwrap_or("unavailable");
        println!("code-actions · {path}:{start_line}:{start_col}  unavailable ({reason})");
        return Ok(());
    }

    let actions: Vec<CodeAction> =
        serde_json::from_value(resp["actions"].clone()).context("parse code-actions response")?;
    let provider = resp["provider"].as_str().unwrap_or("?").to_string();

    if let Some(n) = suggest {
        return code_actions_suggest_cmd(&client, daemon, repo, &actions, &provider, n, json).await;
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }

    println!(
        "code-actions · {path}:{start_line}:{start_col}  [{provider}]  ({})",
        actions.len()
    );
    for (i, a) in actions.iter().enumerate() {
        let pref = if a.is_preferred { "  *preferred*" } else { "" };
        let files: Vec<&str> = a.edits.iter().map(|e| e.path.as_str()).collect();
        let kind = if a.kind.is_empty() {
            "-"
        } else {
            a.kind.as_str()
        };
        println!(
            "  [{}] {} ({kind}){pref} → {}",
            i + 1,
            a.title,
            files.join(", ")
        );
    }
    let dropped_co = resp["dropped"]["command_only"].as_u64().unwrap_or(0);
    let dropped_un = resp["dropped"]["unsupported"].as_u64().unwrap_or(0);
    if dropped_co > 0 || dropped_un > 0 {
        println!("  dropped: command_only={dropped_co} unsupported={dropped_un}");
    }
    Ok(())
}

/// `--suggest N` — converts the 1-based `actions[N-1]` into one
/// annotation+suggestion op per (file, edit) via `POST
/// /api/annotations/batch` (see [`code_action_to_batch_ops`]'s doc).
/// Fetches every distinct file the action touches fresh (`GET /api/file`)
/// so the splice math has real, current line content to work against —
/// the daemon's own blob-freshness re-check (`crate::lip`) already
/// guarded the ACTION itself; this is the SAME "never trust a stale read"
/// posture applied to the splice inputs.
#[allow(clippy::too_many_arguments)]
async fn code_actions_suggest_cmd(
    client: &reqwest::Client,
    daemon: &str,
    repo: &str,
    actions: &[CodeAction],
    provider: &str,
    n: usize,
    json: bool,
) -> Result<()> {
    if n == 0 || n > actions.len() {
        anyhow::bail!(
            "no such action index {n} — there are {} action(s) (1..={})",
            actions.len(),
            actions.len()
        );
    }
    let action = &actions[n - 1];
    let mut paths: Vec<&str> = action
        .edits
        .iter()
        .map(|e| e.path.as_str())
        .collect::<Vec<_>>();
    paths.sort_unstable();
    paths.dedup();

    let mut file_contents = std::collections::HashMap::new();
    for p in paths {
        let body = get_json(client, daemon, "/api/file", &[("repo", repo), ("path", p)]).await?;
        let bytes = decode_file_content(&body)?;
        let text = String::from_utf8(bytes).with_context(|| {
            format!("file {p:?} is not valid UTF-8; cannot compute a suggestion")
        })?;
        file_contents.insert(p.to_string(), text);
    }

    let ops = code_action_to_batch_ops(action, provider, &file_contents)?;
    if ops.is_empty() {
        anyhow::bail!("action {n} has no usable edits to convert into a suggestion");
    }

    let (status, resp) = post_json_raw(
        client,
        daemon,
        "/api/annotations/batch",
        &serde_json::json!({ "repo": repo, "ops": ops }),
    )
    .await?;
    if !status.is_success() {
        return Err(annotation_api_error("code-actions suggest", status, &resp));
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
        return Ok(());
    }
    let created: Vec<String> = resp["created_ids"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    println!("created {} suggestion(s):", created.len());
    for id in &created {
        println!("  {id}");
    }
    if !created.is_empty() {
        // Chunk the HINT text at the 50-id apply-batch cap — never the
        // creation itself (design-s2.md § S2-C).
        for chunk in created.chunks(50) {
            println!("  kb-code suggest apply-batch {}", chunk.join(" "));
        }
    }
    Ok(())
}

// --- end S2-B1 append -------------------------------------------------

/// `kb-code framework <PATH> --repo NAME [--kind K]` — PRR-N5.
async fn framework_cmd(
    daemon: &str,
    repo: &str,
    path: &str,
    kind: Option<&str>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut q: Vec<(&str, &str)> = vec![("repo", repo), ("path", path)];
    if let Some(k) = kind {
        q.push(("kind", k));
    }
    let body = get_json(&client, daemon, "/api/framework/edges", &q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    let edges = body["edges"].as_array().cloned().unwrap_or_default();
    println!("framework edges · {path}  ({})", edges.len());
    for e in &edges {
        let dir = e["direction"].as_str().unwrap_or("?");
        let kind = e["kind"].as_str().unwrap_or("?");
        let trust = e["trust"].as_str().unwrap_or("?");
        let other = if dir == "src" {
            format!(
                "→ {} {}",
                e["dst_kind"].as_str().unwrap_or("-"),
                e["dst_path"].as_str().unwrap_or("-")
            )
        } else {
            format!(
                "← {}:{}",
                e["src_path"].as_str().unwrap_or("-"),
                e["src_line"].as_u64().unwrap_or(0)
            )
        };
        println!("  {dir:<4} {kind:<20} {trust:<10} {other}");
    }
    Ok(())
}

/// `kb-code resolve-symbol <SYM> --repo NAME` — PRR-N5. Never errors on a
/// miss (the server's own contract — see `kb_code_server::symbol_addr`'s
/// module doc) — prints a `found: false` line instead.
async fn resolve_symbol_cmd(daemon: &str, repo: &str, sym: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let q: Vec<(&str, &str)> = vec![("repo", repo), ("sym", sym)];
    let body = get_json(&client, daemon, "/api/resolve-symbol", &q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    if !body["found"].as_bool().unwrap_or(false) {
        let reason = body["reason"].as_str().unwrap_or("not found");
        println!("resolve-symbol · {sym}  NOT FOUND — {reason}");
        return Ok(());
    }
    let via = body["via"].as_str().unwrap_or("?");
    let path = body["path"].as_str().unwrap_or("?");
    let line = body["line"].as_u64().unwrap_or(0);
    let kind = body["kind"].as_str().unwrap_or("-");
    println!("resolve-symbol · {sym}  [{via}] {kind} {path}:{line}");
    Ok(())
}

/// `kb-code callees <PATH>:<LINE>:<COL> --repo NAME` — V3.1-H1.
async fn callees_cmd(
    daemon: &str,
    repo: &str,
    path: &str,
    line: u32,
    col: u32,
    rev: Option<&str>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let line_s = line.to_string();
    let col_s = col.to_string();
    let mut q: Vec<(&str, &str)> = vec![
        ("repo", repo),
        ("path", path),
        ("line", &line_s),
        ("col", &col_s),
    ];
    if let Some(r) = rev {
        q.push(("ref", r));
    }
    let body = get_json(&client, daemon, "/api/hierarchy/callees", &q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let fname = body["function"]["name"].as_str().unwrap_or("?");
    let fkind = body["function"]["kind"].as_str().unwrap_or("?");
    println!("callees · {fname} ({fkind})  {path}:{line}");
    println!(
        "  {:<10} {:<8} {:<6} {:<24} target",
        "class", "line", "args", "name"
    );
    for c in body["callees"].as_array().into_iter().flatten() {
        let class = c["class"].as_str().unwrap_or("?");
        let ln = c["line"].as_u64().unwrap_or(0);
        let args = c["arg_count"]
            .as_u64()
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".into());
        let name = c["name"].as_str().unwrap_or("?");
        let qual = c["qualifier"].as_str().unwrap_or("");
        let display = if qual.is_empty() {
            name.to_string()
        } else {
            format!("{qual}.{name}")
        };
        let target = c["target"].as_object().map_or_else(
            || "-".into(),
            |t| {
                format!(
                    "{}:{} ({})",
                    t.get("path").and_then(|v| v.as_str()).unwrap_or("?"),
                    t.get("line").and_then(|v| v.as_u64()).unwrap_or(0),
                    t.get("class").and_then(|v| v.as_str()).unwrap_or("?")
                )
            },
        );
        println!("  {class:<10} {ln:<8} {args:<6} {display:<24} {target}");
    }
    Ok(())
}

/// `kb-code callers <PATH>:<LINE>:<COL> --repo NAME` — V3.1-H1.
async fn callers_cmd(
    daemon: &str,
    repo: &str,
    path: &str,
    line: u32,
    col: u32,
    rev: Option<&str>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let line_s = line.to_string();
    let col_s = col.to_string();
    let mut q: Vec<(&str, &str)> = vec![
        ("repo", repo),
        ("path", path),
        ("line", &line_s),
        ("col", &col_s),
    ];
    if let Some(r) = rev {
        q.push(("ref", r));
    }
    let body = get_json(&client, daemon, "/api/hierarchy/callers", &q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let fname = body["function"]["name"].as_str().unwrap_or("?");
    let fkind = body["function"]["kind"].as_str().unwrap_or("?");
    println!("callers · {fname} ({fkind})  {path}:{line}");
    println!("  {:<10} {:<40} sites", "class", "enclosing");
    for g in body["callers"].as_array().into_iter().flatten() {
        let p = g["path"].as_str().unwrap_or("?");
        let enc = g["enclosing"].as_object().map_or_else(
            || format!("{p} (top-level)"),
            |e| {
                format!(
                    "{p} · {} ({}) L{}",
                    e.get("name").and_then(|v| v.as_str()).unwrap_or("?"),
                    e.get("kind").and_then(|v| v.as_str()).unwrap_or("?"),
                    e.get("line").and_then(|v| v.as_u64()).unwrap_or(0)
                )
            },
        );
        let sites = g["sites"].as_array().cloned().unwrap_or_default();
        let best = sites
            .iter()
            .filter_map(|s| s["class"].as_str())
            .min_by_key(|c| match *c {
                "exact" => 0,
                "likely" => 1,
                _ => 2,
            })
            .unwrap_or("?");
        let site_s = sites
            .iter()
            .map(|s| {
                format!(
                    "{}:{}({})",
                    s["line"].as_u64().unwrap_or(0),
                    s["col"].as_u64().unwrap_or(0),
                    s["class"].as_str().unwrap_or("?")
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        println!("  {best:<10} {enc:<40} {site_s}");
    }
    if body["truncated"].as_bool().unwrap_or(false) {
        println!("  (truncated)");
    }
    Ok(())
}

/// `kb-code implementors <TYPE> --repo NAME` — V3.1-H1 type hierarchy.
async fn implementors_cmd(
    daemon: &str,
    repo: &str,
    name: &str,
    path: Option<&str>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut q: Vec<(&str, &str)> = vec![("repo", repo), ("name", name)];
    if let Some(p) = path {
        q.push(("path", p));
    }
    let body = get_json(&client, daemon, "/api/hierarchy/types", &q).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!("types · {name}");
    for (label, key) in [("supertypes", "supertypes"), ("subtypes", "subtypes")] {
        let rows = body[key].as_array().cloned().unwrap_or_default();
        if rows.is_empty() {
            continue;
        }
        println!("  [{label}] ({})", rows.len());
        println!(
            "    {:<10} {:<12} {:<20} {:<28} target",
            "class", "kind", "name", "via"
        );
        for r in &rows {
            let class = r["class"].as_str().unwrap_or("?");
            let kind = r["kind"].as_str().unwrap_or("?");
            let n = r["name"].as_str().unwrap_or("?");
            let via = format!(
                "{}:{}",
                r["via"]["path"].as_str().unwrap_or("?"),
                r["via"]["line"].as_u64().unwrap_or(0)
            );
            let target = r["target"].as_object().map_or_else(
                || "-".into(),
                |t| {
                    format!(
                        "{}:{}",
                        t.get("path").and_then(|v| v.as_str()).unwrap_or("?"),
                        t.get("line").and_then(|v| v.as_u64()).unwrap_or(0)
                    )
                },
            );
            println!("    {class:<10} {kind:<12} {n:<20} {via:<28} {target}");
        }
    }
    Ok(())
}

// --- `kb-code hook install|uninstall|status` --------------------------------
//
// W5.3 (kb-code-why.sh) + D4 (kb-code-annotations.sh) — both hooks' install
// surface. Deliberately NEVER writes to `~/.claude/settings.json` (or any
// project one): `install`/`uninstall` only PRINT what to merge/remove;
// `status` only checks facts it can verify independently (the manual-install
// script paths + daemon reachability), same "never silently mutate the
// user's settings" posture kb-memory's own plugin docs follow (see
// plugins/kb-memory/hooks/README.md "Install — pick one").

const HOOK_SETTINGS_SNIPPET: &str = r#"{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Edit|Write",
        "hooks": [
          { "type": "command", "command": "~/.claude/hooks/kb-code-why.sh", "timeout": 8 },
          { "type": "command", "command": "~/.claude/hooks/kb-code-annotations.sh", "timeout": 8 }
        ]
      }
    ],
    "SessionStart": [
      {
        "matcher": "startup|resume|clear",
        "hooks": [
          { "type": "command", "command": "~/.claude/hooks/kb-code-annotations.sh --session-start", "timeout": 8 }
        ]
      }
    ]
  }
}"#;

fn hook_install() {
    println!("Neither hook is ever wired into ~/.claude/settings.json automatically —");
    println!("pick one of the two paths below.");
    println!();
    println!("Mode 1: manual (settings.json)");
    println!("  mkdir -p ~/.claude/hooks");
    println!("  cp plugins/kb-code/hooks/kb-code-why.sh ~/.claude/hooks/");
    println!("  cp plugins/kb-code/hooks/kb-code-annotations.sh ~/.claude/hooks/");
    println!("  chmod +x ~/.claude/hooks/kb-code-why.sh ~/.claude/hooks/kb-code-annotations.sh");
    println!();
    println!("  Then merge this into ~/.claude/settings.json (global) or a project");
    println!("  .claude/settings.json:");
    println!();
    println!("{HOOK_SETTINGS_SNIPPET}");
    println!();
    println!("Mode 2: Claude Code plugin");
    println!("  manifest: plugins/kb-code/.claude-plugin/plugin.json");
    println!("  hooks:    plugins/kb-code/hooks/hooks.json");
    println!("  /plugin marketplace add <path-to-kb-repo>");
    println!("  /plugin install kb-code@kb-plugins");
    println!();
    println!("Check with: kb-code hook status");
}

fn hook_uninstall() {
    println!("Remove the \"PreToolUse\"/\"SessionStart\" blocks this added to");
    println!("~/.claude/settings.json (or your project's .claude/settings.json) — the");
    println!("exact blocks, if you used the manual install path:");
    println!();
    println!("{HOOK_SETTINGS_SNIPPET}");
    println!();
    println!(
        "Then (optional): rm ~/.claude/hooks/kb-code-why.sh ~/.claude/hooks/kb-code-annotations.sh"
    );
    println!();
    println!("Installed as a plugin instead? Run: /plugin uninstall kb-code@kb-plugins");
}

/// Best-effort "is `needle` (a hook script name) wired into a
/// hooks.{PreToolUse,SessionStart} array" check against one
/// `settings.json` — a plain substring scan (not a JSON schema validation:
/// a settings.json can be arbitrarily shaped, and this only needs to
/// answer "does this file mention the hook at all"). `None` when the file
/// doesn't exist or isn't readable — NOT the same as `Some(false)` (exists
/// but doesn't mention it). Shared by both `kb-code-why.sh`'s and
/// `kb-code-annotations.sh`'s own status checks below.
fn settings_mentions(path: &Path, needle: &str) -> Option<bool> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.contains(needle))
}

async fn hook_status(daemon: &str, json: bool) -> Result<()> {
    let home = std::env::var("HOME").ok();
    let global_settings = home.as_ref().map(|h| format!("{h}/.claude/settings.json"));
    let project_settings = ".claude/settings.json".to_string();

    let manual_path = home
        .as_ref()
        .map(|h| format!("{h}/.claude/hooks/kb-code-why.sh"));
    let manual_installed = manual_path
        .as_deref()
        .map(|p| Path::new(p).is_file())
        .unwrap_or(false);
    // Best-effort "wired" signal: does a settings.json we can actually
    // read mention the hook? Checked at both the global and project-local
    // conventional paths — NOT a plugin-install check (plugin state lives
    // outside this repo's knowledge; see the printed note below).
    let global_wired = global_settings
        .as_deref()
        .and_then(|p| settings_mentions(Path::new(p), "kb-code-why.sh"));
    let project_wired = settings_mentions(Path::new(&project_settings), "kb-code-why.sh");
    let wired_anywhere = global_wired == Some(true) || project_wired == Some(true);

    // D4 — the same three checks, mirrored for kb-code-annotations.sh.
    let annotations_manual_path = home
        .as_ref()
        .map(|h| format!("{h}/.claude/hooks/kb-code-annotations.sh"));
    let annotations_manual_installed = annotations_manual_path
        .as_deref()
        .map(|p| Path::new(p).is_file())
        .unwrap_or(false);
    let annotations_global_wired = global_settings
        .as_deref()
        .and_then(|p| settings_mentions(Path::new(p), "kb-code-annotations.sh"));
    let annotations_project_wired =
        settings_mentions(Path::new(&project_settings), "kb-code-annotations.sh");
    let annotations_wired_anywhere =
        annotations_global_wired == Some(true) || annotations_project_wired == Some(true);

    let client = http_client()?;
    let daemon_reachable = get_json(&client, daemon, "/api/identity", &[])
        .await
        .is_ok();

    if json {
        let body = serde_json::json!({
            "manual_hook_path": manual_path,
            "manual_hook_installed": manual_installed,
            "global_settings_mentions_hook": global_wired,
            "project_settings_mentions_hook": project_wired,
            "wired_anywhere": wired_anywhere,
            "annotations_hook_manual_path": annotations_manual_path,
            "annotations_hook_manual_installed": annotations_manual_installed,
            "global_settings_mentions_annotations_hook": annotations_global_wired,
            "project_settings_mentions_annotations_hook": annotations_project_wired,
            "annotations_hook_wired_anywhere": annotations_wired_anywhere,
            "daemon": daemon,
            "daemon_reachable": daemon_reachable,
        });
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    let fmt_wired = |w: Option<bool>| match w {
        Some(true) => "mentions the hook",
        Some(false) => "present, no mention of the hook",
        None => "not found / unreadable",
    };

    println!("-- kb-code-why.sh (PreToolUse provenance) --");
    match (&manual_path, manual_installed) {
        (Some(p), true) => println!("manual hook script: present  ({p})"),
        (Some(p), false) => println!("manual hook script: MISSING ({p})"),
        (None, _) => println!("manual hook script: unknown (no $HOME in env)"),
    }
    println!(
        "global settings:    ~/.claude/settings.json — {}",
        fmt_wired(global_wired)
    );
    println!(
        "project settings:   ./.claude/settings.json — {}",
        fmt_wired(project_wired)
    );
    println!(
        "wired: {}",
        if wired_anywhere {
            "yes"
        } else {
            "not detected"
        }
    );

    println!();
    println!("-- kb-code-annotations.sh (PreToolUse + SessionStart, flag-for-agent) --");
    match (&annotations_manual_path, annotations_manual_installed) {
        (Some(p), true) => println!("manual hook script: present  ({p})"),
        (Some(p), false) => println!("manual hook script: MISSING ({p})"),
        (None, _) => println!("manual hook script: unknown (no $HOME in env)"),
    }
    println!(
        "global settings:    ~/.claude/settings.json — {}",
        fmt_wired(annotations_global_wired)
    );
    println!(
        "project settings:   ./.claude/settings.json — {}",
        fmt_wired(annotations_project_wired)
    );
    println!(
        "wired: {}",
        if annotations_wired_anywhere {
            "yes"
        } else {
            "not detected"
        }
    );

    println!();
    println!(
        "kb-code daemon:     {} — {}",
        daemon,
        if daemon_reachable {
            "reachable"
        } else {
            "UNREACHABLE"
        }
    );
    println!();
    println!("Note: the settings.json scan is a plain substring check, not plugin-install");
    println!("state (there's no single canonical location once plugins are in play). If");
    println!("you installed via `/plugin install kb-code@kb-plugins`, check `/plugin list`.");
    Ok(())
}

// --- `kb-code bench-search` -------------------------------------------------

async fn bench_search_cmd(
    daemon: &str,
    repo: Option<&str>,
    queries_path: &Path,
    limit: Option<usize>,
    json: bool,
) -> Result<()> {
    let text = std::fs::read_to_string(queries_path)
        .with_context(|| format!("read {}", queries_path.display()))?;
    let queries = bench::parse_queries(&text)
        .map_err(|e| anyhow::anyhow!("{}: {e}", queries_path.display()))?;
    if queries.is_empty() {
        anyhow::bail!("{}: no queries", queries_path.display());
    }

    // V71-D1b — `bench-search` is explicitly meant to be run against a
    // COLD daemon (that's the whole point of the "cold daemon" exit
    // criterion), so its FIRST query is exactly the request most likely to
    // hit the cold-cache cliff `SEARCH_CLIENT_TIMEOUT` exists for.
    let client = client_builder()
        .timeout(SEARCH_CLIENT_TIMEOUT)
        .build()
        .context("build http client")?;
    let mut report = bench::BenchReport::default();
    let mut per_query: Vec<serde_json::Value> = Vec::with_capacity(queries.len());

    for q in &queries {
        let mut query_params: Vec<(&str, &str)> = vec![("q", q.query.as_str())];
        if let Some(r) = repo {
            query_params.push(("repo", r));
        }
        let limit_s = limit.map(|l| l.to_string());
        if let Some(l) = &limit_s {
            query_params.push(("limit", l));
        }
        let start = std::time::Instant::now();
        let body = get_json_warming_aware(&client, daemon, "/api/search", &query_params).await?;
        let latency_ms = start.elapsed().as_secs_f64() * 1000.0;

        let outcome = bench::score_response(&body, &q.expect_path, latency_ms);
        per_query.push(serde_json::json!({
            "query": q.query,
            "expect_path": q.expect_path,
            "expect_kind": q.expect_kind,
            "latency_ms": latency_ms,
            "lanes": outcome.lanes.iter().map(|l| serde_json::json!({
                "lane": l.lane,
                "rank": l.rank,
            })).collect::<Vec<_>>(),
        }));
        report.record(&outcome);
    }

    if json {
        let lanes: serde_json::Map<String, serde_json::Value> = report
            .lanes
            .iter()
            .map(|(lane, agg)| (lane.clone(), lane_agg_json(agg)))
            .collect();
        let out = serde_json::json!({
            "queries": report.queries,
            "lanes": lanes,
            "overall": lane_agg_json(&report.overall),
            "per_query": per_query,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!(
        "kb-code bench-search — {} quer{} from {}",
        report.queries,
        if report.queries == 1 { "y" } else { "ies" },
        queries_path.display()
    );
    println!(
        "{:<12} {:>6} {:>10} {:>10} {:>10} {:>10}",
        "lane", "n", "recall@1", "recall@5", "p50 ms", "p95 ms"
    );
    for (lane, agg) in &report.lanes {
        print_bench_row(lane, agg);
    }
    print_bench_row("overall", &report.overall);
    println!();
    println!(
        "ADR-6 governance: [semantic] enabled defaults to false — flipping that default \
         requires a RECORDED run of this bench (--json), not a vibe check."
    );
    if report.lanes.contains_key("sessions") || report.lanes.contains_key("transcripts") {
        println!(
            "note: sessions/transcripts recall is STRUCTURALLY 0% here (their hits carry no \
             `path` field for hit_rank to match) — not a retrieval regression; see bench.rs's \
             module doc and this query file's header."
        );
    }
    Ok(())
}

fn lane_agg_json(agg: &bench::LaneAgg) -> serde_json::Value {
    serde_json::json!({
        "n": agg.total,
        "hit_at_1": agg.hit_at_1,
        "hit_at_5": agg.hit_at_5,
        "recall_at_1": agg.recall_at_1(),
        "recall_at_5": agg.recall_at_5(),
        "p50_ms": agg.p50_ms(),
        "p95_ms": agg.p95_ms(),
    })
}

fn print_bench_row(lane: &str, agg: &bench::LaneAgg) {
    println!(
        "{:<12} {:>6} {:>9.1}% {:>9.1}% {:>10.1} {:>10.1}",
        lane,
        agg.total,
        agg.recall_at_1() * 100.0,
        agg.recall_at_5() * 100.0,
        agg.p50_ms(),
        agg.p95_ms(),
    );
}

// =========================================================================
// V70-A10 — `kb-code workspace …` ("Workspaces v0", kb-code v7 "The
// Continuum"). Rides the SAME `GET`/`POST`/`PATCH`/`DELETE /api/sets[...]`
// wire surface `kb-code set` uses (`kind=workspace`,
// `kb_code_server::reading_sets`'s module doc "Workspaces" section) plus
// `POST`/`GET /api/annotations` (`set_id`, `routes::create_annotation`'s
// doc). Deliberately its OWN small family of near-duplicate helpers
// (`resolve_workspace_id` vs. `resolve_set_id`, `print_workspace_row` vs.
// the plain-set printers) rather than widening the `set` family's own
// functions with a `kind` parameter everywhere — a small duplicated helper
// stays cheaper to read than a shared one whose parameter list keeps
// growing (same posture `reading_sets::repo_relative`'s own doc documents
// for its `sessiondiff` sibling).
// =========================================================================

/// `kb-code workspace …` — see the section banner above. `NAME-OR-ID`
/// resolves against `GET /api/sets?repo=&kind=workspace` (exact name match
/// first, else a unique id PREFIX match — same ladder `SetCmd`'s own doc
/// documents, via [`resolve_workspace_id`]).
#[derive(Subcommand, Debug)]
enum WorkspaceCmd {
    /// `kb-code workspace list --repo R [--group ref] [--json]` —
    /// `GET /api/sets?repo=&kind=workspace[&group=ref]`.
    List {
        #[arg(long)]
        repo: String,
        /// Group the listing by `ref` label (`{groups: [{ref, workspaces:
        /// [...]}]}` on the wire).
        #[arg(long)]
        group: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code workspace show <NAME-OR-ID> --repo R` — `GET /api/sets/{id}`.
    Show {
        name_or_id: String,
        #[arg(long)]
        repo: String,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code workspace save --repo R --name N [-d DESC]
    /// [--description-file FILE] [--ref REF] [--desk-json JSON|-]
    /// [--file PATH[:LINE|START-END]]...` — `POST /api/sets` with
    /// `kind: "workspace"`. `--file` is repeatable (mirrors `kb-code set
    /// create --span`'s own grammar — [`parse_span_arg`]).
    Save {
        #[arg(long)]
        repo: String,
        #[arg(long)]
        name: String,
        #[arg(short = 'd', long = "description")]
        description: Option<String>,
        /// Read `description_md` from a file (Markdown, unbounded local
        /// size — the daemon still enforces its own byte cap).
        #[arg(long = "description-file")]
        description_file: Option<PathBuf>,
        #[arg(long = "ref")]
        git_ref: Option<String>,
        /// A literal JSON value, or `-` to read it from stdin (mirrors
        /// `review findings import --stdin`'s own convention).
        #[arg(long = "desk-json")]
        desk_json: Option<String>,
        #[arg(long = "file")]
        files: Vec<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code workspace open <NAME-OR-ID> --repo R [--print-url]` —
    /// prints the SPA URL that restores this workspace. Never launches a
    /// browser — see [`workspace_open_cmd`]'s doc.
    Open {
        name_or_id: String,
        #[arg(long)]
        repo: String,
        /// Accepted for the verb's documented grammar; the URL prints
        /// regardless — there is nothing else this verb does.
        #[arg(long)]
        print_url: bool,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
    /// `kb-code workspace note add …` — see [`WorkspaceNoteCmd`].
    Note {
        #[command(subcommand)]
        cmd: WorkspaceNoteCmd,
    },
    /// `kb-code workspace export <NAME-OR-ID> --repo R --md` — a Markdown
    /// write-up (title, ref, description, entries as `path:line` links,
    /// notes threaded) printed to stdout — see
    /// [`workspace_export_cmd`]'s doc.
    Export {
        name_or_id: String,
        #[arg(long)]
        repo: String,
        /// Accepted for the verb's documented grammar; Markdown is the
        /// only export shape this verb offers today.
        #[arg(long)]
        md: bool,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
    },
}

/// `kb-code workspace note add <NAME-OR-ID> --repo R -b BODY [--at
/// PATH:LINE[-END]] [--reply-to ANN-ID]` — see
/// [`workspace_note_add_cmd`]'s doc.
#[derive(Subcommand, Debug)]
enum WorkspaceNoteCmd {
    Add {
        name_or_id: String,
        #[arg(long)]
        repo: String,
        #[arg(short = 'b', long = "body")]
        body: String,
        /// `PATH:LINE` or `PATH:START-END` — a code-anchored note. Omitted
        /// (with no `--reply-to` either) makes a general, path-less
        /// workspace note (`anchor_kind: "set"`).
        #[arg(long = "at")]
        at: Option<String>,
        /// Reply to an existing note in this workspace (`parent_id`) —
        /// inherits `set_id` server-side.
        #[arg(long = "reply-to")]
        reply_to: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:4747")]
        daemon: String,
        #[arg(long)]
        json: bool,
    },
}

/// Resolve a `NAME-OR-ID` against `GET /api/sets?repo=&kind=workspace`: an
/// EXACT name match first, else a UNIQUE id PREFIX match — same ladder
/// `resolve_set_id` uses, scoped to workspaces only.
async fn resolve_workspace_id(
    client: &reqwest::Client,
    daemon: &str,
    repo: &str,
    name_or_id: &str,
) -> Result<String> {
    let body = get_json(
        client,
        daemon,
        "/api/sets",
        &[("repo", repo), ("kind", "workspace")],
    )
    .await?;
    let sets = body["sets"].as_array().cloned().unwrap_or_default();
    if let Some(s) = sets.iter().find(|s| s["name"].as_str() == Some(name_or_id)) {
        return Ok(s["id"].as_str().unwrap_or_default().to_string());
    }
    let prefix_matches: Vec<&serde_json::Value> = sets
        .iter()
        .filter(|s| {
            s["id"]
                .as_str()
                .is_some_and(|id| id.starts_with(name_or_id))
        })
        .collect();
    match prefix_matches.len() {
        1 => Ok(prefix_matches[0]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string()),
        0 => anyhow::bail!("no workspace named or id-prefixed {name_or_id:?} in repo {repo:?}"),
        n => {
            anyhow::bail!("{name_or_id:?} matches {n} workspaces by id prefix — be more specific")
        }
    }
}

fn print_workspace_row(w: &serde_json::Value) {
    let ref_chip = w["ref"]
        .as_str()
        .map(|r| format!(" [{r}]"))
        .unwrap_or_default();
    println!(
        "{:<14} {:<26} {:>3} file(s) {:>3} note(s){ref_chip}  {}",
        w["id"].as_str().unwrap_or("?"),
        w["name"].as_str().unwrap_or("?"),
        w["span_count"].as_u64().unwrap_or(0),
        w["note_count"].as_u64().unwrap_or(0),
        w["description"].as_str().unwrap_or(""),
    );
}

/// `kb-code workspace list --repo R [--group ref] [--json]` —
/// `GET /api/sets?repo=&kind=workspace[&group=ref]`.
async fn workspace_list_cmd(
    daemon: &str,
    repo: &str,
    group: Option<&str>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let mut query: Vec<(&str, &str)> = vec![("repo", repo), ("kind", "workspace")];
    if let Some(g) = group {
        query.push(("group", g));
    }
    let body = get_json(&client, daemon, "/api/sets", &query).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    if let Some(groups) = body["groups"].as_array() {
        if groups.is_empty() {
            println!("(no workspaces in {repo})");
            return Ok(());
        }
        for g in groups {
            let r = g["ref"].as_str().unwrap_or("(no ref)");
            println!("# {r}");
            for w in g["workspaces"].as_array().cloned().unwrap_or_default() {
                print_workspace_row(&w);
            }
        }
        return Ok(());
    }
    let sets = body["sets"].as_array().cloned().unwrap_or_default();
    if sets.is_empty() {
        println!("(no workspaces in {repo})");
        return Ok(());
    }
    for w in &sets {
        print_workspace_row(w);
    }
    Ok(())
}

/// `kb-code workspace show <NAME-OR-ID> --repo R` — `GET /api/sets/{id}`.
async fn workspace_show_cmd(daemon: &str, repo: &str, name_or_id: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let id = resolve_workspace_id(&client, daemon, repo, name_or_id).await?;
    let body = get_json(&client, daemon, &format!("/api/sets/{id}"), &[]).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    print_set_view(&body);
    if let Some(r) = body["ref"].as_str() {
        println!("  ref: {r}");
    }
    if let Some(md) = body["description_md"].as_str() {
        if !md.is_empty() {
            println!("  {md}");
        }
    }
    Ok(())
}

/// `kb-code workspace save …` — see [`WorkspaceCmd::Save`]'s doc.
/// `--desk-json -` reads the snapshot from stdin (mirrors `review findings
/// import --stdin`'s own convention); a literal value is sent as-is (the
/// daemon validates it's JSON, `reading_sets::validate_desk_json`'s doc).
#[allow(clippy::too_many_arguments)]
async fn workspace_save_cmd(
    daemon: &str,
    repo: &str,
    name: &str,
    description: Option<&str>,
    description_file: Option<&Path>,
    git_ref: Option<&str>,
    desk_json: Option<&str>,
    files: &[String],
    json: bool,
) -> Result<()> {
    let description_md = match description_file {
        Some(p) => Some(
            std::fs::read_to_string(p)
                .with_context(|| format!("read --description-file {}", p.display()))?,
        ),
        None => None,
    };
    let desk_json_value: Option<String> = match desk_json {
        Some("-") => {
            let mut buf = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf)
                .context("read desk_json from stdin")?;
            Some(buf)
        }
        Some(s) => Some(s.to_string()),
        None => None,
    };
    let span_values: Vec<serde_json::Value> = files
        .iter()
        .map(|raw| {
            let (path, start, end) = parse_span_arg(raw);
            span_json(&path, start, end, None, None)
        })
        .collect();
    let mut payload = serde_json::json!({
        "repo": repo,
        "name": name,
        "kind": "workspace",
        "spans": span_values,
    });
    if let Some(d) = description {
        payload["description"] = serde_json::json!(d);
    }
    if let Some(md) = &description_md {
        payload["description_md"] = serde_json::json!(md);
    }
    if let Some(r) = git_ref {
        payload["ref"] = serde_json::json!(r);
    }
    if let Some(dj) = &desk_json_value {
        payload["desk_json"] = serde_json::json!(dj);
    }
    let client = http_client()?;
    let (status, body) = post_json_raw(&client, daemon, "/api/sets", &payload).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    }
    if status != reqwest::StatusCode::CREATED {
        return Err(annotation_api_error("save workspace", status, &body));
    }
    if !json {
        println!(
            "✓ saved workspace {name:?} ({})",
            body["id"].as_str().unwrap_or("?")
        );
    }
    Ok(())
}

/// `kb-code workspace open <NAME-OR-ID> --repo R [--print-url]` — prints
/// the SPA URL that restores this workspace: the FIRST entry's reader URL
/// (`/r/{repo}/{path}[?line=N]`) plus `?workspace={id}` — the SPA reads
/// that param on mount to restore the desk + working set (V70-A10's own
/// SPA deliverable, `Reader.tsx`). Prefixed with `daemon` — kb-code-server
/// serves the SPA itself at its own origin (`spa.rs`'s doc), so the printed
/// URL is directly clickable. Never launches a browser.
async fn workspace_open_cmd(daemon: &str, repo: &str, name_or_id: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let id = resolve_workspace_id(&client, daemon, repo, name_or_id).await?;
    let body = get_json(&client, daemon, &format!("/api/sets/{id}"), &[]).await?;
    let spans = body["spans"].as_array().cloned().unwrap_or_default();
    let first = spans.first();
    let path = first.and_then(|s| s["path"].as_str()).unwrap_or("");
    let line = first.and_then(|s| s["line_start"].as_u64());
    let mut url = format!(
        "{}/r/{}/{}?workspace={}",
        daemon.trim_end_matches('/'),
        repo,
        path,
        id
    );
    if let Some(l) = line {
        url.push_str(&format!("&line={l}"));
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "id": id, "url": url }))?
        );
    } else {
        println!("{url}");
    }
    Ok(())
}

/// `kb-code workspace note add …` — `POST /api/annotations` with `set_id`
/// — see [`WorkspaceNoteCmd::Add`]'s doc.
async fn workspace_note_add_cmd(
    daemon: &str,
    repo: &str,
    name_or_id: &str,
    body: &str,
    at: Option<&str>,
    reply_to: Option<&str>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let set_id = resolve_workspace_id(&client, daemon, repo, name_or_id).await?;
    let mut payload = serde_json::json!({ "repo": repo, "body": body, "set_id": set_id });
    if let Some(parent) = reply_to {
        payload["path"] = serde_json::json!("");
        payload["parent_id"] = serde_json::json!(parent);
    } else if let Some(target) = at {
        let (path, start, end) = parse_span_arg(target);
        let start = start.ok_or_else(|| {
            anyhow::anyhow!("--at requires PATH:LINE or PATH:START-END, got {target:?}")
        })?;
        payload["path"] = serde_json::json!(path);
        payload["line"] = serde_json::json!(start);
        if let Some(e) = end {
            if e != start {
                payload["anchor_kind"] = serde_json::json!("range");
                payload["line_end"] = serde_json::json!(e);
            }
        }
    } else {
        payload["path"] = serde_json::json!("");
        payload["anchor_kind"] = serde_json::json!(kb_code_server::annotations::ANCHOR_KIND_SET);
    }
    let (status, resp) = post_json_raw(&client, daemon, "/api/annotations", &payload).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&resp)?);
    }
    if status != reqwest::StatusCode::CREATED {
        return Err(annotation_api_error("add workspace note", status, &resp));
    }
    if !json {
        print!("✓ noted ");
        print_annotation_row(&resp, "");
    }
    Ok(())
}

/// `kb-code workspace export <NAME-OR-ID> --repo R --md` — title, ref,
/// description, entries as `path:line` links, notes threaded (general
/// notes first, then code-anchored ones — `sort_by_key`'s stability keeps
/// each group's own creation order, since `anns` arrives oldest-first from
/// the server) — printed to stdout.
async fn workspace_export_cmd(daemon: &str, repo: &str, name_or_id: &str) -> Result<()> {
    let client = http_client()?;
    let id = resolve_workspace_id(&client, daemon, repo, name_or_id).await?;
    let set = get_json(&client, daemon, &format!("/api/sets/{id}"), &[]).await?;
    let notes = get_json(
        &client,
        daemon,
        "/api/annotations",
        &[("set_id", id.as_str())],
    )
    .await?;

    let mut out = String::new();
    let name = set["name"].as_str().unwrap_or("?");
    out.push_str(&format!("# {name}\n\n"));
    if let Some(r) = set["ref"].as_str() {
        out.push_str(&format!("*ref: `{r}`*\n\n"));
    }
    let desc = set["description_md"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| set["description"].as_str().filter(|s| !s.is_empty()));
    if let Some(d) = desc {
        out.push_str(d);
        out.push_str("\n\n");
    }

    out.push_str("## Files\n\n");
    let spans = set["spans"].as_array().cloned().unwrap_or_default();
    if spans.is_empty() {
        out.push_str("_(none)_\n\n");
    }
    for s in &spans {
        let path = s["path"].as_str().unwrap_or("?");
        let lines = match (s["line_start"].as_u64(), s["line_end"].as_u64()) {
            (Some(a), Some(b)) if a == b => format!(":{a}"),
            (Some(a), Some(b)) => format!(":{a}-{b}"),
            _ => String::new(),
        };
        let note = s["note"]
            .as_str()
            .map(|n| format!(" — {n}"))
            .unwrap_or_default();
        out.push_str(&format!("- `{path}{lines}`{note}\n"));
    }
    out.push('\n');

    let anns = notes["annotations"].as_array().cloned().unwrap_or_default();
    out.push_str("## Notes\n\n");
    if anns.is_empty() {
        out.push_str("_(none)_\n");
    } else {
        let mut top: Vec<&serde_json::Value> = Vec::new();
        let mut replies: std::collections::HashMap<&str, Vec<&serde_json::Value>> =
            std::collections::HashMap::new();
        for a in &anns {
            match a["parent_id"].as_str() {
                Some(pid) => replies.entry(pid).or_default().push(a),
                None => top.push(a),
            }
        }
        top.sort_by_key(|a| !a["path"].as_str().unwrap_or("").is_empty());
        for a in &top {
            let path = a["path"].as_str().unwrap_or("");
            let author = a["author"].as_str().unwrap_or("?");
            let text = a["body"].as_str().unwrap_or("");
            if path.is_empty() {
                out.push_str(&format!("- **{author}**: {text}\n"));
            } else {
                let line = a["line"].as_u64().unwrap_or(0);
                out.push_str(&format!("- **{author}** at `{path}:{line}`: {text}\n"));
            }
            if let Some(id_str) = a["id"].as_str() {
                if let Some(rs) = replies.get(id_str) {
                    for r in rs {
                        let rauthor = r["author"].as_str().unwrap_or("?");
                        let rtext = r["body"].as_str().unwrap_or("");
                        out.push_str(&format!("  - **{rauthor}**: {rtext}\n"));
                    }
                }
            }
        }
    }

    print!("{out}");
    Ok(())
}

// ── V71-G0 — `kb-code entity` / `kb-code seq` ────────────────────────────
//
// Both verbs build their request as DATA through the two functions below,
// and both take the route PATH from the server crate's own declared
// contract (`kb_code_server::entities::V71_G0_ROUTES`) rather than a
// string literal. That is what makes the pair testable against each other:
// `cli_requests_send_every_param_their_route_requires` walks every declared
// route and fails loudly when no CLI verb builds a request for it, or when
// a request omits a param the route declares required — the exact v7.0
// defect where `review impact` never sent the param its route needed.

/// The `GET /api/entity` request: `(path, query)`.
fn entity_request(
    repo: &str,
    ent: &str,
    worktree: Option<&str>,
) -> (&'static str, Vec<(&'static str, String)>) {
    let mut query: Vec<(&'static str, String)> =
        vec![("repo", repo.to_string()), ("ent", ent.to_string())];
    if let Some(w) = worktree {
        query.push(("worktree", w.to_string()));
    }
    (kb_code_server::entities::ENTITY_ROUTE.path, query)
}

/// The `GET /api/seq` request: `(path, query)`.
fn seq_request(
    repo: &str,
    projection: Option<&str>,
    workspace: Option<&str>,
) -> (&'static str, Vec<(&'static str, String)>) {
    let mut query: Vec<(&'static str, String)> = vec![("repo", repo.to_string())];
    if let Some(p) = projection {
        query.push(("projection", p.to_string()));
    }
    if let Some(w) = workspace {
        query.push(("workspace", w.to_string()));
    }
    (kb_code_server::seq::SEQ_ROUTE.path, query)
}

fn as_query_pairs<'a>(query: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    query.iter().map(|(k, v)| (*k, v.as_str())).collect()
}

/// `kb-code entity <NAME> --repo R` — `GET /api/entity`.
async fn entity_cmd(
    daemon: &str,
    repo: &str,
    name: &str,
    worktree: Option<&str>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let (path, query) = entity_request(repo, name, worktree);
    let body = get_json(&client, daemon, path, &as_query_pairs(&query)).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let entities = body["entities"].as_array().cloned().unwrap_or_default();
    for e in &entities {
        let fqn = e["fqn"].as_str().unwrap_or("?");
        let kind = e["kind"].as_str().unwrap_or("?");
        let defs = e["definitions"].as_array().cloned().unwrap_or_default();
        let c = &e["trust_counts"];
        println!(
            "{fqn} ({kind}) — {} definition site(s)  [exact {} · likely {} · candidate {}]",
            defs.len(),
            c["exact"].as_i64().unwrap_or(0),
            c["likely"].as_i64().unwrap_or(0),
            c["candidate"].as_i64().unwrap_or(0),
        );
        for d in defs {
            let stale = if d["stale"].as_bool().unwrap_or(false) {
                "  (stale — the indexed blob is no longer the one at this path)"
            } else {
                ""
            };
            println!(
                "  {}:{}-{}  {}  via {}{stale}",
                d["path"].as_str().unwrap_or("?"),
                d["line_start"].as_i64().unwrap_or(0),
                d["line_end"].as_i64().unwrap_or(0),
                d["trust"].as_str().unwrap_or("?"),
                d["matched_via"].as_str().unwrap_or("?"),
            );
        }
    }
    if body["ambiguous"].as_bool().unwrap_or(false) {
        println!("(ambiguous — every constant answering to this name is listed above)");
    }
    for n in body["notes"].as_array().cloned().unwrap_or_default() {
        if let Some(n) = n.as_str() {
            println!("note: {n}");
        }
    }
    Ok(())
}

/// `kb-code seq list --repo R` — `GET /api/seq`.
async fn seq_list_cmd(
    daemon: &str,
    repo: &str,
    projection: Option<&str>,
    workspace: Option<&str>,
    json: bool,
) -> Result<()> {
    let client = http_client()?;
    let (path, query) = seq_request(repo, projection, workspace);
    let body = get_json(&client, daemon, path, &as_query_pairs(&query)).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    for p in body["projections"].as_array().cloned().unwrap_or_default() {
        let size = match p["size"].as_i64() {
            Some(n) => format!("{n} item(s)"),
            None => "size unknown".to_string(),
        };
        let bound = match p["workspace_id"].as_str() {
            Some(w) => format!("  → workspace {w}"),
            None => String::new(),
        };
        let r = match p["ref"].as_str() {
            Some(r) => format!("  ref {r}"),
            None => String::new(),
        };
        println!(
            "{:<10} {:<18} {:<32} {size}{r}{bound}",
            p["projection"].as_str().unwrap_or("?"),
            p["id"].as_str().unwrap_or("?"),
            p["name"].as_str().unwrap_or("?"),
        );
    }
    for n in body["notes"].as_array().cloned().unwrap_or_default() {
        if let Some(n) = n.as_str() {
            println!("note: {n}");
        }
    }
    Ok(())
}

// ── V71-E2 — `kb-code act` ───────────────────────────────────────────────
//
// The CLI half of `kbc-actions/1`. Like `entity`/`seq`, the request is
// built as DATA by one function that takes the route PATH from the server
// crate's own declared contract (`kb_code_server::actions::ACTIONS_ROUTE`),
// so `cli_requests_send_every_param_their_route_requires` can walk the two
// against each other.

/// A `kb-code act` target: `PATH`, `PATH:LINE`, `PATH:LINE:COL` or
/// `PATH:A-B`. Returns `(path, line, col, end_line)`.
///
/// The range form is tried FIRST on the trailing segment, because `A-B` and
/// `LINE` are unambiguous but `PATH:12-40:3` is not a shape this grammar
/// admits — a range has no column, by construction (a multi-line selection's
/// columns are the buffer's business, and the route's `end_col` is optional
/// for exactly that reason).
fn parse_act_target(s: &str) -> Result<(String, u32, u32, Option<u32>)> {
    if let Some((head, tail)) = s.rsplit_once(':') {
        if let Some((a, b)) = tail.split_once('-') {
            if let (Ok(a), Ok(b)) = (a.parse::<u32>(), b.parse::<u32>()) {
                if !head.is_empty() {
                    if b < a {
                        anyhow::bail!("range end {b} is before start {a} in {s:?}");
                    }
                    return Ok((head.to_string(), a, 0, Some(b)));
                }
            }
        }
    }
    if let Ok((path, line, col)) = parse_resolve_target(s) {
        return Ok((path, line, col, None));
    }
    let (path, line) = parse_path_line(s);
    if path.is_empty() {
        anyhow::bail!("expected PATH, PATH:LINE, PATH:LINE:COL or PATH:A-B, got {s:?}");
    }
    Ok((path, line.unwrap_or(1), 0, None))
}

/// The `GET /api/actions` request: `(path, query)`.
#[allow(clippy::too_many_arguments)]
fn actions_request(
    repo: &str,
    path: &str,
    line: u32,
    col: u32,
    end_line: Option<u32>,
    rev: Option<&str>,
    text: Option<&str>,
    target_index: Option<usize>,
) -> (&'static str, Vec<(&'static str, String)>) {
    let mut query: Vec<(&'static str, String)> = vec![
        ("repo", repo.to_string()),
        ("path", path.to_string()),
        ("line", line.to_string()),
        ("col", col.to_string()),
    ];
    if let Some(e) = end_line {
        query.push(("end_line", e.to_string()));
    }
    if let Some(r) = rev {
        query.push(("ref", r.to_string()));
    }
    if let Some(t) = text {
        query.push(("text", t.to_string()));
    }
    if let Some(i) = target_index {
        query.push(("target", i.to_string()));
    }
    (kb_code_server::actions::ACTIONS_ROUTE.path, query)
}

/// `true` when `id` is an ORDINAL rather than a stable id. D5's rule: `act`
/// never accepts one, because "whatever is second today" is precisely the
/// muscle-memory failure the stable ordering exists to prevent, and a script
/// that pins an ordinal breaks silently the day a row is added.
fn act_id_is_ordinal(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_digit())
}

/// Find one row by id across every group, and report the closest names when
/// there is no hit (Kakoune's docstring posture: never a bare "unknown").
fn act_find_row<'a>(body: &'a serde_json::Value, id: &str) -> Result<&'a serde_json::Value> {
    for g in body["groups"].as_array().into_iter().flatten() {
        for a in g["actions"].as_array().into_iter().flatten() {
            if a["id"].as_str() == Some(id) {
                return Ok(a);
            }
        }
    }
    let known: Vec<&str> = body["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|g| g["actions"].as_array().into_iter().flatten())
        .filter_map(|a| a["id"].as_str())
        .collect();
    anyhow::bail!(
        "no action {id:?} for this target — available here: {}",
        if known.is_empty() {
            "(none)".to_string()
        } else {
            known.join(", ")
        }
    )
}

#[allow(clippy::too_many_arguments)]
async fn act_cmd(
    daemon: &str,
    repo: &str,
    id: Option<&str>,
    list: Option<&str>,
    at: Option<&str>,
    rev: Option<&str>,
    text: Option<&str>,
    target_index: Option<usize>,
    confirm: bool,
    json: bool,
) -> Result<()> {
    let target = match (list, at) {
        (Some(t), None) => t,
        (None, Some(t)) => t,
        (Some(_), Some(_)) => anyhow::bail!("pass either --list TARGET or --at TARGET, not both"),
        (None, None) => {
            anyhow::bail!("pass --list TARGET to see the menu, or --at TARGET with an action id")
        }
    };
    if list.is_some() && id.is_some() {
        anyhow::bail!("--list prints the whole menu; drop the action id or use --at instead");
    }
    if at.is_some() && id.is_none() {
        anyhow::bail!("--at TARGET needs an action id: `kb-code act <id> --at {target}`");
    }
    if let Some(id) = id {
        if act_id_is_ordinal(id) {
            anyhow::bail!(
                "{id:?} is an ordinal — `kb-code act` takes a stable action id \
                 (run `kb-code act --list {target} --repo {repo}` to see them)"
            );
        }
    }

    let (path, line, col, end_line) = parse_act_target(target)?;
    let client = http_client()?;
    let (route, owned) = actions_request(repo, &path, line, col, end_line, rev, text, target_index);
    let q: Vec<(&str, &str)> = owned.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let body = get_json(&client, daemon, route, &q).await?;

    let Some(id) = id else {
        if json {
            println!("{}", serde_json::to_string_pretty(&body)?);
            return Ok(());
        }
        return print_actions(&body);
    };

    let row = act_find_row(&body, id)?.clone();
    if row["mutating"].as_bool().unwrap_or(false) && !confirm {
        anyhow::bail!(
            "{id} is a mutating action — re-run with --confirm \
             (kb-code act {id} --at {target} --repo {repo} --confirm)"
        );
    }
    if !row["enabled"].as_bool().unwrap_or(true) {
        let why = row["disabled_reason"].as_str().unwrap_or("no reason given");
        anyhow::bail!("{id} is not available here: {why}");
    }

    // A row backed by a daemon READ is actually performed; every other op is
    // a CLIENT operation, so the honest answer is the op plus the command
    // line that does it.
    if let Some(req) = row.get("request").filter(|v| !v.is_null()) {
        let rpath = req["path"].as_str().unwrap_or_default().to_string();
        let pairs: Vec<(String, String)> = req["query"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|p| {
                let a = p.as_array()?;
                Some((
                    a.first()?.as_str()?.to_string(),
                    a.get(1)?.as_str()?.to_string(),
                ))
            })
            .collect();
        let rq: Vec<(&str, &str)> = pairs
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let out = get_json(&client, daemon, &rpath, &rq).await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&out)?);
        } else {
            println!("{id} · {}", row["title"].as_str().unwrap_or(id));
            println!("  GET {rpath}");
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
        return Ok(());
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&row)?);
        return Ok(());
    }
    println!("{id} · {}", row["title"].as_str().unwrap_or(id));
    println!("  {}", row["doc"].as_str().unwrap_or(""));
    println!("  op: {}", serde_json::to_string(&row["op"])?);
    match row["cli"].as_str() {
        Some(c) => println!("  run: {c}"),
        None => println!("  run: (browser-local — this row has no CLI equivalent)"),
    }
    Ok(())
}

/// Human rendering of a `kbc-actions/1` body. The segmented control is
/// printed as the target LIST (never a hidden cycle), the active one
/// marked, and the withheld mutating group is stated rather than left as a
/// silent gap.
fn print_actions(body: &serde_json::Value) -> Result<()> {
    let repo = body["repo"].as_str().unwrap_or("?");
    let active = body["active"].as_u64().unwrap_or(0) as usize;
    let targets = body["targets"].as_array().cloned().unwrap_or_default();
    println!("actions · {repo}");
    for (i, t) in targets.iter().enumerate() {
        println!(
            "  {} [{i}] {:<9} {}",
            if i == active { "▸" } else { " " },
            t["kind"].as_str().unwrap_or("?"),
            t["label"].as_str().unwrap_or("?"),
        );
        if let Some(n) = t["note"].as_str() {
            println!("        {n}");
        }
    }
    for g in body["groups"].as_array().into_iter().flatten() {
        println!("  {}", g["title"].as_str().unwrap_or("?"));
        for a in g["actions"].as_array().into_iter().flatten() {
            let key = a["key"].as_str().unwrap_or(" ");
            let id = a["id"].as_str().unwrap_or("?");
            let title = a["title"].as_str().unwrap_or("");
            let flags = match (
                a["mutating"].as_bool().unwrap_or(false),
                a["enabled"].as_bool().unwrap_or(true),
            ) {
                (true, _) => "  [mutating · needs --confirm]",
                (_, false) => "  [unavailable]",
                _ => "",
            };
            println!("    {key}  {id:<28} {title}{flags}");
            if let Some(r) = a["disabled_reason"].as_str() {
                println!("       └ {r}");
            }
        }
    }
    let m = &body["mutations"];
    if !m["available"].as_bool().unwrap_or(false) {
        println!("  Change: {}", m["reason"].as_str().unwrap_or(""));
    }
    for n in body["notes"].as_array().cloned().unwrap_or_default() {
        if let Some(n) = n.as_str() {
            println!("note: {n}");
        }
    }
    Ok(())
}

// ── V71-F1 — `kb-code tree --view …` / `kb-code scope …` ─────────────────
//
// Both verbs build their request as DATA through `tree_v2_request`, and
// take the route PATH from the server crate's own declared contract
// (`kb_code_server::tree::V71_F1_ROUTES`) rather than a string literal —
// the same shape V71-G0 established, and what makes
// `cli_requests_send_every_param_their_route_requires` able to see this
// unit's route at all.
//
// The renderers below are FORMATTERS. Every number they print is the
// daemon's own (`counts`, `unplaced_total`, `truncated`, the folder
// aggregates); none is re-derived from the rows that happened to arrive,
// which is the exact way a second implementation starts.

/// Everything `GET /api/tree/2` takes, as one borrow-only struct so the
/// request builder has no eleven-argument signature.
#[derive(Debug, Default, Clone, Copy)]
struct TreeV2Opts<'a> {
    repo: &'a str,
    view: Option<&'a str>,
    root: Option<&'a str>,
    scope: Option<&'a str>,
    filter: Option<&'a str>,
    mode: Option<&'a str>,
    decorate: Option<&'a str>,
    base: Option<&'a str>,
    review: Option<i64>,
    depth: Option<u32>,
    expand: Option<&'a str>,
    limit: Option<usize>,
}

/// The `GET /api/tree/2` request: `(path, query)`. `repo` is always sent —
/// it is the route's one required param and the contract test proves this
/// builder sends it.
fn tree_v2_request(o: &TreeV2Opts<'_>) -> (&'static str, Vec<(&'static str, String)>) {
    let mut query: Vec<(&'static str, String)> = vec![("repo", o.repo.to_string())];
    let mut push = |k: &'static str, v: Option<&str>| {
        if let Some(v) = v.filter(|s| !s.is_empty()) {
            query.push((k, v.to_string()));
        }
    };
    push("view", o.view);
    push("root", o.root);
    push("scope", o.scope);
    push("filter", o.filter);
    push("mode", o.mode);
    push("decorate", o.decorate);
    push("base", o.base);
    push("expand", o.expand);
    if let Some(r) = o.review {
        query.push(("review", r.to_string()));
    }
    if let Some(d) = o.depth {
        query.push(("depth", d.to_string()));
    }
    if let Some(l) = o.limit {
        query.push(("limit", l.to_string()));
    }
    (kb_code_server::tree::TREE_V2_ROUTE.path, query)
}

async fn fetch_tree_v2(daemon: &str, o: &TreeV2Opts<'_>) -> Result<serde_json::Value> {
    let client = http_client()?;
    let (path, query) = tree_v2_request(o);
    get_json(&client, daemon, path, &as_query_pairs(&query)).await
}

/// One row's decoration suffix — only the lanes the response says ran.
fn tree_facts_cell(facts: &serde_json::Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(g) = facts["git"].as_str() {
        parts.push(format!("git:{g}"));
    }
    if let Some(n) = facts["git_changed"].as_u64() {
        parts.push(format!("git:{n}\u{2206}"));
    }
    if let Some(r) = facts["review"].as_str() {
        parts.push(r.to_string());
    }
    if let (Some(v), Some(t)) = (
        facts["review_viewed"].as_u64(),
        facts["review_total"].as_u64(),
    ) {
        parts.push(format!("viewed {v}/{t}"));
    }
    if let Some(f) = facts["findings"].as_str() {
        parts.push(format!("finding:{f}"));
    }
    for (key, label) in [("annot", "annot"), ("todo", "todo"), ("bookmark", "mark")] {
        if let Some(n) = facts[key].as_u64() {
            parts.push(format!("{label}:{n}"));
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("   [{}]", parts.join(" \u{b7} "))
    }
}

/// Print the honesty fields — always, in every format. In `paths` mode they
/// go to STDERR so a `| xargs` pipeline stays clean while the human still
/// sees that they were given part of the tree.
fn print_tree_honesty(body: &serde_json::Value, to_stderr: bool) {
    let mut lines: Vec<String> = Vec::new();
    if body["scope_applied"].as_bool() == Some(false) && !body["scope"].is_null() {
        lines.push(format!(
            "scope NOT applied: {}",
            body["scope"].as_str().unwrap_or("")
        ));
    }
    for n in body["notes"].as_array().cloned().unwrap_or_default() {
        if let Some(n) = n.as_str() {
            lines.push(format!("note: {n}"));
        }
    }
    for d in body["diagnostics"].as_array().cloned().unwrap_or_default() {
        lines.push(format!(
            "warning: {} — {}",
            d["token"].as_str().unwrap_or("?"),
            d["message"].as_str().unwrap_or("")
        ));
    }
    let t = &body["truncated"];
    if !t.is_null() {
        lines.push(format!(
            "TRUNCATED by {}: {} of {} rows — {}",
            t["by"].as_str().unwrap_or("?"),
            t["returned"].as_u64().unwrap_or(0),
            t["total"].as_u64().unwrap_or(0),
            t["reason"].as_str().unwrap_or(""),
        ));
    }
    let unplaced = body["unplaced_total"].as_u64().unwrap_or(0);
    if unplaced > 0 {
        lines.push(format!(
            "UNPLACED: {unplaced} file(s) this projection could not place"
        ));
    }
    for l in lines {
        if to_stderr {
            eprintln!("{l}");
        } else {
            println!("{l}");
        }
    }
}

/// `kb-code tree --view …` — `GET /api/tree/2`.
async fn tree_v2_cmd(daemon: &str, opts: &TreeV2Opts<'_>, format: &str) -> Result<()> {
    let body = fetch_tree_v2(daemon, opts).await?;
    if format == "json" {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let rows = body["rows"].as_array().cloned().unwrap_or_default();
    if format == "paths" {
        for r in &rows {
            if r["kind"] == "file" {
                if let Some(p) = r["path"].as_str() {
                    println!("{p}");
                }
            }
        }
        print_tree_honesty(&body, true);
        return Ok(());
    }
    if format != "tree" {
        anyhow::bail!("unknown --format {format:?} — expected tree, paths or json");
    }
    let counts = &body["counts"];
    println!(
        "{} \u{b7} view {} \u{b7} {} file(s){}{}",
        body["repo"].as_str().unwrap_or("?"),
        body["view"].as_str().unwrap_or("?"),
        counts["files"].as_u64().unwrap_or(0),
        match counts["matched"].as_u64() {
            Some(m) => format!(" \u{b7} {m} matched"),
            None => String::new(),
        },
        match body["scope"].as_str() {
            Some(sc) if body["scope_applied"].as_bool() == Some(true) =>
                format!(" \u{b7} scope {sc}"),
            _ => String::new(),
        },
    );
    for r in &rows {
        let depth = r["depth"].as_u64().unwrap_or(0) as usize;
        let trust = match r["trust"].as_str() {
            Some(t) => format!(" ({t})"),
            None => String::new(),
        };
        let more = if r["has_more"].as_bool().unwrap_or(false) {
            " \u{2026}"
        } else {
            ""
        };
        // A match count is an ANCESTOR badge — on a file row it would just
        // restate "this row matched", which the highlight already says.
        let matches = match r["match_count"].as_u64().unwrap_or(0) {
            n if n > 0 && r["kind"] != "file" => format!(" [{n}]"),
            _ => String::new(),
        };
        println!(
            "{:indent$}{}{trust}{matches}{more}{}",
            "",
            r["label"].as_str().unwrap_or("?"),
            tree_facts_cell(&r["facts"]),
            indent = depth * 2,
        );
    }
    let unplaced = body["unplaced"].as_array().cloned().unwrap_or_default();
    if !unplaced.is_empty() {
        println!("\nUnplaced:");
        for r in &unplaced {
            println!("  {}", r["label"].as_str().unwrap_or("?"));
        }
    }
    print_tree_honesty(&body, false);
    Ok(())
}

/// Resolve one expression and return `(files, applied, notes)` — the ONE
/// place `kb-code scope` learns a count, so every count it prints is the
/// daemon's own.
async fn resolve_scope_count(
    daemon: &str,
    repo: &str,
    expr: &str,
) -> Result<(u64, bool, Vec<String>)> {
    let body = fetch_tree_v2(
        daemon,
        &TreeV2Opts {
            repo,
            scope: Some(expr),
            depth: Some(1),
            limit: Some(1),
            ..Default::default()
        },
    )
    .await?;
    let notes = body["notes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|n| n.as_str().map(str::to_string))
        .collect();
    Ok((
        body["counts"]["files"].as_u64().unwrap_or(0),
        body["scope_applied"].as_bool().unwrap_or(false),
        notes,
    ))
}

/// `kb-code scope list --repo R`.
async fn scope_list_cmd(daemon: &str, repo: &str, json: bool) -> Result<()> {
    let client = http_client()?;
    let cfg = get_json(&client, daemon, "/api/scopes", &[]).await?;
    let map = cfg["scopes"].as_object().cloned().unwrap_or_default();
    let mut out = Vec::new();
    for (name, patterns) in &map {
        let (files, applied, _) = resolve_scope_count(daemon, repo, &format!("${name}")).await?;
        out.push(serde_json::json!({
            "name": name,
            "source": "config",
            "patterns": patterns,
            "resolved_files": files,
            "applied": applied,
        }));
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": kb_code_server::tree::scope::SCOPE_SCHEMA,
                "repo": repo,
                "scopes": out,
            }))?
        );
        return Ok(());
    }
    if out.is_empty() {
        println!("no scopes configured — add a `[scopes]` block to kb-code.toml");
        return Ok(());
    }
    for s in &out {
        let files = s["resolved_files"].as_u64().unwrap_or(0);
        let drift = if files == 0 {
            "   <- resolves to NOTHING today"
        } else {
            ""
        };
        println!(
            "{:<24} {:>6} file(s)  source:config{drift}",
            s["name"].as_str().unwrap_or("?"),
            files,
        );
    }
    Ok(())
}

/// `kb-code scope show <NAME|EXPR> --repo R`.
async fn scope_show_cmd(
    daemon: &str,
    repo: &str,
    name_or_expr: &str,
    want_paths: bool,
    json: bool,
) -> Result<()> {
    // A bare configured NAME is sugar for `$name`; anything containing a
    // `:` or an operator is already an expression.
    let expr = if name_or_expr.contains(':')
        || name_or_expr.contains('$')
        || name_or_expr.contains('&')
        || name_or_expr.contains('|')
        || name_or_expr.contains('!')
    {
        name_or_expr.to_string()
    } else {
        format!("${name_or_expr}")
    };
    let body = fetch_tree_v2(
        daemon,
        &TreeV2Opts {
            repo,
            scope: Some(&expr),
            depth: Some(0),
            limit: Some(if want_paths { 10_000 } else { 1 }),
            ..Default::default()
        },
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    println!(
        "{expr}\n  normalized: {}\n  applied:    {}\n  files:      {}",
        body["scope"].as_str().unwrap_or("(none)"),
        body["scope_applied"].as_bool().unwrap_or(false),
        body["counts"]["files"].as_u64().unwrap_or(0),
    );
    if want_paths {
        for r in body["rows"].as_array().cloned().unwrap_or_default() {
            if r["kind"] == "file" {
                if let Some(p) = r["path"].as_str() {
                    println!("{p}");
                }
            }
        }
    }
    print_tree_honesty(&body, false);
    Ok(())
}

/// `kb-code scope from-paths <PATH>… --repo R` — the proposal engine is
/// PURE and lives in the server crate (`tree::scope::propose`); the delta
/// against the real repo is one resolve per proposal.
async fn scope_from_paths_cmd(
    daemon: &str,
    repo: &str,
    paths: &[String],
    json: bool,
) -> Result<()> {
    let proposals = kb_code_server::tree::scope::propose(paths);
    let mut out = Vec::new();
    for p in &proposals {
        let (files, applied, notes) = resolve_scope_count(daemon, repo, &p.expr).await?;
        let extra = files.saturating_sub(paths.len() as u64);
        out.push(serde_json::json!({
            "expr": p.expr,
            "why": p.why,
            "exact_enumeration": p.exact_enumeration,
            "resolved_files": files,
            "selected": paths.len(),
            "extra": extra,
            "applied": applied,
            "notes": notes,
        }));
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": kb_code_server::tree::scope::SCOPE_SCHEMA,
                "repo": repo,
                "selection": paths,
                "proposals": out,
            }))?
        );
        return Ok(());
    }
    println!("{} file(s) selected. Proposals:\n", paths.len());
    for p in &out {
        let extra = p["extra"].as_u64().unwrap_or(0);
        let delta = if extra == 0 {
            "exactly your selection".to_string()
        } else {
            format!("+{extra} file(s) you did NOT select")
        };
        println!(
            "  {}\n    {}\n    resolves to {} file(s) \u{2014} {delta}\n",
            p["expr"].as_str().unwrap_or("?"),
            p["why"].as_str().unwrap_or(""),
            p["resolved_files"].as_u64().unwrap_or(0),
        );
    }
    println!("Nothing was saved. To keep one, paste it into kb-code.toml:\n");
    println!("[scopes]");
    if let Some(first) = out.first() {
        println!("# my-scope = <this expression is kbc-scope/1, not a glob list>");
        println!(
            "# kb-code tree --repo {repo} --daemon <url> --scope '{}'",
            first["expr"].as_str().unwrap_or("")
        );
    }
    Ok(())
}

/// `kb-code scope import <packwerk|codeowners> --repo R`.
async fn scope_import_cmd(daemon: &str, repo: &str, source: &str, json: bool) -> Result<()> {
    let derived: Vec<(String, Vec<String>)> = match source {
        "packwerk" => {
            let body = fetch_tree_v2(
                daemon,
                &TreeV2Opts {
                    repo,
                    depth: Some(0),
                    limit: Some(kb_code_server::tree::MAX_TREE_ROWS),
                    ..Default::default()
                },
            )
            .await?;
            let rows = body["rows"].as_array().cloned().unwrap_or_default();
            let paths: Vec<String> = rows
                .iter()
                .filter(|r| r["kind"] == "file")
                .filter_map(|r| r["path"].as_str().map(str::to_string))
                .collect();
            print_tree_honesty(&body, true);
            kb_code_server::tree::scope::packs_from_paths(paths.iter().map(String::as_str))
                .into_iter()
                .filter(|(dir, _)| !dir.is_empty())
                .map(|(dir, name)| (format!("pack-{name}"), vec![format!("{dir}/**")]))
                .collect()
        }
        "codeowners" => {
            let client = http_client()?;
            let mut text = None;
            for rel in kb_code_server::tree::sources::CODEOWNERS_PATHS {
                if let Ok(body) = get_json(
                    &client,
                    daemon,
                    "/api/file",
                    &[("repo", repo), ("path", rel)],
                )
                .await
                {
                    if let Ok(bytes) = decode_file_content(&body) {
                        text = Some((rel, String::from_utf8_lossy(&bytes).into_owned()));
                        break;
                    }
                }
            }
            let Some((rel, text)) = text else {
                anyhow::bail!(
                    "no CODEOWNERS found in {} \u{2014} nothing to import",
                    kb_code_server::tree::sources::CODEOWNERS_PATHS.join(", ")
                );
            };
            eprintln!("note: read {rel}");
            let mut by_owner: std::collections::BTreeMap<String, Vec<String>> =
                std::collections::BTreeMap::new();
            for (pattern, owners) in kb_code_server::tree::scope::parse_codeowners(&text) {
                for o in owners {
                    by_owner.entry(o).or_default().push(pattern.clone());
                }
            }
            by_owner
                .into_iter()
                .map(|(owner, pats)| {
                    (
                        format!("owner-{}", owner.trim_start_matches('@').replace('/', "-")),
                        pats,
                    )
                })
                .collect()
        }
        other => {
            anyhow::bail!("unknown scope source {other:?} \u{2014} expected packwerk or codeowners")
        }
    };

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": kb_code_server::tree::scope::SCOPE_SCHEMA,
                "source": source,
                "scopes": derived
                    .iter()
                    .map(|(n, p)| serde_json::json!({"name": n, "patterns": p}))
                    .collect::<Vec<_>>(),
            }))?
        );
        return Ok(());
    }
    if derived.is_empty() {
        println!("nothing to import from {source}");
        return Ok(());
    }
    println!(
        "# {} scope(s) derived from {source}. kb-code saved NOTHING \u{2014} paste what you want:\n[scopes]",
        derived.len()
    );
    for (name, patterns) in &derived {
        let pats = patterns
            .iter()
            .map(|p| format!("{p:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        println!("{name} = [{pats}]");
    }
    if source == "codeowners" {
        eprintln!(
            "note: CODEOWNERS is LAST-match-wins; a `[scopes]` glob list is ANY-match, so an \
             imported owner scope is a superset of what GitHub would assign. `owner:` inside a \
             kbc-scope/1 expression keeps GitHub's own semantics \u{2014} the import is a \
             starting point, not a translation."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- V70-A5: `commands doctor` IS the CI gate ---------------------------
    //
    // §P2: "`kb-code commands doctor` is a CI gate." Wiring it to a `#[test]`
    // rather than only to a CLI verb means `cargo test -p kb-code-cli` — which
    // `just ci-code` already runs — fails the build on registry drift, with no
    // new CI step to remember to add. It also means the twin check runs
    // against the clap tree of the binary being TESTED, which is the whole
    // point of cross-joining rather than string-matching.

    #[test]
    fn command_registry_passes_doctor() {
        let reg = commands::load().expect("registry.json parses");
        let rep = commands::doctor(&reg, &<Cli as clap::CommandFactory>::command());
        assert!(
            rep.ok(),
            "kbc-cmd/1 registry has {} problem(s):\n  {}\n\nRun `kb-code commands doctor` for the full report.",
            rep.failures.len(),
            rep.failures.join("\n  ")
        );
        // Every check must actually have RUN — a gate that silently stops
        // checking is worse than no gate.
        assert!(
            rep.checks.len() >= 9,
            "only {} checks ran",
            rep.checks.len()
        );
    }

    // --- V71-E1: the usages verb's flags must reach the wire ---------------
    //
    // The v7.0 defect class was a DECLARED surface with no consumer: five
    // `pane.*` registry rows with no handler, and `review impact` never
    // sending the param its route required. This walks the clap declaration
    // of `kb-code usages` against `usages_request` — the one function that
    // builds what actually goes out — so a flag that is added and then not
    // sent fails the build here rather than silently doing nothing.

    #[test]
    fn usages_verb_sends_every_flag_it_declares() {
        use clap::CommandFactory;
        let cmd = Cli::command();
        let usages = cmd
            .get_subcommands()
            .find(|c| c.get_name() == "usages")
            .expect("`usages` subcommand exists");

        // Every declared arg, and where it must show up.
        //   - `target` is the positional, parsed into path/line/col;
        //   - `daemon`/`json` are transport + output format, not wire params.
        let transport = ["target", "daemon", "json"];
        // A flag id → the query key it must produce (or ROUTE for a flag
        // that selects the route instead of a param).
        const ROUTE: &str = "\u{0}route";
        let expected: &[(&str, &str)] = &[
            ("repo", "repo"),
            ("rev", "ref"),
            ("limit", "limit"),
            ("v2", ROUTE),
        ];

        let declared: Vec<String> = usages
            .get_arguments()
            .map(|a| a.get_id().to_string())
            .filter(|id| !transport.contains(&id.as_str()) && id != "help")
            .collect();
        for id in &declared {
            assert!(
                expected.iter().any(|(arg, _)| arg == id),
                "`kb-code usages --{id}` is declared but this test does not \
                 know where it reaches the wire — add it to `expected` AND to \
                 `usages_request`, or it is dead surface"
            );
        }

        // Every non-default flag must change the request.
        let (base_route, base_q) = usages_request("r", "a.rb", 1, 0, None, None, false);
        assert_eq!(base_route, "/api/usages");
        let keys: Vec<&str> = base_q.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, vec!["repo", "path", "line", "col"]);

        let (v2_route, _) = usages_request("r", "a.rb", 1, 0, None, None, true);
        assert_eq!(v2_route, "/api/usages/2", "--v2 must select the v2 route");

        let (_, with_rev) = usages_request("r", "a.rb", 1, 0, Some("HEAD~1"), Some(3), false);
        for (arg, key) in expected {
            if *key == ROUTE {
                continue;
            }
            assert!(
                with_rev.iter().any(|(k, _)| k == key),
                "`--{arg}` never reaches the wire as `{key}=`"
            );
        }
        // And the values are the ones supplied, not defaults.
        assert!(with_rev.contains(&("ref", "HEAD~1".to_string())));
        assert!(with_rev.contains(&("limit", "3".to_string())));
    }
    // --- V71-D1: the SEARCH flag surface, walked against its dispatch ------
    //
    // The v7.0 defect class was a DECLARED surface with no handler — five
    // `pane.*` registry rows nothing fired, and (this crate's own instance)
    // `review impact` never sending the param its route required. A clap
    // flag is exactly that shape: `#[arg(long)]` makes `--budget` appear in
    // `--help`, in shell completion and in an agent's tool manifest, and
    // does NOTHING until something reads it.
    //
    // This walks the real clap tree of `kb-code search` — every long flag on
    // the verb and on each of its lane subcommands — against the dispatch
    // arm's own source, requiring each one to appear at least TWICE:
    // once where it is bound out of the `Cmd::Search { … }` pattern, and
    // once where it is forwarded into the `*_cmd` call. A flag that is
    // declared and then dropped on the floor fails here, by name.
    //
    // It is a source scan, with a scan's limits (it proves the identifier
    // occurs in the arm, not that the value reaches the wire) — the same
    // trade `kb-code-server`'s `git_argv_lint` and `grammar.rs`'s
    // `every_declared_filter_key_has_a_consumer` already make.
    #[test]
    fn every_search_flag_is_bound_and_forwarded() {
        let src: String = include_str!("main.rs")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let start = src
            .find("Cmd::Search{")
            .expect("the Cmd::Search dispatch arm");
        // The NEXT arm, matched on its own binding — `"Cmd::Transcripts{"`
        // alone would also match `"SearchCmd::Transcripts{"` INSIDE this
        // arm and silently cut the window in half (found by walking this
        // check by hand before trusting it).
        let end = src[start..]
            .find("Cmd::Transcripts{cmd}")
            .map(|i| start + i)
            .expect("the arm that follows Cmd::Search");
        let arm = &src[start..end];

        let cli = <Cli as clap::CommandFactory>::command();
        let search = cli
            .get_subcommands()
            .find(|c| c.get_name() == "search")
            .expect("`kb-code search` exists");

        let mut checked = 0usize;
        let mut check = |flag: &str, whose: &str| {
            let ident = flag.replace('-', "_");
            let occurrences = arm.matches(&format!("{ident},")).count()
                + arm.matches(&format!("{ident})")).count()
                + arm.matches(&format!("{ident}.")).count();
            assert!(
                occurrences >= 2,
                "`--{flag}` on `{whose}` is declared but appears {occurrences}× in the \
                 dispatch arm — a flag must be BOUND out of the pattern and FORWARDED \
                 into the call; one occurrence means it is bound and dropped (the v7.0 \
                 dead-surface defect)",
            );
            checked += 1;
        };

        for arg in search.get_arguments() {
            if let Some(long) = arg.get_long() {
                if long != "help" {
                    check(long, "search");
                }
            }
        }
        for sub in search.get_subcommands() {
            for arg in sub.get_arguments() {
                if let Some(long) = arg.get_long() {
                    if long != "help" {
                        check(long, sub.get_name());
                    }
                }
            }
        }
        // A walk that silently stops walking is worse than no walk: the
        // verb has its own five flags plus five lane subcommands.
        assert!(checked >= 20, "only {checked} search flags were walked");
    }

    #[test]
    fn command_registry_resolver_matches_the_spa_dispatcher() {
        // The same assertions `web-code/src/commands/dispatch.test.ts` makes,
        // in Rust. Two implementations of one algorithm only stay honest if
        // both are pinned to the same expectations.
        use std::collections::BTreeMap;
        let reg = commands::load().unwrap();
        let empty = BTreeMap::new();

        let hit = commands::resolve(&reg, "g d", "reader", &empty, "vim").unwrap();
        assert_eq!(hit.id, "reader.goto-definition");
        assert!(commands::resolve(&reg, "g d", "diff", &empty, "vim").is_none());
        // Global rows resolve from every scope — the promise the sixteen
        // keyboard-inert routes could not make.
        for s in &reg.scopes {
            assert_eq!(
                commands::resolve(&reg, "Meta-k", &s.id, &empty, "vim").map(|c| c.id.as_str()),
                Some("cmd.palette.search"),
                "scope {}",
                s.id
            );
        }
        // Wildcards.
        assert_eq!(
            commands::resolve(&reg, "m q", "reader", &empty, "vim")
                .unwrap()
                .id,
            "mark.set"
        );
        assert_eq!(
            commands::resolve(&reg, "Space 3", "reader", &empty, "vim")
                .unwrap()
                .id,
            "drawer.tab"
        );
        // Presets are COLUMNS: `plain` does not fall back to vim.
        assert_eq!(
            commands::resolve(&reg, "F12", "reader", &empty, "plain")
                .unwrap()
                .id,
            "reader.goto-definition"
        );
        assert!(commands::resolve(&reg, "g d", "reader", &empty, "plain").is_none());
        // The Esc class resolves innermost-first.
        let menu = commands::parse_context(Some("diff.menu"));
        assert_eq!(
            commands::resolve(&reg, "Escape", "diff", &menu, "vim")
                .unwrap()
                .id,
            "dismiss.menu"
        );
        let with_help = commands::parse_context(Some("diff.menu,help.open"));
        assert_eq!(
            commands::resolve(&reg, "Escape", "diff", &with_help, "vim")
                .unwrap()
                .id,
            "dismiss.help"
        );
        // A prefix resolves to nothing but has continuations.
        assert!(commands::resolve(&reg, "g", "reader", &empty, "vim").is_none());
        assert!(!commands::continuations(&reg, "g", "reader", &empty, "vim").is_empty());
    }

    #[test]
    fn command_context_parsing_accepts_the_bare_boolean_form() {
        let ctx = commands::parse_context(Some("help.open, mode=visual ,diff.tour"));
        assert_eq!(ctx.get("help.open").map(String::as_str), Some("true"));
        assert_eq!(ctx.get("mode").map(String::as_str), Some("visual"));
        assert_eq!(ctx.get("diff.tour").map(String::as_str), Some("true"));
        assert!(commands::parse_context(None).is_empty());
        assert!(commands::parse_context(Some("")).is_empty());
    }

    // --- parse_resolve_target (B3) ------------------------------------------

    #[test]
    fn parse_resolve_target_splits_on_the_last_two_colons() {
        assert_eq!(
            parse_resolve_target("src/lib.rs:42:5").unwrap(),
            ("src/lib.rs".to_string(), 42, 5)
        );
    }

    #[test]
    fn parse_resolve_target_keeps_earlier_colons_as_part_of_the_path() {
        // A (rare, but legal on some filesystems) path containing its own
        // colon — only the trailing `:LINE:COL` is split off.
        assert_eq!(
            parse_resolve_target("weird:path.rs:10:3").unwrap(),
            ("weird:path.rs".to_string(), 10, 3)
        );
    }

    #[test]
    fn parse_resolve_target_rejects_missing_col() {
        assert!(parse_resolve_target("src/lib.rs:42").is_err());
    }

    #[test]
    fn parse_resolve_target_rejects_no_colons_at_all() {
        assert!(parse_resolve_target("justapath").is_err());
    }

    #[test]
    fn parse_resolve_target_rejects_non_numeric_line() {
        let err = parse_resolve_target("src/lib.rs:x:5").unwrap_err();
        assert!(format!("{err:?}").contains("invalid line number"));
    }

    #[test]
    fn parse_resolve_target_rejects_non_numeric_col() {
        let err = parse_resolve_target("src/lib.rs:42:y").unwrap_err();
        assert!(format!("{err:?}").contains("invalid col number"));
    }

    #[test]
    fn parse_resolve_target_col_zero_is_valid() {
        // 0-based COL — 0 is the common "start of line" case, not an error.
        assert_eq!(
            parse_resolve_target("src/lib.rs:1:0").unwrap(),
            ("src/lib.rs".to_string(), 1, 0)
        );
    }

    // --- S2-B1: parse_code_actions_target / parse_code_actions_end --------

    #[test]
    fn code_actions_target_accepts_path_line_with_no_col() {
        assert_eq!(
            parse_code_actions_target("src/lib.rs:42").unwrap(),
            ("src/lib.rs".to_string(), 42, 0)
        );
    }

    #[test]
    fn code_actions_target_accepts_path_line_col() {
        assert_eq!(
            parse_code_actions_target("src/lib.rs:42:5").unwrap(),
            ("src/lib.rs".to_string(), 42, 5)
        );
    }

    #[test]
    fn code_actions_target_keeps_earlier_colons_as_part_of_the_path() {
        assert_eq!(
            parse_code_actions_target("weird:path.rs:10:3").unwrap(),
            ("weird:path.rs".to_string(), 10, 3)
        );
    }

    #[test]
    fn code_actions_target_rejects_no_colons_at_all() {
        assert!(parse_code_actions_target("justapath").is_err());
    }

    #[test]
    fn code_actions_target_rejects_non_numeric_line() {
        let err = parse_code_actions_target("src/lib.rs:x").unwrap_err();
        assert!(format!("{err:?}").contains("invalid line number"));
    }

    #[test]
    fn code_actions_target_rejects_non_numeric_col() {
        let err = parse_code_actions_target("src/lib.rs:42:y").unwrap_err();
        assert!(format!("{err:?}").contains("invalid col number"));
    }

    #[test]
    fn code_actions_end_accepts_line_only() {
        assert_eq!(parse_code_actions_end("12").unwrap(), (12, 0));
    }

    #[test]
    fn code_actions_end_accepts_line_and_col() {
        assert_eq!(parse_code_actions_end("12:4").unwrap(), (12, 4));
    }

    #[test]
    fn code_actions_end_rejects_non_numeric() {
        assert!(parse_code_actions_end("x:4").is_err());
        assert!(parse_code_actions_end("12:y").is_err());
    }

    // --- S2-B1: splice_full_lines / code_action_edit_to_batch_op ----------

    fn edit(sl: u32, sc: u32, el: u32, ec: u32, new_text: &str) -> CodeActionEdit {
        CodeActionEdit {
            start_line: sl,
            start_col: sc,
            end_line: el,
            end_col: ec,
            new_text: new_text.to_string(),
        }
    }

    #[test]
    fn splice_full_lines_inserts_at_a_point_without_disturbing_the_rest_of_the_line() {
        let content = "fn main() {\n    let x = 1;\n}\n";
        // Insert "use foo;\n" at the very start of line 1 (a point range).
        let e = edit(1, 0, 1, 0, "use foo;\n");
        let out = splice_full_lines(content, &e).unwrap();
        assert_eq!(out, "use foo;\nfn main() {");
    }

    #[test]
    fn splice_full_lines_replaces_a_mid_line_span_preserving_the_rest_of_the_line() {
        let content = "    let x = 1;\n";
        // Replace columns 8..9 ("x") with "y" — the REST of the line (the
        // declaration's `let `/` = 1;`) must survive untouched.
        let e = edit(1, 8, 1, 9, "y");
        let out = splice_full_lines(content, &e).unwrap();
        assert_eq!(out, "    let y = 1;");
    }

    #[test]
    fn splice_full_lines_spans_multiple_lines() {
        let content = "aaa\nbbb\nccc\n";
        // Replace from col 1 on line 1 through col 2 on line 3.
        let e = edit(1, 1, 3, 2, "XX");
        let out = splice_full_lines(content, &e).unwrap();
        assert_eq!(out, "aXXc");
    }

    #[test]
    fn splice_full_lines_errors_on_an_out_of_range_line() {
        let content = "only one line\n";
        let e = edit(5, 0, 5, 0, "x");
        assert!(splice_full_lines(content, &e).is_err());
    }

    #[test]
    fn splice_full_lines_errors_on_a_zero_line() {
        let content = "x\n";
        let e = edit(0, 0, 1, 0, "x");
        assert!(splice_full_lines(content, &e).is_err());
    }

    #[test]
    fn code_action_edit_to_batch_op_uses_line_anchor_for_a_single_line_edit() {
        let content = "    let x = 1;\n";
        let e = edit(1, 8, 1, 9, "y");
        let op =
            code_action_edit_to_batch_op("Rename variable", "rust-analyzer", "a.rs", &e, content)
                .unwrap();
        assert_eq!(op["op"], "add_comment");
        assert_eq!(op["path"], "a.rs");
        assert_eq!(op["line"], 1);
        assert!(op["line_end"].is_null());
        assert_eq!(
            op["anchor_kind"],
            kb_code_server::annotations::ANCHOR_KIND_LINE
        );
        assert_eq!(op["intent"], "note");
        assert_eq!(op["suggestion"]["replacement"], "    let y = 1;");
        let body = op["body"].as_str().unwrap();
        assert!(body.contains("Quick fix: Rename variable"));
        assert!(body.contains("via rust-analyzer (lsp-live)"));
    }

    #[test]
    fn code_action_edit_to_batch_op_uses_range_anchor_for_a_multi_line_edit() {
        let content = "aaa\nbbb\nccc\n";
        let e = edit(1, 1, 3, 2, "XX");
        let op = code_action_edit_to_batch_op("Fix", "gopls", "a.go", &e, content).unwrap();
        assert_eq!(op["line"], 1);
        assert_eq!(op["line_end"], 3);
        assert_eq!(
            op["anchor_kind"],
            kb_code_server::annotations::ANCHOR_KIND_RANGE
        );
    }

    #[test]
    fn code_action_to_batch_ops_preserves_file_then_edit_order() {
        // Two files, the second with TWO disjoint edits — the flattened op
        // sequence must follow the action's own file/edit order exactly
        // (never grouped, never reordered).
        let action = CodeAction {
            title: "Multi-fix".to_string(),
            kind: "quickfix".to_string(),
            is_preferred: false,
            edits: vec![
                CodeActionFileEditFixture::single("b.rs", edit(1, 0, 1, 0, "// b\n")),
                CodeActionFileEditFixture::multi(
                    "a.rs",
                    vec![edit(1, 0, 1, 0, "// a1\n"), edit(2, 0, 2, 0, "// a2\n")],
                ),
            ],
        };
        let mut files = std::collections::HashMap::new();
        files.insert("a.rs".to_string(), "one\ntwo\n".to_string());
        files.insert("b.rs".to_string(), "hello\n".to_string());

        let ops = code_action_to_batch_ops(&action, "rust-analyzer", &files).unwrap();
        assert_eq!(ops.len(), 3, "one op per (file, edit), never per file");
        assert_eq!(ops[0]["path"], "b.rs");
        assert_eq!(ops[1]["path"], "a.rs");
        assert_eq!(ops[1]["line"], 1);
        assert_eq!(ops[2]["path"], "a.rs");
        assert_eq!(ops[2]["line"], 2);
    }

    #[test]
    fn code_action_to_batch_ops_errors_when_a_touched_file_was_never_fetched() {
        let action = CodeAction {
            title: "Fix".to_string(),
            kind: "quickfix".to_string(),
            is_preferred: false,
            edits: vec![CodeActionFileEditFixture::single(
                "missing.rs",
                edit(1, 0, 1, 0, "x"),
            )],
        };
        let files = std::collections::HashMap::new();
        assert!(code_action_to_batch_ops(&action, "rust-analyzer", &files).is_err());
    }

    /// Tiny builder so the two fixture tests above stay readable —
    /// `kb_code_server::code_actions::CodeActionFileEdit`'s fields are all
    /// `pub`, this just avoids repeating the struct-literal shape twice.
    struct CodeActionFileEditFixture;
    impl CodeActionFileEditFixture {
        fn single(
            path: &str,
            e: CodeActionEdit,
        ) -> kb_code_server::code_actions::CodeActionFileEdit {
            kb_code_server::code_actions::CodeActionFileEdit {
                path: path.to_string(),
                edits: vec![e],
            }
        }
        fn multi(
            path: &str,
            es: Vec<CodeActionEdit>,
        ) -> kb_code_server::code_actions::CodeActionFileEdit {
            kb_code_server::code_actions::CodeActionFileEdit {
                path: path.to_string(),
                edits: es,
            }
        }
    }

    // --- annotations v2 CLI parity (D3) -------------------------------------

    fn parse_annotate(args: &[&str]) -> std::result::Result<Cmd, clap::Error> {
        let mut full = vec!["kb-code", "annotate"];
        full.extend_from_slice(args);
        Cli::try_parse_from(full).map(|cli| cli.cmd)
    }

    fn parse_annotations(args: &[&str]) -> std::result::Result<Cmd, clap::Error> {
        let mut full = vec!["kb-code", "annotations"];
        full.extend_from_slice(args);
        Cli::try_parse_from(full).map(|cli| cli.cmd)
    }

    /// The create form (`target`/`message`/kind flags on `Cmd::Annotate`
    /// itself) still parses with no lifecycle subcommand — the mixed
    /// positional + `#[command(subcommand)]` pattern (same one
    /// `Cmd::Search` already uses for its bare `query`) must resolve a
    /// non-subcommand-name first token to the plain fields, not an error.
    #[test]
    fn annotate_create_form_parses_target_and_message_with_no_subcommand() {
        let cmd = parse_annotate(&["src/lib.rs:42", "-m", "hi", "--repo", "r"]).unwrap();
        match cmd {
            Cmd::Annotate {
                target,
                message,
                repo,
                to,
                symbol,
                sha,
                intent,
                cmd,
                ..
            } => {
                assert_eq!(target.as_deref(), Some("src/lib.rs:42"));
                assert_eq!(message.as_deref(), Some("hi"));
                assert_eq!(repo.as_deref(), Some("r"));
                assert!(cmd.is_none());
                assert!(to.is_none());
                assert!(!symbol);
                assert!(sha.is_none());
                assert!(intent.is_none());
            }
            other => panic!("expected Cmd::Annotate, got {other:?}"),
        }
    }

    /// V70-A3X — `--review`/`--ps`/`--side` on the create form.
    #[test]
    fn annotate_create_form_parses_review_ps_and_side() {
        let cmd = parse_annotate(&[
            "src/lib.rs:42",
            "-m",
            "hi",
            "--repo",
            "r",
            "--review",
            "7",
            "--ps",
            "2",
            "--side",
            "old",
        ])
        .unwrap();
        match cmd {
            Cmd::Annotate {
                review, ps, side, ..
            } => {
                assert_eq!(review, Some(7));
                assert_eq!(ps, Some(2));
                assert_eq!(side.as_deref(), Some("old"));
            }
            other => panic!("expected Cmd::Annotate, got {other:?}"),
        }
    }

    /// `--ps`/`--side` `requires = "review"` — clap-enforced (V70-A3X).
    #[test]
    fn annotate_create_form_ps_without_review_is_rejected() {
        let err =
            parse_annotate(&["src/lib.rs:42", "-m", "hi", "--repo", "r", "--ps", "2"]).unwrap_err();
        assert!(
            err.to_string().contains("review"),
            "expected a clap `requires` error naming --review, got: {err}"
        );
    }

    /// A lifecycle subcommand token (`reply`) dispatches into `AnnotateCmd`
    /// instead, leaving the create-form fields at their `None`/default —
    /// the flip side of the test above.
    #[test]
    fn annotate_reply_subcommand_parses_and_leaves_create_fields_unset() {
        let cmd = parse_annotate(&[
            "reply", "ann_1", "-m", "hi", "--repo", "r", "--path", "lib.rs",
        ])
        .unwrap();
        match cmd {
            Cmd::Annotate {
                target,
                cmd:
                    Some(AnnotateCmd::Reply {
                        id,
                        message,
                        repo,
                        path,
                        intent,
                        ..
                    }),
                ..
            } => {
                assert_eq!(id, "ann_1");
                assert_eq!(message, "hi");
                assert_eq!(repo, "r");
                assert_eq!(path, "lib.rs");
                assert!(intent.is_none());
                assert!(target.is_none(), "the create-form target must stay unset");
            }
            other => panic!("expected Cmd::Annotate{{cmd: Some(Reply..)}}, got {other:?}"),
        }
    }

    #[test]
    fn annotate_set_intent_and_delete_subcommands_parse() {
        let cmd = parse_annotate(&["set-intent", "ann_1", "todo"]).unwrap();
        match cmd {
            Cmd::Annotate {
                cmd: Some(AnnotateCmd::SetIntent { id, intent, .. }),
                ..
            } => {
                assert_eq!(id, "ann_1");
                assert_eq!(intent, "todo");
            }
            other => panic!("expected SetIntent, got {other:?}"),
        }

        let cmd = parse_annotate(&["delete", "ann_1", "--yes"]).unwrap();
        match cmd {
            Cmd::Annotate {
                cmd: Some(AnnotateCmd::Delete { id, yes, .. }),
                ..
            } => {
                assert_eq!(id, "ann_1");
                assert!(yes);
            }
            other => panic!("expected Delete, got {other:?}"),
        }
    }

    #[test]
    fn annotate_kind_flags_to_and_symbol_are_mutually_exclusive() {
        assert!(parse_annotate(&[
            "src/lib.rs:1",
            "-m",
            "x",
            "--repo",
            "r",
            "--to",
            "5",
            "--symbol"
        ])
        .is_err());
    }

    #[test]
    fn annotate_kind_flags_symbol_and_sha_are_mutually_exclusive() {
        assert!(parse_annotate(&[
            "src/lib.rs:1",
            "-m",
            "x",
            "--repo",
            "r",
            "--symbol",
            "--sha",
            "abc123"
        ])
        .is_err());
    }

    #[test]
    fn annotate_kind_flags_to_and_sha_are_mutually_exclusive() {
        assert!(parse_annotate(&[
            "src/lib.rs:1",
            "-m",
            "x",
            "--repo",
            "r",
            "--to",
            "5",
            "--sha",
            "abc123"
        ])
        .is_err());
    }

    #[test]
    fn annotate_each_kind_flag_is_fine_alone() {
        assert!(parse_annotate(&["src/lib.rs:1", "-m", "x", "--repo", "r", "--to", "5"]).is_ok());
        assert!(parse_annotate(&["src/lib.rs:1", "-m", "x", "--repo", "r", "--symbol"]).is_ok());
        assert!(
            parse_annotate(&["src/lib.rs:1", "-m", "x", "--repo", "r", "--sha", "abc123"]).is_ok()
        );
    }

    #[test]
    fn annotations_plain_path_form_still_parses_with_no_subcommand() {
        let cmd = parse_annotations(&["lib.rs", "--repo", "r"]).unwrap();
        match cmd {
            Cmd::Annotations {
                path, repo, cmd, ..
            } => {
                assert_eq!(path.as_deref(), Some("lib.rs"));
                assert_eq!(repo.as_deref(), Some("r"));
                assert!(cmd.is_none());
            }
            other => panic!("expected Cmd::Annotations, got {other:?}"),
        }
    }

    #[test]
    fn annotations_open_subcommand_parses() {
        let cmd = parse_annotations(&[
            "open",
            "--repo",
            "r",
            "--intent",
            "todo",
            "--path-prefix",
            "src/",
        ])
        .unwrap();
        match cmd {
            Cmd::Annotations {
                path,
                cmd:
                    Some(AnnotationsCmd::Open {
                        repo,
                        intent,
                        path_prefix,
                        ..
                    }),
                ..
            } => {
                assert!(path.is_none());
                assert_eq!(repo, "r");
                assert_eq!(intent.as_deref(), Some("todo"));
                assert_eq!(path_prefix.as_deref(), Some("src/"));
            }
            other => panic!("expected Cmd::Annotations{{cmd: Some(Open..)}}, got {other:?}"),
        }
    }

    #[test]
    fn require_valid_intent_accepts_the_full_vocab() {
        for i in ["note", "question", "todo", "flag-for-agent", "tour-stop"] {
            assert!(require_valid_intent(i).is_ok(), "{i} should be accepted");
        }
    }

    #[test]
    fn require_valid_intent_rejects_unknown_values() {
        let err = require_valid_intent("urgent").unwrap_err();
        assert!(format!("{err}").contains("invalid intent"));
    }

    #[test]
    fn anchor_kind_chip_formats_line_range_and_diff() {
        let line = serde_json::json!({"anchor_kind": "line", "line": 10});
        assert_eq!(anchor_kind_chip(&line), "L10");

        let range = serde_json::json!({"anchor_kind": "range", "line": 10, "line_end": 24});
        assert_eq!(anchor_kind_chip(&range), "L10\u{2013}24");

        let diff = serde_json::json!({"anchor_kind": "diff", "line": 2, "sha": "abcdef0123456789"});
        assert_eq!(anchor_kind_chip(&diff), "abcdef01");
    }

    #[test]
    fn anchor_kind_chip_degrades_symbol_to_a_plain_line_marker() {
        // The wire's AnnotationView carries no symbol name/anchor2 payload
        // (`kb_code_server::routes::AnnotationView`) — see the fn's doc.
        let symbol = serde_json::json!({"anchor_kind": "symbol", "line": 7});
        assert_eq!(anchor_kind_chip(&symbol), "L7");
    }

    #[test]
    fn intent_chip_formats_default_and_explicit() {
        assert_eq!(intent_chip(&serde_json::json!({})), "[note]");
        assert_eq!(
            intent_chip(&serde_json::json!({"intent": "todo"})),
            "[todo]"
        );
    }

    #[test]
    fn reply_suffix_pluralizes_correctly() {
        assert_eq!(reply_suffix(0), "");
        assert_eq!(reply_suffix(1), "  (1 reply)");
        assert_eq!(reply_suffix(3), "  (3 replies)");
    }

    #[test]
    fn annotation_api_error_formats_status_and_message() {
        let body = serde_json::json!({"error": "boom"});
        let err = annotation_api_error("do thing", reqwest::StatusCode::BAD_REQUEST, &body);
        let rendered = format!("{err}");
        assert!(rendered.contains("do thing"));
        assert!(rendered.contains("boom"));
        assert!(rendered.contains("400"));
    }

    #[test]
    fn loopback_or_api_error_renders_bare_404_as_requires_loopback() {
        let err = loopback_or_api_error(
            "review verdict",
            "http://example.invalid:9",
            reqwest::StatusCode::NOT_FOUND,
            &serde_json::Value::Null,
        );
        let rendered = format!("{err}");
        assert!(
            rendered.contains("requires loopback"),
            "bare 404 must not stay a bare 404: {rendered}"
        );
        assert!(!rendered.contains("failed (404)"));
    }

    #[test]
    fn loopback_or_api_error_keeps_json_404_as_api_error() {
        let err = loopback_or_api_error(
            "apply suggestion \"ann_x\"",
            "http://127.0.0.1:1",
            reqwest::StatusCode::NOT_FOUND,
            &serde_json::json!({"error": "annotation \"ann_x\""}),
        );
        let rendered = format!("{err}");
        assert!(rendered.contains("annotation \"ann_x\""));
        assert!(!rendered.contains("requires loopback"));
    }

    #[test]
    fn format_apply_drift_caps_each_side_at_20() {
        let expected: String = (1..=25).map(|i| format!("e{i}\n")).collect();
        let found: String = (1..=22).map(|i| format!("f{i}\n")).collect();
        let body = serde_json::json!({
            "error": "working-tree range no longer matches suggestion.original",
            "expected": expected,
            "found": found,
            "resolved_line": 4,
        });
        let rendered = format_apply_drift(&body);
        assert!(rendered.contains("at line 4"));
        assert!(rendered.contains("--- expected"));
        assert!(rendered.contains("--- found"));
        assert!(rendered.contains("e20"));
        assert!(!rendered.contains("e21"));
        assert!(rendered.contains("5 more line(s)"));
        assert!(rendered.contains("2 more line(s)"));
    }

    #[test]
    fn format_review_comment_line_orphaned_and_suggestion() {
        let orphan = serde_json::json!({
            "intent": "note",
            "author": "you",
            "resolved": false,
            "body": "first\nsecond",
            "resolution": {
                "orphaned": true,
                "original": {"ps": 1, "line": 10}
            },
            "suggestion": null,
            "replies": []
        });
        let line = format_review_comment_line(&orphan);
        assert!(line.contains("⚠ orphaned (was ps1:L10)"));
        assert!(line.contains("first"));

        let pending = serde_json::json!({
            "intent": "question",
            "author": "claude",
            "resolved": true,
            "body": "use this",
            "resolution": {"orphaned": false, "line": 3},
            "suggestion": {"replacement": "x", "applied": false},
            "replies": [{"id": "r1"}, {"id": "r2"}]
        });
        let line = format_review_comment_line(&pending);
        assert!(line.contains("L3"));
        assert!(line.contains("resolved"));
        assert!(line.contains("suggestion pending"));
        assert!(line.contains("(2 replies)"));
    }

    #[test]
    fn format_suggest_terms_matches_brief_shape() {
        let branch = serde_json::json!({
            "ahead": 3,
            "behind": 1,
            "suggest": {
                "score": 1.9,
                "terms": {
                    "recency": 0.82,
                    "has_open_review": 1.0,
                    "ahead": 0.2
                }
            }
        });
        assert_eq!(
            format_suggest_terms(&branch).as_deref(),
            Some("recency 0.82 · review · ahead 3")
        );
    }

    fn parse_cli(args: &[&str]) -> std::result::Result<Cmd, clap::Error> {
        let mut full = vec!["kb-code"];
        full.extend_from_slice(args);
        Cli::try_parse_from(full).map(|cli| cli.cmd)
    }

    #[test]
    fn v4_verbs_parse() {
        match parse_cli(&["branches", "--repo", "r", "--sort", "suggested"]).unwrap() {
            Cmd::Branches { sort, .. } => assert_eq!(sort, "suggested"),
            other => panic!("expected Branches, got {other:?}"),
        }
        match parse_cli(&["compare", "main", "topic", "--repo", "r", "--three-dot"]).unwrap() {
            Cmd::Compare {
                from,
                to,
                three_dot,
                ..
            } => {
                assert_eq!(from, "main");
                assert_eq!(to, "topic");
                assert!(three_dot);
            }
            other => panic!("expected Compare, got {other:?}"),
        }
        match parse_cli(&["merge-check", "topic", "--repo", "r"]).unwrap() {
            Cmd::MergeCheck { to, from, .. } => {
                assert_eq!(to, "topic");
                assert!(from.is_none());
            }
            other => panic!("expected MergeCheck, got {other:?}"),
        }
        match parse_cli(&["repo-state", "--repo", "r"]).unwrap() {
            Cmd::RepoState { repo, .. } => assert_eq!(repo, "r"),
            other => panic!("expected RepoState, got {other:?}"),
        }
        match parse_cli(&["review", "comments", "4", "--all", "--ps", "2"]).unwrap() {
            Cmd::Review {
                cmd: ReviewCmd::Comments { id, all, ps, .. },
            } => {
                assert_eq!(id, 4);
                assert!(all);
                assert_eq!(ps.as_deref(), Some("2"));
            }
            other => panic!("expected review comments, got {other:?}"),
        }
        match parse_cli(&["review", "verdict", "4", "approve", "-m", "lgtm"]).unwrap() {
            Cmd::Review {
                cmd:
                    ReviewCmd::Verdict {
                        id,
                        state,
                        note,
                        clear,
                        ..
                    },
            } => {
                assert_eq!(id, 4);
                assert_eq!(state.as_deref(), Some("approve"));
                assert_eq!(note.as_deref(), Some("lgtm"));
                assert!(!clear);
            }
            other => panic!("expected review verdict, got {other:?}"),
        }
        match parse_cli(&["review", "verdict", "4", "--clear"]).unwrap() {
            Cmd::Review {
                cmd: ReviewCmd::Verdict { clear, state, .. },
            } => {
                assert!(clear);
                assert!(state.is_none());
            }
            other => panic!("expected review verdict --clear, got {other:?}"),
        }
        match parse_cli(&["review", "distill", "4", "--json"]).unwrap() {
            Cmd::Review {
                cmd: ReviewCmd::Distill { id, json, .. },
            } => {
                assert_eq!(id, 4);
                assert!(json);
            }
            other => panic!("expected review distill, got {other:?}"),
        }
        match parse_cli(&["suggest", "ann_x", "-m", "fix"]).unwrap() {
            Cmd::Suggest {
                id, message, cmd, ..
            } => {
                assert_eq!(id.as_deref(), Some("ann_x"));
                assert_eq!(message.as_deref(), Some("fix"));
                assert!(cmd.is_none());
            }
            other => panic!("expected suggest PUT, got {other:?}"),
        }
        match parse_cli(&["suggest", "list", "--review", "3"]).unwrap() {
            Cmd::Suggest {
                cmd: Some(SuggestCmd::List { review, .. }),
                ..
            } => assert_eq!(review, 3),
            other => panic!("expected suggest list, got {other:?}"),
        }
        match parse_cli(&["suggest", "apply", "ann_x", "--resolve"]).unwrap() {
            Cmd::Suggest {
                cmd: Some(SuggestCmd::Apply { id, resolve, .. }),
                ..
            } => {
                assert_eq!(id, "ann_x");
                assert!(resolve);
            }
            other => panic!("expected suggest apply, got {other:?}"),
        }
        match parse_cli(&[
            "suggest",
            "apply-batch",
            "ann_a",
            "ann_b",
            "ann_c",
            "--resolve",
        ])
        .unwrap()
        {
            Cmd::Suggest {
                cmd: Some(SuggestCmd::ApplyBatch { ids, resolve, .. }),
                ..
            } => {
                assert_eq!(ids, vec!["ann_a", "ann_b", "ann_c"]);
                assert!(resolve);
            }
            other => panic!("expected suggest apply-batch, got {other:?}"),
        }
        match parse_cli(&["annotate", "batch", "--repo", "r", "--file", "ops.json"]).unwrap() {
            Cmd::Annotate {
                cmd: Some(AnnotateCmd::Batch { repo, file, .. }),
                ..
            } => {
                assert_eq!(repo, "r");
                assert!(file.is_some());
            }
            other => panic!("expected annotate batch, got {other:?}"),
        }
        match parse_cli(&[
            "annotate",
            "watch",
            "--repo",
            "r",
            "--once",
            "--ignore-author",
            "claude",
        ])
        .unwrap()
        {
            Cmd::Annotate {
                cmd:
                    Some(AnnotateCmd::Watch {
                        repo,
                        once,
                        ignore_author,
                        ..
                    }),
                ..
            } => {
                assert_eq!(repo.as_deref(), Some("r"));
                assert!(once);
                assert_eq!(ignore_author, vec!["claude"]);
            }
            other => panic!("expected annotate watch, got {other:?}"),
        }
    }

    // --- reading sets (Phase E3) --------------------------------------------

    #[test]
    fn parse_span_arg_handles_whole_file_single_line_and_range() {
        assert_eq!(
            parse_span_arg("src/lib.rs"),
            ("src/lib.rs".to_string(), None, None)
        );
        assert_eq!(
            parse_span_arg("src/lib.rs:42"),
            ("src/lib.rs".to_string(), Some(42), Some(42))
        );
        assert_eq!(
            parse_span_arg("src/lib.rs:10-20"),
            ("src/lib.rs".to_string(), Some(10), Some(20))
        );
    }

    #[test]
    fn parse_span_arg_treats_an_unparseable_trailing_colon_as_part_of_the_path() {
        // Neither `START-END` nor a bare `LINE` — liberal fallback, the
        // whole string is the path (same posture `parse_path_line`
        // documents for `kb-code why`).
        assert_eq!(
            parse_span_arg("weird:path:not-a-range"),
            ("weird:path:not-a-range".to_string(), None, None)
        );
    }

    fn parse_pack(args: &[&str]) -> std::result::Result<Cmd, clap::Error> {
        let mut full = vec!["kb-code", "pack"];
        full.extend_from_slice(args);
        Cli::try_parse_from(full).map(|cli| cli.cmd)
    }

    #[test]
    fn pack_accepts_either_paths_or_set_but_not_both() {
        let cmd = parse_pack(&["src/lib.rs", "--repo", "r"]).unwrap();
        match cmd {
            Cmd::Pack { paths, set, .. } => {
                assert_eq!(paths, vec!["src/lib.rs".to_string()]);
                assert!(set.is_none());
            }
            other => panic!("expected Cmd::Pack, got {other:?}"),
        }

        let cmd = parse_pack(&["--set", "the-ingest-path", "--repo", "r"]).unwrap();
        match cmd {
            Cmd::Pack { paths, set, .. } => {
                assert!(paths.is_empty());
                assert_eq!(set.as_deref(), Some("the-ingest-path"));
            }
            other => panic!("expected Cmd::Pack, got {other:?}"),
        }

        // clap itself rejects PATHS together with --set (`conflicts_with`).
        assert!(parse_pack(&["src/lib.rs", "--set", "x", "--repo", "r"]).is_err());
    }

    fn parse_set(args: &[&str]) -> std::result::Result<Cmd, clap::Error> {
        let mut full = vec!["kb-code", "set"];
        full.extend_from_slice(args);
        Cli::try_parse_from(full).map(|cli| cli.cmd)
    }

    #[test]
    fn set_create_parses_repeatable_span_flags() {
        let cmd = parse_set(&[
            "create",
            "the ingest path",
            "--repo",
            "r",
            "-d",
            "how it flows",
            "--span",
            "src/lib.rs",
            "--span",
            "src/main.rs:10-20",
        ])
        .unwrap();
        match cmd {
            Cmd::Set {
                cmd:
                    SetCmd::Create {
                        name,
                        repo,
                        description,
                        spans,
                        ..
                    },
            } => {
                assert_eq!(name, "the ingest path");
                assert_eq!(repo, "r");
                assert_eq!(description.as_deref(), Some("how it flows"));
                assert_eq!(
                    spans,
                    vec!["src/lib.rs".to_string(), "src/main.rs:10-20".to_string()]
                );
            }
            other => panic!("expected Cmd::Set{{Create}}, got {other:?}"),
        }
    }

    #[test]
    fn set_from_session_parses_optional_name() {
        let cmd = parse_set(&["from-session", "sess-1", "--repo", "r"]).unwrap();
        match cmd {
            Cmd::Set {
                cmd:
                    SetCmd::FromSession {
                        session_id,
                        repo,
                        name,
                        ..
                    },
            } => {
                assert_eq!(session_id, "sess-1");
                assert_eq!(repo, "r");
                assert!(name.is_none());
            }
            other => panic!("expected Cmd::Set{{FromSession}}, got {other:?}"),
        }
    }

    #[test]
    fn set_from_doc_parses_optional_name() {
        let cmd = parse_set(&["from-doc", "platform", "9f8b7182d433", "--repo", "r"]).unwrap();
        match cmd {
            Cmd::Set {
                cmd:
                    SetCmd::FromDoc {
                        kb,
                        doc,
                        repo,
                        name,
                        ..
                    },
            } => {
                assert_eq!(kb, "platform");
                assert_eq!(doc, "9f8b7182d433");
                assert_eq!(repo, "r");
                assert!(name.is_none());
            }
            other => panic!("expected Cmd::Set{{FromDoc}}, got {other:?}"),
        }

        let cmd = parse_set(&[
            "from-doc",
            "platform",
            "9f8b7182d433",
            "--repo",
            "r",
            "--name",
            "checkout flow",
        ])
        .unwrap();
        match cmd {
            Cmd::Set {
                cmd: SetCmd::FromDoc { name, .. },
            } => assert_eq!(name.as_deref(), Some("checkout flow")),
            other => panic!("expected Cmd::Set{{FromDoc}}, got {other:?}"),
        }
    }

    #[test]
    fn set_add_parses_span_note_and_ref() {
        let cmd = parse_set(&[
            "add",
            "the-ingest-path",
            "src/lib.rs:1-2",
            "--note",
            "look here",
            "--ref",
            "deadbeef",
            "--repo",
            "r",
        ])
        .unwrap();
        match cmd {
            Cmd::Set {
                cmd:
                    SetCmd::Add {
                        name_or_id,
                        span,
                        note,
                        git_ref,
                        repo,
                        ..
                    },
            } => {
                assert_eq!(name_or_id, "the-ingest-path");
                assert_eq!(span, "src/lib.rs:1-2");
                assert_eq!(note.as_deref(), Some("look here"));
                assert_eq!(git_ref.as_deref(), Some("deadbeef"));
                assert_eq!(repo, "r");
            }
            other => panic!("expected Cmd::Set{{Add}}, got {other:?}"),
        }
    }

    // --- S1: `kb-code scip ingest` CLI parsing ------------------------------

    fn parse_scip(args: &[&str]) -> std::result::Result<Cmd, clap::Error> {
        let mut full = vec!["kb-code", "scip"];
        full.extend_from_slice(args);
        Cli::try_parse_from(full).map(|cli| cli.cmd)
    }

    #[test]
    fn scip_ingest_parses_defaults() {
        let cmd = parse_scip(&["ingest", "index.scip", "--repo", "kb"]).unwrap();
        match cmd {
            Cmd::Scip {
                cmd:
                    ScipCmd::Ingest {
                        index,
                        repo,
                        batch_size,
                        daemon,
                        json,
                    },
            } => {
                assert_eq!(index, PathBuf::from("index.scip"));
                assert_eq!(repo, "kb");
                assert_eq!(batch_size, 200);
                assert_eq!(daemon, "http://127.0.0.1:4747");
                assert!(!json);
            }
            other => panic!("expected Cmd::Scip{{Ingest}}, got {other:?}"),
        }
    }

    #[test]
    fn scip_ingest_parses_batch_size_override() {
        let cmd =
            parse_scip(&["ingest", "index.scip", "--repo", "kb", "--batch-size", "50"]).unwrap();
        match cmd {
            Cmd::Scip {
                cmd: ScipCmd::Ingest { batch_size, .. },
            } => assert_eq!(batch_size, 50),
            other => panic!("expected Cmd::Scip{{Ingest}}, got {other:?}"),
        }
    }

    #[test]
    fn scip_ingest_requires_repo() {
        assert!(parse_scip(&["ingest", "index.scip"]).is_err());
    }

    // --- PRR-N12 (N2): `kb-code scip run` CLI parsing -----------------------

    #[test]
    fn scip_run_parses_repo_and_defaults() {
        let cmd = parse_scip(&["run", "--repo", "kb"]).unwrap();
        match cmd {
            Cmd::Scip {
                cmd:
                    ScipCmd::Run {
                        repo,
                        all,
                        dry_run,
                        timeout_secs,
                        daemon,
                        json,
                    },
            } => {
                assert_eq!(repo.as_deref(), Some("kb"));
                assert!(!all);
                assert!(!dry_run);
                assert_eq!(timeout_secs, 600);
                assert_eq!(daemon, "http://127.0.0.1:4747");
                assert!(!json);
            }
            other => panic!("expected Cmd::Scip{{Run}}, got {other:?}"),
        }
    }

    #[test]
    fn scip_run_parses_all_dry_run_and_timeout_override() {
        let cmd = parse_scip(&[
            "run",
            "--all",
            "--dry-run",
            "--timeout-secs",
            "30",
            "--json",
        ])
        .unwrap();
        match cmd {
            Cmd::Scip {
                cmd:
                    ScipCmd::Run {
                        repo,
                        all,
                        dry_run,
                        timeout_secs,
                        json,
                        ..
                    },
            } => {
                assert!(repo.is_none());
                assert!(all);
                assert!(dry_run);
                assert_eq!(timeout_secs, 30);
                assert!(json);
            }
            other => panic!("expected Cmd::Scip{{Run}}, got {other:?}"),
        }
    }

    /// `--repo`/`--all` are both OPTIONAL at the clap layer (their
    /// mutual-exclusivity + "one of them is required" rule is enforced at
    /// runtime in `scip_run_cmd`, not by clap) — this just pins that
    /// `scip run` with neither flag still PARSES.
    #[test]
    fn scip_run_parses_with_neither_repo_nor_all() {
        let cmd = parse_scip(&["run"]).unwrap();
        match cmd {
            Cmd::Scip {
                cmd: ScipCmd::Run { repo, all, .. },
            } => {
                assert!(repo.is_none());
                assert!(!all);
            }
            other => panic!("expected Cmd::Scip{{Run}}, got {other:?}"),
        }
    }

    /// Neither `--repo` nor `--all` — the runtime guard fires BEFORE any
    /// network call (`scip_run_cmd`'s very first statement), so this needs
    /// no live daemon.
    #[tokio::test]
    async fn scip_run_requires_repo_or_all() {
        let err = scip_run_cmd("http://127.0.0.1:0", None, false, false, 600, false)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("--repo NAME or --all"),
            "got: {err}"
        );
    }

    /// Both `--repo` and `--all` — same "fires before any network call"
    /// guarantee as the test above.
    #[tokio::test]
    async fn scip_run_rejects_repo_and_all_together() {
        let err = scip_run_cmd("http://127.0.0.1:0", Some("kb"), true, false, 600, false)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"), "got: {err}");
    }

    #[test]
    fn percent_decode_handles_spaces_and_leaves_malformed_sequences_untouched() {
        assert_eq!(
            percent_decode("/home/user/my%20project"),
            "/home/user/my project"
        );
        assert_eq!(percent_decode("/no/escapes/here"), "/no/escapes/here");
        // A trailing lone `%` (no two hex digits after it) is left as-is.
        assert_eq!(percent_decode("/weird%"), "/weird%");
        assert_eq!(percent_decode("/bad%zz/path"), "/bad%zz/path");
    }

    #[test]
    fn scip_project_root_strips_file_scheme_and_decodes() {
        let mut index = ::scip::types::Index::new();
        index.metadata = protobuf::MessageField::some(::scip::types::Metadata {
            project_root: "file:///home/user/my%20project".to_string(),
            ..Default::default()
        });
        assert_eq!(
            scip_project_root(&index),
            Some(PathBuf::from("/home/user/my project"))
        );
    }

    #[test]
    fn scip_project_root_is_none_when_empty() {
        let index = ::scip::types::Index::new();
        assert_eq!(scip_project_root(&index), None);
    }

    // --- DCB W1.C — `kb-code doclens` --------------------------------------

    fn parse_doclens(args: &[&str]) -> std::result::Result<Cmd, clap::Error> {
        let mut full = vec!["kb-code", "doclens"];
        full.extend_from_slice(args);
        Cli::try_parse_from(full).map(|cli| cli.cmd)
    }

    #[test]
    fn doclens_show_requires_kb_and_doc() {
        assert!(parse_doclens(&["show"]).is_err());
        assert!(parse_doclens(&["show", "--kb", "platform"]).is_err());
        assert!(parse_doclens(&["show", "--doc", "9f8b7182d433"]).is_err());
        assert!(parse_doclens(&["show", "--kb", "platform", "--doc", "9f8b7182d433"]).is_ok());
    }

    /// A checkout is NEVER auto-selected, but the CLI must still let you ASK
    /// without one — the daemon answers `repo_required` and names the
    /// scorecard, which is the honest path.
    #[test]
    fn doclens_show_repo_is_optional() {
        let cmd = parse_doclens(&["show", "--kb", "platform", "--doc", "d1"]).unwrap();
        match cmd {
            Cmd::Doclens {
                cmd: DoclensCmd::Show {
                    repo, group, state, ..
                },
            } => {
                assert!(repo.is_none() && group.is_none() && state.is_none());
            }
            other => panic!("expected doclens show, got {other:?}"),
        }
        assert!(parse_doclens(&["show", "--kb", "k", "--doc", "d", "--repo", "alpha"]).is_ok());
    }

    #[test]
    fn doclens_pin_requires_repo() {
        assert!(parse_doclens(&["pin", "--kb", "platform", "--doc", "d1"]).is_err());
        assert!(
            parse_doclens(&["pin", "--kb", "platform", "--doc", "d1", "--repo", "alpha"]).is_ok()
        );
    }

    #[test]
    fn doclens_repos_requires_kb_and_doc() {
        assert!(parse_doclens(&["repos", "--kb", "platform"]).is_err());
        assert!(parse_doclens(&["repos", "--kb", "platform", "--doc", "d1"]).is_ok());
    }

    /// W2.A — `pins` is the ONE doc-lens verb whose `--kb` is optional: the
    /// question it answers ("what have I pinned") has no single doc to gate
    /// on, so a bare invocation must parse.
    #[test]
    fn doclens_pins_kb_is_optional() {
        let bare = parse_doclens(&["pins"]).unwrap();
        match bare {
            Cmd::Doclens {
                cmd: DoclensCmd::Pins { kb, json, .. },
            } => {
                assert!(kb.is_none());
                assert!(!json);
            }
            other => panic!("expected doclens pins, got {other:?}"),
        }
        let scoped = parse_doclens(&["pins", "--kb", "platform", "--json"]).unwrap();
        match scoped {
            Cmd::Doclens {
                cmd: DoclensCmd::Pins { kb, json, .. },
            } => {
                assert_eq!(kb.as_deref(), Some("platform"));
                assert!(json);
            }
            other => panic!("expected doclens pins, got {other:?}"),
        }
        // `pins` (the ledger) and `pin` (the write) are different verbs and
        // must not be abbreviations of one another.
        assert!(parse_doclens(&["pins", "--repo", "alpha"]).is_err());
    }

    #[test]
    fn doclens_unpin_requires_kb_and_doc() {
        assert!(parse_doclens(&["unpin"]).is_err());
        assert!(parse_doclens(&["unpin", "--kb", "platform"]).is_err());
        assert!(parse_doclens(&["unpin", "--doc", "d1"]).is_err());
        let ok = parse_doclens(&["unpin", "--kb", "platform", "--doc", "d1"]).unwrap();
        match ok {
            Cmd::Doclens {
                cmd: DoclensCmd::Unpin {
                    kb, doc, daemon, ..
                },
            } => {
                assert_eq!(kb, "platform");
                assert_eq!(doc, "d1");
                assert_eq!(daemon, "http://127.0.0.1:4747");
            }
            other => panic!("expected doclens unpin, got {other:?}"),
        }
        // Unlike `pin`, it takes no `--repo`: forgetting is not repo-scoped.
        assert!(parse_doclens(&["unpin", "--kb", "k", "--doc", "d", "--repo", "alpha"]).is_err());
    }

    #[test]
    fn doclens_sync_takes_no_kb_or_doc_and_accepts_force() {
        // DCB W3.A — the sync SET is daemon-side ("every kb with at least one
        // pin"); a `--kb` here would be a second, drifting definition of it.
        let bare = parse_doclens(&["sync"]).unwrap();
        match bare {
            Cmd::Doclens {
                cmd: DoclensCmd::Sync { force, json, .. },
            } => {
                assert!(!force);
                assert!(!json);
            }
            other => panic!("expected doclens sync, got {other:?}"),
        }
        let forced = parse_doclens(&["sync", "--force", "--json"]).unwrap();
        match forced {
            Cmd::Doclens {
                cmd: DoclensCmd::Sync { force, json, .. },
            } => {
                assert!(force);
                assert!(json);
            }
            other => panic!("expected doclens sync, got {other:?}"),
        }
        assert!(parse_doclens(&["sync", "--kb", "platform"]).is_err());
        assert!(parse_doclens(&["sync", "--doc", "d1"]).is_err());
    }

    #[test]
    fn doclens_sync_stats_name_every_non_zero_skip() {
        let body = serde_json::json!({
            "schema": "doclens-sync/1",
            "forced": true,
            "kbs_synced": 2,
            "docs_resolved": 7,
            "docs_skipped_unpinned": 41,
            "docs_skipped_cap": 3,
            "docs_dropped_404": 1,
            "docs_skipped_not_allowlisted": 5,
            "errors": ["research: kb_unreachable: kb down"],
        });
        let out = print_doclens_sync_stats(&body);
        assert!(out.contains("2 kb(s) walked"));
        assert!(out.contains("7 doc(s) resolved"));
        assert!(out.contains("--force: cursors reset"));
        assert!(out.contains("41  unpinned (never fetched)"));
        assert!(out.contains("3  skipped — batch_cap spent"));
        assert!(out.contains("1  dropped — kb 404s the doc"));
        assert!(out.contains("5  skipped — kb not in [doclens] kbs"));
        assert!(out.contains("ERROR  research: kb_unreachable: kb down"));

        // A clean pass prints ONE line — no zero-valued noise.
        let clean = serde_json::json!({
            "forced": false, "kbs_synced": 1, "docs_resolved": 2,
            "docs_skipped_unpinned": 0, "docs_skipped_cap": 0,
            "docs_dropped_404": 0, "docs_skipped_not_allowlisted": 0,
            "errors": [],
        });
        assert_eq!(print_doclens_sync_stats(&clean).lines().count(), 1);
    }

    /// `doclens` and `lenses` are different features answering different
    /// questions; neither may shadow or absorb the other (amendment 14).
    #[test]
    fn doclens_subcommands_do_not_shadow_lenses() {
        let lenses =
            Cli::try_parse_from(["kb-code", "lenses", "src/lib.rs", "--repo", "kb"]).unwrap();
        assert!(matches!(lenses.cmd, Cmd::Lenses { .. }));
        let doclens =
            Cli::try_parse_from(["kb-code", "doclens", "repos", "--kb", "k", "--doc", "d"])
                .unwrap();
        assert!(matches!(doclens.cmd, Cmd::Doclens { .. }));
        // `doclens` takes a SUBCOMMAND — the `lenses <PATH>` positional form
        // must not accidentally parse here.
        assert!(Cli::try_parse_from(["kb-code", "doclens", "src/lib.rs"]).is_err());
    }

    /// The renderer keys on `line_evidence`, not on `line_state` alone —
    /// W2.A's fourth tier (`rev_remap`) must light up without touching this
    /// function again. DCB-W2.A.R fix 6: `rev_remap` only ever ships
    /// `line_state=confirmed` (never `drifted` — that combination cannot be
    /// produced by the resolver), so both reachable combos are pinned here:
    /// a genuine move renders "moved …", and an identity remap (delta 0,
    /// still git-verified) renders plain "confirmed" — parity with the
    /// SPA's `LineBadge`.
    #[test]
    fn doclens_line_cell_renders_all_four_tiers() {
        let cell = |state: &str, evidence: &str, delta: i64| {
            doclens_line_cell(&serde_json::json!({
                "line_state": state,
                "line_evidence": evidence,
                "line_hint_delta": delta,
            }))
        };
        assert_eq!(cell("confirmed", "context_token", 0), "confirmed");
        assert_eq!(cell("drifted", "context_token", 11), "drifted +11");
        assert_eq!(cell("drifted", "context_token", -4), "drifted -4");
        assert_eq!(cell("unverifiable", "none", 0), "unverifiable");
        assert_eq!(cell("absent", "none", 0), "—");
        assert_eq!(cell("confirmed", "rev_remap", 7), "moved +7 (git)");
        assert_eq!(cell("confirmed", "rev_remap", 0), "confirmed");
    }

    #[test]
    fn doclens_path_cell_shows_the_exact_ambiguity_count() {
        let cell = |v: serde_json::Value| doclens_path_cell(&v);
        assert_eq!(
            cell(serde_json::json!({"path_state": "present"})),
            "present"
        );
        assert_eq!(
            cell(serde_json::json!({"path_state": "ambiguous", "candidate_count": 6})),
            "ambig×6"
        );
        assert_eq!(
            cell(serde_json::json!({"path_state": "external"})),
            "external"
        );
        assert_eq!(
            cell(serde_json::json!({"path_state": null, "issue": {"number": 1}})),
            "issue"
        );
        assert_eq!(cell(serde_json::json!({"path_state": null})), "—");
    }

    #[test]
    fn doclens_api_error_names_the_reason_and_offers_the_scorecard() {
        let body = serde_json::json!({
            "error": "no checkout chosen for platform/d1",
            "reason": "repo_required",
        });
        let e = doclens_api_error(reqwest::StatusCode::BAD_REQUEST, &body, "platform", "d1")
            .to_string();
        assert!(e.contains("doclens: repo_required"));
        assert!(e.contains("kb-code doclens repos --kb platform --doc d1"));

        // Every other reason renders without the scorecard hint.
        let body = serde_json::json!({ "error": "kb down", "reason": "kb_unreachable" });
        let e = doclens_api_error(reqwest::StatusCode::SERVICE_UNAVAILABLE, &body, "k", "d")
            .to_string();
        assert!(e.contains("doclens: kb_unreachable — kb down (HTTP 503)"));
        assert!(!e.contains("doclens repos"));
    }

    // --- story attention-gap beat (CT-E2) -----------------------------------

    #[test]
    fn story_gap_line_renders_count_range_and_reason() {
        let e = serde_json::json!({
            "status": "gap",
            "commit_count": 3,
            "first_seen": 1_700_000_000i64,
            "last_seen": 1_700_000_200i64,
            "reason": "no-captured-session",
        });
        assert_eq!(
            story_gap_line(&e),
            "— no captured session for 3 commits (1700000000..1700000200) —"
        );
    }

    #[test]
    fn story_gap_line_singular_commit_collapses_the_range_and_the_plural() {
        let e = serde_json::json!({
            "status": "gap",
            "commit_count": 1,
            "first_seen": 1_700_000_000i64,
            "last_seen": 1_700_000_000i64,
            "reason": "no-captured-session",
        });
        assert_eq!(
            story_gap_line(&e),
            "— no captured session for 1 commit (1700000000) —"
        );
    }

    #[test]
    fn story_gap_line_join_unavailable_names_the_weaker_claim() {
        let e = serde_json::json!({
            "status": "gap",
            "commit_count": 2,
            "first_seen": 100i64,
            "last_seen": 200i64,
            "reason": "join-unavailable",
        });
        assert_eq!(
            story_gap_line(&e),
            "— session join unavailable for 2 commits (100..200) —"
        );
    }

    // --- PRR-R3: review findings {import,list,add} / disposition CLI parity ---

    #[test]
    fn review_findings_import_parses_from_file_and_mode() {
        match parse_cli(&[
            "review",
            "findings",
            "import",
            "4",
            "--from-file",
            "batch.json",
            "--mode",
            "additive",
            "--json",
        ])
        .unwrap()
        {
            Cmd::Review {
                cmd: ReviewCmd::Findings { cmd },
            } => match cmd {
                ReviewFindingsCmd::Import {
                    id,
                    from_file,
                    stdin,
                    mode,
                    json,
                    ..
                } => {
                    assert_eq!(id, 4);
                    assert_eq!(from_file.as_deref(), Some(Path::new("batch.json")));
                    assert!(!stdin);
                    assert_eq!(mode.as_deref(), Some("additive"));
                    assert!(json);
                }
                other => panic!("expected Import, got {other:?}"),
            },
            other => panic!("expected Review{{Findings}}, got {other:?}"),
        }
    }

    #[test]
    fn review_findings_import_parses_stdin() {
        match parse_cli(&["review", "findings", "import", "4", "--stdin"]).unwrap() {
            Cmd::Review {
                cmd:
                    ReviewCmd::Findings {
                        cmd:
                            ReviewFindingsCmd::Import {
                                id,
                                from_file,
                                stdin,
                                ..
                            },
                    },
            } => {
                assert_eq!(id, 4);
                assert!(from_file.is_none());
                assert!(stdin);
            }
            other => panic!("expected Review{{Findings{{Import}}}}, got {other:?}"),
        }
    }

    #[test]
    fn review_findings_list_parses_filters() {
        match parse_cli(&[
            "review",
            "findings",
            "list",
            "4",
            "--ps",
            "2",
            "--disposition",
            "agree",
            "--all",
        ])
        .unwrap()
        {
            Cmd::Review {
                cmd:
                    ReviewCmd::Findings {
                        cmd:
                            ReviewFindingsCmd::List {
                                id,
                                ps,
                                disposition,
                                all,
                                ..
                            },
                    },
            } => {
                assert_eq!(id, 4);
                assert_eq!(ps.as_deref(), Some("2"));
                assert_eq!(disposition.as_deref(), Some("agree"));
                assert!(all);
            }
            other => panic!("expected Review{{Findings{{List}}}}, got {other:?}"),
        }
    }

    #[test]
    fn review_findings_add_parses_line_form() {
        match parse_cli(&[
            "review",
            "findings",
            "add",
            "4",
            "--severity",
            "concern",
            "--category",
            "Style",
            "--path",
            "app/models/order.rb",
            "--line",
            "12",
            "-m",
            "A finding title",
            "--rationale",
            "Because reasons.",
        ])
        .unwrap()
        {
            Cmd::Review {
                cmd:
                    ReviewCmd::Findings {
                        cmd: ReviewFindingsCmd::Add(args),
                    },
            } => {
                let ReviewFindingsAddArgs {
                    id,
                    severity,
                    category,
                    path,
                    line,
                    lines,
                    whole_file,
                    title,
                    rationale,
                    ..
                } = *args;
                assert_eq!(id, 4);
                assert_eq!(severity, "concern");
                assert_eq!(category, "Style");
                assert_eq!(path, "app/models/order.rb");
                assert_eq!(line, Some(12));
                assert!(lines.is_none());
                assert!(!whole_file);
                assert_eq!(title, "A finding title");
                assert_eq!(rationale, "Because reasons.");
            }
            other => panic!("expected Review{{Findings{{Add}}}}, got {other:?}"),
        }
    }

    #[test]
    fn review_findings_add_parses_whole_file_form() {
        match parse_cli(&[
            "review",
            "findings",
            "add",
            "4",
            "--severity",
            "ok",
            "--category",
            "Naming",
            "--path",
            "app/models/order.rb",
            "--whole-file",
            "-m",
            "t",
            "--rationale",
            "r",
        ])
        .unwrap()
        {
            Cmd::Review {
                cmd:
                    ReviewCmd::Findings {
                        cmd: ReviewFindingsCmd::Add(args),
                    },
            } => {
                assert!(args.whole_file);
                assert!(args.line.is_none());
                assert!(args.lines.is_none());
            }
            other => panic!("expected Review{{Findings{{Add}}}}, got {other:?}"),
        }
    }

    #[test]
    fn review_findings_add_parses_evidence_and_evidence_lang() {
        match parse_cli(&[
            "review",
            "findings",
            "add",
            "4",
            "--severity",
            "concern",
            "--category",
            "Style",
            "--path",
            "app/models/order.rb",
            "--line",
            "12",
            "-m",
            "A finding title",
            "--rationale",
            "Because reasons.",
            "--evidence",
            "/tmp/snippet.rb",
            "--evidence-lang",
            "ruby",
        ])
        .unwrap()
        {
            Cmd::Review {
                cmd:
                    ReviewCmd::Findings {
                        cmd: ReviewFindingsCmd::Add(args),
                    },
            } => {
                assert_eq!(args.evidence.as_deref(), Some(Path::new("/tmp/snippet.rb")));
                assert_eq!(args.evidence_lang.as_deref(), Some("ruby"));
            }
            other => panic!("expected Review{{Findings{{Add}}}}, got {other:?}"),
        }
    }

    #[test]
    fn review_findings_add_evidence_lang_without_evidence_is_rejected() {
        // `--evidence-lang` `requires` `--evidence` (clap-enforced) — a
        // caller can't tag evidence that doesn't exist.
        let err = parse_cli(&[
            "review",
            "findings",
            "add",
            "4",
            "--severity",
            "concern",
            "--category",
            "Style",
            "--path",
            "app/models/order.rb",
            "--line",
            "12",
            "-m",
            "A finding title",
            "--rationale",
            "Because reasons.",
            "--evidence-lang",
            "ruby",
        ])
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("evidence"),
            "expected a clap `requires` error naming --evidence, got: {msg}"
        );
    }

    #[test]
    fn review_disposition_parses_set_and_clear() {
        match parse_cli(&[
            "review",
            "disposition",
            "4",
            "f-a",
            "agree",
            "-m",
            "good catch",
        ])
        .unwrap()
        {
            Cmd::Review {
                cmd:
                    ReviewCmd::Disposition {
                        id,
                        slug,
                        action,
                        note,
                        ..
                    },
            } => {
                assert_eq!(id, 4);
                assert_eq!(slug, "f-a");
                assert_eq!(action, "agree");
                assert_eq!(note.as_deref(), Some("good catch"));
            }
            other => panic!("expected Review{{Disposition}}, got {other:?}"),
        }

        match parse_cli(&["review", "disposition", "4", "f-a", "clear"]).unwrap() {
            Cmd::Review {
                cmd: ReviewCmd::Disposition { action, note, .. },
            } => {
                assert_eq!(action, "clear");
                assert!(note.is_none());
            }
            other => panic!("expected Review{{Disposition}}, got {other:?}"),
        }
    }

    /// V70-A3X — `kb-code review set-artifact ID KB DOC_ID`.
    #[test]
    fn review_set_artifact_parses_id_kb_and_doc_id() {
        match parse_cli(&["review", "set-artifact", "4", "platform", "doc123"]).unwrap() {
            Cmd::Review {
                cmd: ReviewCmd::SetArtifact { id, kb, doc_id, .. },
            } => {
                assert_eq!(id, 4);
                assert_eq!(kb, "platform");
                assert_eq!(doc_id, "doc123");
            }
            other => panic!("expected Review{{SetArtifact}}, got {other:?}"),
        }
    }

    /// V70-R — `kb-code review compose ID --from-file FILE`.
    #[test]
    fn review_compose_parses_id_and_from_file() {
        match parse_cli(&["review", "compose", "4", "--from-file", "compose.json"]).unwrap() {
            Cmd::Review {
                cmd:
                    ReviewCmd::Compose {
                        id,
                        from_file,
                        stdin,
                        ..
                    },
            } => {
                assert_eq!(id, 4);
                assert_eq!(from_file.as_deref(), Some(Path::new("compose.json")));
                assert!(!stdin);
            }
            other => panic!("expected Review{{Compose}}, got {other:?}"),
        }
    }

    #[test]
    fn review_pr_status_parses_id_and_json() {
        match parse_cli(&["review", "pr-status", "9", "--json"]).unwrap() {
            Cmd::Review {
                cmd: ReviewCmd::PrStatus { id, json, .. },
            } => {
                assert_eq!(id, 9);
                assert!(json);
            }
            other => panic!("expected Review{{PrStatus}}, got {other:?}"),
        }
    }

    #[test]
    fn review_timeline_parses_id() {
        match parse_cli(&["review", "timeline", "12"]).unwrap() {
            Cmd::Review {
                cmd: ReviewCmd::Timeline { id, json, .. },
            } => {
                assert_eq!(id, 12);
                assert!(!json);
            }
            other => panic!("expected Review{{Timeline}}, got {other:?}"),
        }
    }

    #[test]
    fn review_github_threads_parses_id_and_json_flag() {
        match parse_cli(&["review", "github-threads", "12", "--json"]).unwrap() {
            Cmd::Review {
                cmd: ReviewCmd::GithubThreads { id, json, .. },
            } => {
                assert_eq!(id, 12);
                assert!(json);
            }
            other => panic!("expected Review{{GithubThreads}}, got {other:?}"),
        }
    }

    #[test]
    fn review_inbox_parses_repo_state_and_limit() {
        match parse_cli(&[
            "review", "inbox", "--repo", "widget", "--state", "closed", "--limit", "10",
        ])
        .unwrap()
        {
            Cmd::Review {
                cmd:
                    ReviewCmd::Inbox {
                        repo,
                        all_repos,
                        state,
                        limit,
                        ..
                    },
            } => {
                assert_eq!(repo.as_deref(), Some("widget"));
                assert!(!all_repos);
                assert_eq!(state.as_deref(), Some("closed"));
                assert_eq!(limit, Some(10));
            }
            other => panic!("expected Review{{Inbox}}, got {other:?}"),
        }
    }

    #[test]
    fn review_inbox_parses_all_repos_flag() {
        match parse_cli(&["review", "inbox", "--all-repos"]).unwrap() {
            Cmd::Review {
                cmd: ReviewCmd::Inbox {
                    repo, all_repos, ..
                },
            } => {
                assert!(repo.is_none());
                assert!(all_repos);
            }
            other => panic!("expected Review{{Inbox}}, got {other:?}"),
        }
    }

    // --- PRR-R8/R9: sweep / analytics CLI parsing ----------------------------

    #[test]
    fn review_sweep_parses_repo_and_include_closed() {
        match parse_cli(&[
            "review",
            "sweep",
            "--repo",
            "widget",
            "--include-closed",
            "--json",
        ])
        .unwrap()
        {
            Cmd::Review {
                cmd:
                    ReviewCmd::Sweep {
                        repo,
                        all_repos,
                        include_closed,
                        json,
                        ..
                    },
            } => {
                assert_eq!(repo.as_deref(), Some("widget"));
                assert!(!all_repos);
                assert!(include_closed);
                assert!(json);
            }
            other => panic!("expected Review{{Sweep}}, got {other:?}"),
        }
    }

    #[test]
    fn review_sweep_parses_all_repos_flag() {
        match parse_cli(&["review", "sweep", "--all-repos"]).unwrap() {
            Cmd::Review {
                cmd: ReviewCmd::Sweep {
                    repo, all_repos, ..
                },
            } => {
                assert!(repo.is_none());
                assert!(all_repos);
            }
            other => panic!("expected Review{{Sweep}}, got {other:?}"),
        }
    }

    #[test]
    fn review_analytics_parses_repo_from_and_to() {
        match parse_cli(&[
            "review",
            "analytics",
            "--repo",
            "widget",
            "--from",
            "1000",
            "--to",
            "2000",
        ])
        .unwrap()
        {
            Cmd::Review {
                cmd: ReviewCmd::Analytics { repo, from, to, .. },
            } => {
                assert_eq!(repo.as_deref(), Some("widget"));
                assert_eq!(from, Some(1000));
                assert_eq!(to, Some(2000));
            }
            other => panic!("expected Review{{Analytics}}, got {other:?}"),
        }
    }

    // --- PRR-R5: export-github / publish CLI parsing ------------------------

    #[test]
    fn review_export_github_parses_repeated_finding_and_flags() {
        match parse_cli(&[
            "review",
            "export-github",
            "4",
            "--finding",
            "f-a",
            "--finding",
            "f-b",
            "--include-waived",
            "--include-orphaned-as-general",
            "--json",
        ])
        .unwrap()
        {
            Cmd::Review {
                cmd:
                    ReviewCmd::ExportGithub {
                        id,
                        finding,
                        include_waived,
                        include_orphaned_as_general,
                        json,
                        ..
                    },
            } => {
                assert_eq!(id, 4);
                assert_eq!(finding, vec!["f-a".to_string(), "f-b".to_string()]);
                assert!(include_waived);
                assert!(include_orphaned_as_general);
                assert!(json);
            }
            other => panic!("expected Review{{ExportGithub}}, got {other:?}"),
        }
    }

    #[test]
    fn review_export_github_parses_with_no_findings_named() {
        match parse_cli(&["review", "export-github", "4"]).unwrap() {
            Cmd::Review {
                cmd: ReviewCmd::ExportGithub { id, finding, .. },
            } => {
                assert_eq!(id, 4);
                assert!(finding.is_empty());
            }
            other => panic!("expected Review{{ExportGithub}}, got {other:?}"),
        }
    }

    #[test]
    fn review_publish_parses_finding_form() {
        match parse_cli(&[
            "review",
            "publish",
            "4",
            "f-a",
            "--url",
            "https://x/1",
            "--comment-id",
            "999",
        ])
        .unwrap()
        {
            Cmd::Review {
                cmd:
                    ReviewCmd::Publish {
                        id,
                        slug,
                        verdict,
                        url,
                        comment_id,
                        review_id,
                        ..
                    },
            } => {
                assert_eq!(id, 4);
                assert_eq!(slug.as_deref(), Some("f-a"));
                assert!(!verdict);
                assert_eq!(url, "https://x/1");
                assert_eq!(comment_id.as_deref(), Some("999"));
                assert!(review_id.is_none());
            }
            other => panic!("expected Review{{Publish}}, got {other:?}"),
        }
    }

    #[test]
    fn review_publish_parses_verdict_form() {
        match parse_cli(&[
            "review",
            "publish",
            "4",
            "--verdict",
            "--url",
            "https://x/review-1",
            "--review-id",
            "42",
        ])
        .unwrap()
        {
            Cmd::Review {
                cmd:
                    ReviewCmd::Publish {
                        id,
                        slug,
                        verdict,
                        url,
                        review_id,
                        ..
                    },
            } => {
                assert_eq!(id, 4);
                assert!(slug.is_none());
                assert!(verdict);
                assert_eq!(url, "https://x/review-1");
                assert_eq!(review_id.as_deref(), Some("42"));
            }
            other => panic!("expected Review{{Publish}}, got {other:?}"),
        }
    }

    // ── S2-B2: `kb-code inbox` CLI parsing ──────────────────────────────

    #[test]
    fn inbox_parses_with_defaults() {
        match parse_cli(&["inbox"]).unwrap() {
            Cmd::Inbox {
                daemon,
                json,
                watch,
                interval,
            } => {
                assert_eq!(daemon, "http://127.0.0.1:4747");
                assert!(!json);
                assert!(!watch);
                assert_eq!(interval, 30);
            }
            other => panic!("expected Cmd::Inbox, got {other:?}"),
        }
    }

    #[test]
    fn inbox_parses_watch_and_interval_and_json_and_daemon() {
        match parse_cli(&[
            "inbox",
            "--watch",
            "--interval",
            "5",
            "--json",
            "--daemon",
            "http://example:9",
        ])
        .unwrap()
        {
            Cmd::Inbox {
                daemon,
                json,
                watch,
                interval,
            } => {
                assert_eq!(daemon, "http://example:9");
                assert!(json);
                assert!(watch);
                assert_eq!(interval, 5);
            }
            other => panic!("expected Cmd::Inbox, got {other:?}"),
        }
    }

    // --- V70-A10: `kb-code workspace …` -------------------------------------

    fn parse_workspace(args: &[&str]) -> std::result::Result<Cmd, clap::Error> {
        let mut full = vec!["kb-code", "workspace"];
        full.extend_from_slice(args);
        Cli::try_parse_from(full).map(|cli| cli.cmd)
    }

    #[test]
    fn workspace_list_parses_group_flag() {
        let cmd = parse_workspace(&["list", "--repo", "r", "--group", "ref"]).unwrap();
        match cmd {
            Cmd::Workspace {
                cmd: WorkspaceCmd::List { repo, group, .. },
            } => {
                assert_eq!(repo, "r");
                assert_eq!(group.as_deref(), Some("ref"));
            }
            other => panic!("expected Cmd::Workspace{{List}}, got {other:?}"),
        }

        let cmd = parse_workspace(&["list", "--repo", "r"]).unwrap();
        match cmd {
            Cmd::Workspace {
                cmd: WorkspaceCmd::List { group, .. },
            } => assert!(group.is_none()),
            other => panic!("expected Cmd::Workspace{{List}}, got {other:?}"),
        }
    }

    #[test]
    fn workspace_save_parses_repeatable_file_flags_and_sidecar_fields() {
        let cmd = parse_workspace(&[
            "save",
            "--repo",
            "r",
            "--name",
            "the auth rework",
            "-d",
            "short blurb",
            "--ref",
            "feature/x",
            "--desk-json",
            "{\"v\":1}",
            "--file",
            "src/lib.rs:10",
            "--file",
            "src/main.rs",
        ])
        .unwrap();
        match cmd {
            Cmd::Workspace {
                cmd:
                    WorkspaceCmd::Save {
                        repo,
                        name,
                        description,
                        git_ref,
                        desk_json,
                        files,
                        ..
                    },
            } => {
                assert_eq!(repo, "r");
                assert_eq!(name, "the auth rework");
                assert_eq!(description.as_deref(), Some("short blurb"));
                assert_eq!(git_ref.as_deref(), Some("feature/x"));
                assert_eq!(desk_json.as_deref(), Some("{\"v\":1}"));
                assert_eq!(
                    files,
                    vec!["src/lib.rs:10".to_string(), "src/main.rs".to_string()]
                );
            }
            other => panic!("expected Cmd::Workspace{{Save}}, got {other:?}"),
        }
    }

    #[test]
    fn workspace_open_parses_print_url_flag() {
        let cmd = parse_workspace(&["open", "ws_123", "--repo", "r", "--print-url"]).unwrap();
        match cmd {
            Cmd::Workspace {
                cmd:
                    WorkspaceCmd::Open {
                        name_or_id,
                        repo,
                        print_url,
                        ..
                    },
            } => {
                assert_eq!(name_or_id, "ws_123");
                assert_eq!(repo, "r");
                assert!(print_url);
            }
            other => panic!("expected Cmd::Workspace{{Open}}, got {other:?}"),
        }
    }

    #[test]
    fn workspace_note_add_parses_at_and_reply_to() {
        let cmd = parse_workspace(&[
            "note",
            "add",
            "ws_123",
            "--repo",
            "r",
            "-b",
            "why is this here?",
            "--at",
            "src/lib.rs:42",
        ])
        .unwrap();
        match cmd {
            Cmd::Workspace {
                cmd:
                    WorkspaceCmd::Note {
                        cmd:
                            WorkspaceNoteCmd::Add {
                                name_or_id,
                                repo,
                                body,
                                at,
                                reply_to,
                                ..
                            },
                    },
            } => {
                assert_eq!(name_or_id, "ws_123");
                assert_eq!(repo, "r");
                assert_eq!(body, "why is this here?");
                assert_eq!(at.as_deref(), Some("src/lib.rs:42"));
                assert!(reply_to.is_none());
            }
            other => panic!("expected Cmd::Workspace{{Note{{Add}}}}, got {other:?}"),
        }

        let cmd = parse_workspace(&[
            "note",
            "add",
            "ws_123",
            "--repo",
            "r",
            "-b",
            "+1",
            "--reply-to",
            "ann_abc",
        ])
        .unwrap();
        match cmd {
            Cmd::Workspace {
                cmd:
                    WorkspaceCmd::Note {
                        cmd: WorkspaceNoteCmd::Add { at, reply_to, .. },
                    },
            } => {
                assert!(at.is_none());
                assert_eq!(reply_to.as_deref(), Some("ann_abc"));
            }
            other => panic!("expected Cmd::Workspace{{Note{{Add}}}}, got {other:?}"),
        }
    }

    #[test]
    fn workspace_export_parses_md_flag() {
        let cmd = parse_workspace(&["export", "ws_123", "--repo", "r", "--md"]).unwrap();
        match cmd {
            Cmd::Workspace {
                cmd:
                    WorkspaceCmd::Export {
                        name_or_id,
                        repo,
                        md,
                        ..
                    },
            } => {
                assert_eq!(name_or_id, "ws_123");
                assert_eq!(repo, "r");
                assert!(md);
            }
            other => panic!("expected Cmd::Workspace{{Export}}, got {other:?}"),
        }
    }

    // --- V71-G0: the declaration↔consumer walk ------------------------------
    //
    // The v7.0 defect class was a SILENTLY DEAD surface — a declared thing
    // with nothing behind it. This is the CLI half of the pair (the server
    // half is `kb_code_server::entities`'s own
    // `every_declared_v71_g0_route_is_registered_and_requires_its_params`,
    // which walks the same list against `router.rs`).

    #[test]
    fn cli_requests_send_every_param_their_route_requires() {
        let built = [
            entity_request("repo", "Reseller::Order", None),
            seq_request("repo", None, None),
            // V71-E2 appends its own route to the SAME walk rather than
            // starting a second one: one list of built requests, every
            // declared contract checked against it.
            actions_request("repo", "a.rb", 1, 0, None, None, None, None),
            // V71-F1 — the same walk, one milestone later. A route added to
            // `tree::V71_F1_ROUTES` with no verb building a request for it
            // fails HERE, by path.
            tree_v2_request(&TreeV2Opts {
                repo: "repo",
                ..Default::default()
            }),
            syntax_request(),
            parity_request(),
        ];
        // Rebase note (V71-F1 replayed onto V71-E2): ONE walk over BOTH
        // units' declared contracts — E2's `actions::V71_E2_ROUTES` and
        // F1's `tree::V71_F1_ROUTES` — chained onto V71-G0's, written in
        // F1's `let declared = …` shape because that shape generalises to
        // an arbitrary number of milestones instead of growing the `for`
        // header once per unit. The two failure modes are unchanged and
        // still loud BY NAME: a declared route no verb builds a request
        // for panics naming its PATH, and a required param the CLI omits
        // fails naming the PARAM.
        let declared = kb_code_server::entities::V71_G0_ROUTES
            .iter()
            .chain(kb_code_server::actions::V71_E2_ROUTES.iter())
            .chain(kb_code_server::tree::V71_F1_ROUTES.iter())
            // V72-H1 — same walk, one milestone later. Both routes take
            // no params, so what this proves for them is the OTHER half
            // of the dead-surface rule: a declared route with no verb
            // building a request for it fails here, by path.
            .chain(kb_code_server::syntax::V72_H1_ROUTES.iter());
        for c in declared {
            let (path, query) = built
                .iter()
                .find(|(p, _)| *p == c.path)
                .unwrap_or_else(|| panic!("no kb-code verb builds a request for {}", c.path));
            assert_eq!(*path, c.path);
            for required in c.required_params {
                assert!(
                    query.iter().any(|(k, _)| k == required),
                    "{}: the CLI request omits {required:?}, which the route requires",
                    c.path
                );
            }
        }
    }

    // --- V71-E2: `act`'s own declaration↔consumer walk ---------------------
    //
    // Same shape as `usages_verb_sends_every_flag_it_declares` (V71-E1) and
    // the G0 route walk above: the clap declaration of `kb-code act` is
    // cross-joined against `actions_request`, the ONE function that builds
    // what goes out, so a flag that is declared and then never sent fails
    // here rather than silently doing nothing.

    #[test]
    fn act_verb_sends_every_flag_it_declares() {
        use clap::CommandFactory;
        let cmd = Cli::command();
        let act = cmd
            .get_subcommands()
            .find(|c| c.get_name() == "act")
            .expect("`act` subcommand exists");

        // Args that are transport, output format, or the local
        // resolve/refuse machinery rather than wire params.
        const LOCAL: &str = "\u{0}local";
        let expected: &[(&str, &str)] = &[
            ("repo", "repo"),
            ("list", "path"),
            ("at", "path"),
            ("rev", "ref"),
            ("text", "text"),
            ("target_index", "target"),
            ("id", LOCAL),
            ("confirm", LOCAL),
        ];
        let transport = ["daemon", "json"];
        let declared: Vec<String> = act
            .get_arguments()
            .map(|a| a.get_id().to_string())
            .filter(|id| !transport.contains(&id.as_str()) && id != "help")
            .collect();
        for id in &declared {
            assert!(
                expected.iter().any(|(arg, _)| arg == id),
                "`kb-code act --{id}` is declared but this test does not know \
                 where it reaches the wire — add it to `expected` AND to \
                 `actions_request`, or it is dead surface"
            );
        }
        for (arg, _) in expected {
            assert!(
                declared.iter().any(|d| d == arg),
                "`{arg}` is expected on the wire but is not a declared arg"
            );
        }

        let (route, base) = actions_request("r", "a.rb", 1, 0, None, None, None, None);
        assert_eq!(route, "/api/actions");
        let keys: Vec<&str> = base.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, vec!["repo", "path", "line", "col"]);

        let (_, full) = actions_request(
            "r",
            "a.rb",
            10,
            4,
            Some(20),
            Some("HEAD~1"),
            Some("total"),
            Some(2),
        );
        for key in ["end_line", "ref", "text", "target"] {
            assert!(
                full.iter().any(|(k, _)| *k == key),
                "the optional flag mapping to `{key}=` never reaches the wire"
            );
        }
    }

    /// D5 — `act <stable-id>` never accepts an ordinal.
    #[test]
    fn act_refuses_an_ordinal_id() {
        assert!(act_id_is_ordinal("2"));
        assert!(act_id_is_ordinal("0"));
        assert!(!act_id_is_ordinal("find.usages"));
        assert!(!act_id_is_ordinal("nav.definition"));
        assert!(!act_id_is_ordinal(""));
    }

    #[test]
    fn act_target_grammar_covers_path_line_col_and_range() {
        assert_eq!(
            parse_act_target("app/models/order.rb").unwrap(),
            ("app/models/order.rb".to_string(), 1, 0, None)
        );
        assert_eq!(
            parse_act_target("app/models/order.rb:88").unwrap(),
            ("app/models/order.rb".to_string(), 88, 0, None)
        );
        assert_eq!(
            parse_act_target("app/models/order.rb:88:4").unwrap(),
            ("app/models/order.rb".to_string(), 88, 4, None)
        );
        assert_eq!(
            parse_act_target("app/models/order.rb:88-92").unwrap(),
            ("app/models/order.rb".to_string(), 88, 0, Some(92))
        );
        assert!(
            parse_act_target("a.rb:92-88").is_err(),
            "an inverted range is refused"
        );
    }

    #[test]
    fn act_parses_both_of_its_two_shapes() {
        let cli = Cli::try_parse_from([
            "kb-code",
            "act",
            "--list",
            "a.rb:10:2",
            "--repo",
            "r",
            "--json",
        ])
        .unwrap();
        match cli.cmd {
            Cmd::Act { id, list, at, .. } => {
                assert!(id.is_none());
                assert_eq!(list.as_deref(), Some("a.rb:10:2"));
                assert!(at.is_none());
            }
            other => panic!("expected Cmd::Act, got {other:?}"),
        }
        let cli = Cli::try_parse_from([
            "kb-code",
            "act",
            "find.usages",
            "--at",
            "a.rb:10:2",
            "--repo",
            "r",
        ])
        .unwrap();
        match cli.cmd {
            Cmd::Act {
                id, at, confirm, ..
            } => {
                assert_eq!(id.as_deref(), Some("find.usages"));
                assert_eq!(at.as_deref(), Some("a.rb:10:2"));
                assert!(!confirm);
            }
            other => panic!("expected Cmd::Act, got {other:?}"),
        }
    }

    /// V71-F1's own dead-surface walk, the shape V71-D1 established for
    /// `kb-code search`: every NEW long flag on `kb-code tree` must be
    /// BOUND out of the dispatch pattern AND reach the wire as a query
    /// param. A flag clap accepts and the request builder ignores is the
    /// v7.0 defect ("`review impact` never sending its required param")
    /// exactly.
    #[test]
    fn every_kbc_tree_flag_is_bound_and_forwarded() {
        use clap::CommandFactory;
        const SRC: &str = include_str!("main.rs");
        let cli = Cli::command();
        let tree = cli
            .get_subcommands()
            .find(|c| c.get_name() == "tree")
            .expect("kb-code tree exists");
        let declared: Vec<String> = tree
            .get_arguments()
            .filter_map(|a| a.get_long().map(str::to_string))
            .collect();
        // The pre-V71-F1 flags keep their own (legacy) path.
        let legacy = ["repo", "ref", "json", "daemon", "help"];
        let dispatch = SRC
            .split("Cmd::Tree {")
            .nth(1)
            .expect("the dispatch arm exists");
        for flag in &declared {
            if legacy.contains(&flag.as_str()) {
                continue;
            }
            let binding = flag.replace('-', "_");
            assert!(
                dispatch[..2000].contains(&binding),
                "`kb-code tree --{flag}` is declared but never bound in the dispatch arm"
            );
            // …and it reaches the wire.
            let probe = "probe";
            let opts = match flag.as_str() {
                "view" => TreeV2Opts {
                    repo: "r",
                    view: Some(probe),
                    ..Default::default()
                },
                "scope" => TreeV2Opts {
                    repo: "r",
                    scope: Some(probe),
                    ..Default::default()
                },
                "filter" => TreeV2Opts {
                    repo: "r",
                    filter: Some(probe),
                    ..Default::default()
                },
                "mode" => TreeV2Opts {
                    repo: "r",
                    mode: Some(probe),
                    ..Default::default()
                },
                "decorate" => TreeV2Opts {
                    repo: "r",
                    decorate: Some(probe),
                    ..Default::default()
                },
                "base" => TreeV2Opts {
                    repo: "r",
                    base: Some(probe),
                    ..Default::default()
                },
                "review" => TreeV2Opts {
                    repo: "r",
                    review: Some(7),
                    ..Default::default()
                },
                "depth" => TreeV2Opts {
                    repo: "r",
                    depth: Some(3),
                    ..Default::default()
                },
                "limit" => TreeV2Opts {
                    repo: "r",
                    limit: Some(9),
                    ..Default::default()
                },
                // `--format` is a RENDERER choice, not a wire param, and
                // says so here rather than being silently exempt.
                "format" => continue,
                other => panic!(
                    "`kb-code tree --{other}` is new and this test does not know how to \
                     forward it — add it to `TreeV2Opts` or name it a renderer-only flag"
                ),
            };
            let (_, query) = tree_v2_request(&opts);
            assert!(
                query.iter().any(|(k, _)| *k == flag.as_str()),
                "`--{flag}` is bound but `tree_v2_request` never sends it"
            );
        }
    }

    #[test]
    fn act_requires_a_repo() {
        assert!(Cli::try_parse_from(["kb-code", "act", "--list", "a.rb"]).is_err());
    }

    #[test]
    fn tree_v2_request_sends_only_what_it_was_given() {
        let (path, bare) = tree_v2_request(&TreeV2Opts {
            repo: "r",
            ..Default::default()
        });
        assert_eq!(path, kb_code_server::tree::TREE_V2_ROUTE.path);
        assert_eq!(bare.len(), 1, "a bare request is `repo` alone: {bare:?}");
        // An empty string is not a value — it must not become `?scope=`.
        let (_, empties) = tree_v2_request(&TreeV2Opts {
            repo: "r",
            scope: Some(""),
            filter: Some(""),
            ..Default::default()
        });
        assert_eq!(empties.len(), 1, "{empties:?}");
    }

    #[test]
    fn scope_from_paths_proposals_all_parse_and_end_in_an_enumeration() {
        // The CLI's proposal engine is the SERVER crate's pure one — this
        // asserts the CLI is calling THAT rather than growing its own.
        let props = kb_code_server::tree::scope::propose(&[
            "app/models/order.rb".to_string(),
            "app/models/cart.rb".to_string(),
        ]);
        assert!(!props.is_empty());
        assert!(props.last().expect("non-empty").exact_enumeration);
    }

    #[test]
    fn entity_request_carries_the_optional_worktree_only_when_given() {
        let (_, without) = entity_request("r", "Foo", None);
        assert!(!without.iter().any(|(k, _)| *k == "worktree"));
        let (_, with) = entity_request("r", "Foo", Some("wt1"));
        assert_eq!(
            with.iter()
                .find(|(k, _)| *k == "worktree")
                .map(|(_, v)| v.as_str()),
            Some("wt1")
        );
    }

    #[test]
    fn seq_request_carries_only_the_filters_it_was_given() {
        let (_, bare) = seq_request("r", None, None);
        assert_eq!(bare.len(), 1);
        let (_, filtered) = seq_request("r", Some("boards"), Some("set_1"));
        assert_eq!(filtered.len(), 3);
        assert!(filtered
            .iter()
            .any(|(k, v)| *k == "projection" && v == "boards"));
        assert!(filtered
            .iter()
            .any(|(k, v)| *k == "workspace" && v == "set_1"));
    }

    #[test]
    fn entity_parses_its_name_positionally_and_its_flags() {
        let cli = Cli::try_parse_from([
            "kb-code",
            "entity",
            "Reseller::Order",
            "--repo",
            "r",
            "--worktree",
            "wt1",
            "--json",
        ])
        .unwrap();
        match cli.cmd {
            Cmd::Entity {
                name,
                repo,
                worktree,
                json,
                ..
            } => {
                assert_eq!(name, "Reseller::Order");
                assert_eq!(repo, "r");
                assert_eq!(worktree.as_deref(), Some("wt1"));
                assert!(json);
            }
            other => panic!("expected Cmd::Entity, got {other:?}"),
        }
    }

    #[test]
    fn entity_requires_a_repo() {
        assert!(Cli::try_parse_from(["kb-code", "entity", "Foo"]).is_err());
    }

    #[test]
    fn seq_list_parses_its_two_filters() {
        let cli = Cli::try_parse_from([
            "kb-code",
            "seq",
            "list",
            "--repo",
            "r",
            "--projection",
            "workspace",
            "--workspace",
            "set_abc",
        ])
        .unwrap();
        match cli.cmd {
            Cmd::Seq {
                cmd:
                    SeqCmd::List {
                        repo,
                        projection,
                        workspace,
                        ..
                    },
            } => {
                assert_eq!(repo, "r");
                assert_eq!(projection.as_deref(), Some("workspace"));
                assert_eq!(workspace.as_deref(), Some("set_abc"));
            }
            other => panic!("expected Cmd::Seq{{List}}, got {other:?}"),
        }
    }

    // --- V71-D2: every search output flag writes its kbcq/1 clause --------
    //
    // The v7.0 defect class, CLI edition: a declared flag that never reaches
    // the wire. `--facets`/`--group`/`--explain` are all grammar SUGAR — the
    // only thing they do is append a token to `q=`, so the whole surface is
    // provable without a daemon by walking each flag against the string
    // `build_search_query` actually produces, and re-parsing that string
    // with the daemon's OWN parser (not a second copy of the grammar here).

    #[test]
    fn search_flags_write_the_kbcq_clause_they_promise() {
        use kb_code_server::search::grammar;

        let cases: [(SearchOutputOpts, &str); 3] = [
            (
                SearchOutputOpts {
                    explain: true,
                    ..Default::default()
                },
                "explain",
            ),
            (
                SearchOutputOpts {
                    facets: true,
                    ..Default::default()
                },
                "facets",
            ),
            (
                SearchOutputOpts {
                    group: Some("dir".to_string()),
                    ..Default::default()
                },
                "group",
            ),
        ];
        for (opts, key) in cases {
            let q = build_search_query("needle", &opts);
            assert!(
                q.contains(&format!("{key}:")),
                "the flag for `{key}:` appended nothing — it is a dead flag"
            );
            let p = grammar::parse(&q);
            assert!(
                p.diagnostics.is_empty(),
                "`{q}` did not parse cleanly: {:?}",
                p.diagnostics
            );
            assert_eq!(p.query, "needle", "`{q}` swallowed the query text");
            let landed = match key {
                "explain" => p.explain,
                "facets" => p.facets,
                "group" => p.group == Some(grammar::GroupKey::Dir),
                _ => false,
            };
            assert!(landed, "`{q}` parsed but the daemon read nothing from it");
        }
    }

    /// The author's own token wins over the flag — otherwise
    /// `--group file` on a query that already says `group:dir` would emit a
    /// contradictory pair that the grammar silently resolves last-one-wins.
    #[test]
    fn a_flag_never_overrides_a_clause_the_author_already_wrote() {
        let opts = SearchOutputOpts {
            group: Some("file".to_string()),
            facets: true,
            ..Default::default()
        };
        let q = build_search_query("needle group:dir facets:1", &opts);
        assert_eq!(q, "needle group:dir facets:1");
    }

    #[test]
    fn search_parses_its_two_new_output_flags() {
        let cli = Cli::try_parse_from(["kb-code", "search", "order", "--facets", "--group", "dir"])
            .unwrap();
        match cli.cmd {
            Cmd::Search { facets, group, .. } => {
                assert!(facets);
                assert_eq!(group.as_deref(), Some("dir"));
            }
            other => panic!("expected Cmd::Search, got {other:?}"),
        }
    }
}
