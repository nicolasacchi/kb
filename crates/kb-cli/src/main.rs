//! `kb` — the user-facing command. Subcommands:
//!
//!   kb add <path>                       register a source folder
//!   kb search <q> [opts]                hybrid (default) / keyword / semantic
//!   kb read <id>                        open the artifact in $BROWSER
//!   kb cat <id>                         dump artifact HTML to stdout
//!   kb daemon [--config P]              start the HTTP+SSE daemon
//!   kb backup <kb> [--out P]            tar the kb's state dir
//!   kb status                           daemon /api/stats snapshot (sqlite fallback)
//!   kb model {list, download, set, rm}  embedding-model lifecycle (v0.1)

mod commands;
mod http;
mod session_marker;
mod sse;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

// `version` = git describe (kb-buildstamp's build.rs, PF-B1) — the
// workspace Cargo version is pinned at 0.0.0 forever; tags are the real
// versioning.
#[derive(Parser, Debug)]
#[command(
    name = "kb",
    version = kb_buildstamp::VERSION,
    about = "kb — html artifact daemon CLI"
)]
pub(crate) struct Cli {
    /// Path to kb.toml. Defaults to `~/.config/kb/kb.toml`.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Register a source folder for indexing.
    Add {
        path: PathBuf,
        /// Name the kb (defaults to "default").
        #[arg(long, default_value = "default")]
        kb: String,
        /// D3 — embedding model to write to the new `[kb.*]` section's
        /// `embedding_model` field. Must match an entry from
        /// `kb model list` (e.g. `bge-small-en-v1.5`, `bge-base-en-v1.5`,
        /// `bge-large-en-v1.5`). Omit to leave the field unset — the
        /// daemon's `[defaults]` block (if any) or the registry default
        /// (`bge-small-en-v1.5`) fills in at startup. See
        /// docs/research/foundation/14-embedding-bakeoff-2026-05-19.html
        /// for which model to pick.
        #[arg(long = "embedding-model", value_name = "NAME")]
        embedding_model: Option<String>,
    },
    /// Search for artifacts. Default mode is `hybrid` (BM25 + vector via
    /// RRF k=60). Use `--mode keyword` for BM25-only or `--mode semantic`
    /// for vector-only. Hybrid + semantic require an embedder configured
    /// on the daemon (`kb model set <name> --kb <kb>`).
    Search {
        q: String,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long, default_value = "hybrid")]
        mode: String,
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Filter to an exact `kb-category` (R0 escape hatch). Search
        /// excludes `memory-session` docs by default — pass
        /// `--category memory-session` to surface captured session
        /// transcripts, otherwise invisible to `kb search`.
        #[arg(long)]
        category: Option<String>,
        /// Skip daemon HTTP and read lance directly (read-only). Implies
        /// `--mode keyword` since offline mode has no embedder.
        #[arg(long)]
        offline: bool,
        /// Force HTTP against this daemon URL.
        #[arg(long)]
        daemon: Option<String>,
        /// v0.4 D1 — emit pretty-printed JSON instead of the
        /// human-readable two-line per-hit format.
        #[arg(long)]
        json: bool,
        /// Q-track (board B1) — read-during window: keep only hits whose
        /// artifact was opened (a reading-history 'open' visit) on or
        /// after this bound. Accepts `YYYY-MM-DD` or bare unix seconds.
        /// Requires the daemon (no reading history in `--offline` reads).
        #[arg(long)]
        read_from: Option<String>,
        /// Read-during window upper bound — same accepted formats as
        /// `--read-from`.
        #[arg(long)]
        read_to: Option<String>,
    },
    /// v0.9 M5 — store a memory the agent wants to keep (agent-explicit
    /// capture). Renders an HTML artifact and POSTs it to a memory corpus.
    ///
    /// MI-W3.4 threat model: a memory written from untrusted fetched
    /// content (`--source fetched-web`) persists indefinitely and, once
    /// global, is recallable from every project on this daemon by default.
    /// Passing `--source fetched-web` without an explicit `--global` flips
    /// the default from global to non-global (a one-line stderr notice
    /// marks it whenever it fires) — narrowing the blast radius without
    /// adding a new trust tier (kb still has exactly one: identity is
    /// attribution, not authorization). `--global` always wins when passed.
    Remember {
        /// The memory text (plain). `--title` overrides the derived heading.
        text: String,
        #[arg(long)]
        title: Option<String>,
        /// RA4 — a one-line summary distinct from the title, surfaced on
        /// recall hits and the SPA /memory view as a clean gloss.
        #[arg(long)]
        summary: Option<String>,
        /// Target a memory corpus by name. Overrides `--scope`.
        #[arg(long)]
        kb: Option<String>,
        /// Pick the memory corpus by scope: "global" | "project".
        #[arg(long)]
        scope: Option<String>,
        #[arg(long, default_value = "memory-user")]
        category: String,
        /// Comma-separated tags.
        #[arg(long)]
        tags: Option<String>,
        /// Importance 0..1 (clamped). Higher → ranks higher in recall.
        #[arg(long)]
        salience: Option<f32>,
        /// Recency fade rate: "slow" | "fast".
        #[arg(long)]
        decay: Option<String>,
        /// Artifact id this memory replaces (drops it from recall).
        #[arg(long)]
        supersedes: Option<String>,
        /// MI-W3.3a — optional CoALA-minimal classification: episodic |
        /// semantic | procedural. Absent (the default) leaves the memory
        /// untyped — kb never infers or backfills a type later.
        #[arg(long)]
        r#type: Option<String>,
        /// MI-W3.4 — where the CONTENT originally came from: fetched-web |
        /// user-dictated | agent-inference. SURFACED, NEVER SCORED.
        /// Passing `--source fetched-web` without an explicit `--global`
        /// flips the write's default scope from global to non-global — see
        /// the module docs for the threat model this narrows.
        #[arg(long)]
        source: Option<String>,
        /// CT-C3 — record an approach that was tried and did NOT work — it
        /// will surface on recall with an explicit warning. Writes the
        /// `kb-outcome: failed` meta AND the paired `outcome:failed` tag
        /// (the indexed carrier); the recall hook renders such hits as
        /// "✗ didn't work: <title>". SURFACED, NEVER SCORED — a failed
        /// memory ranks exactly like an ordinary one.
        #[arg(long)]
        failed: bool,
        /// v0.14 S1 — Claude Code session id to stamp on the memory.
        /// Overrides the auto-detected id read from
        /// `~/.cache/kb/current-session` (written by the SessionStart /
        /// UserPromptSubmit hooks). Use this when scripting `kb
        /// remember` outside an agent session.
        #[arg(long)]
        session_id: Option<String>,
        /// v0.14 S1 — opt out of session-id stamping. Skips the
        /// marker-file read AND ignores any `--session-id` override.
        #[arg(long)]
        no_session: bool,
        /// L8 — make the memory recallable from every kb (the V0010
        /// `*` sentinel). Default when neither `--global` nor `--link`
        /// is set, matching the "global by default" CLI ergonomic.
        /// Mutually exclusive with `--link`.
        #[arg(long, conflicts_with = "link")]
        global: bool,
        /// L8 — scope the memory to an explicit comma-separated kb
        /// list. Each name must exist on the daemon (validated server-
        /// side); the resulting memory is NOT global unless you also
        /// pass `--global` (which conflicts and would error). Empty
        /// list = memory invisible to recall (rarely useful).
        #[arg(long, conflicts_with = "global")]
        link: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// v0.9 M5 — recall memories relevant to a query (ranked fan-out
    /// across the in-scope memory corpora).
    Recall {
        query: String,
        /// "auto" (default) | "all" | "global" | "project". B1: `auto`
        /// narrows the daemon-wide fan-out to global corpora + the current
        /// repo's own `memory-<slug>` project corpus (derived from the git
        /// main checkout root's basename — `--cwd` if given, else the
        /// process cwd; see `kb remember`'s same auto-link ladder). Outside
        /// a repo (or with no git), `auto` degrades to exactly `all`. `all`
        /// is the explicit, un-narrowed everything-view — pass it outright
        /// for a true cross-project fan-out (dedup oracles like
        /// `/kb-reflect` must keep doing this on purpose). `global`/
        /// `project` are unchanged.
        #[arg(long, default_value = "auto")]
        scope: String,
        /// Restrict `--scope project` (or an explicit `--scope auto
        /// --project <p>`) to a named corpus.
        #[arg(long)]
        project: Option<String>,
        /// B1 — derive the `--scope auto` project corpus from this
        /// directory instead of the process cwd. Ignored by every other
        /// scope value.
        #[arg(long)]
        cwd: Option<String>,
        #[arg(long, default_value_t = 5)]
        limit: usize,
        /// L8 — return only memories visible to this kb (global or
        /// explicitly linked). Use to scope recall to a single kb's
        /// view, e.g. for testing a per-kb /memory page.
        #[arg(long)]
        for_kb: Option<String>,
        /// Bypass the salience/decay floor so low-salience and decayed
        /// memories also surface. Use as a DEDUP oracle (e.g. `/kb-reflect`):
        /// the floor otherwise hides the very memories a distiller must see
        /// to avoid writing a near-duplicate. Ranking math is unchanged.
        #[arg(long)]
        no_floor: bool,
        /// invariant:10 decomposition — print the per-hit arithmetic behind
        /// `score` (rank → rel, salience, decay/age_days) under each row,
        /// mirroring `kb resurface --explain`. The daemon always computes
        /// these; this only renders what's already on the wire.
        #[arg(long)]
        explain: bool,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// CT-D1 — the ONE context pack for a task: recalled memories (with
    /// their score decomposition), prior-session POINTERS, open comments on
    /// matching artifacts, and the kb-local code-path citations — assembled
    /// server-side, deterministically, under a hard char budget whose every
    /// truncation is reported. This is what `kb-recall.sh`'s turn-1 scent
    /// line ("3 prior sessions · 2 open comments — run `kb context`") points
    /// at: the scent is counts, this verb is the substance.
    Context {
        /// The task text — what you are about to work on.
        query: String,
        /// Your working directory. Not a filter: prior sessions from this
        /// cwd float to the top of the sessions lane and are marked `here`.
        #[arg(long)]
        cwd: Option<String>,
        /// Hard char budget for the whole pack (default 4000, clamped to
        /// 200..=32000). Every drop it causes is reported, never silent.
        #[arg(long)]
        budget: Option<u32>,
        /// YOUR session id — excluded from the sessions lane so the pack
        /// doesn't tell you about yourself (invariant #11 multi-capture).
        #[arg(long)]
        session: Option<String>,
        /// Bypass the salience/decay floor on the memories lane (the same
        /// flag `kb recall --no-floor` carries; ranking math unchanged).
        /// Use as a DEDUP oracle — `/kb-distill` does — since the floor
        /// otherwise hides exactly the decayed memories a distiller must see
        /// to avoid writing a near-duplicate.
        #[arg(long)]
        no_floor: bool,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// R2 — why is a file the way it is? Pulls the past sessions that touched
    /// it (episodic memory) and inlines the prompt / decisions / commits that
    /// produced it. Distinct from `recall` (curated facts): this reconstructs
    /// what actually happened, with provenance and a confidence label.
    Why {
        /// The file to explain (absolute or repo-relative; basename matched).
        path: String,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// CT-B1 — why-memory: the fact → origin session → commits →
    /// files-changed-since chain as one verb. Zero new server surface (pure
    /// composition of existing endpoints) — the CLI's terminal twin of the
    /// SPA's `ProvenanceThread` (MI-W4.6). Degrades honestly at every hop:
    /// no origin session recorded, a purged capture, or an unresolvable git
    /// lookup are all explicit lines, never a silent gap.
    WhyMemory {
        /// The memory's artifact id (the 12-hex id `kb recall`/`kb memory
        /// census` print).
        id: String,
        /// The memory corpus the id lives in. Without it, every
        /// memory-scoped kb on the daemon is searched.
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// R3 — recollect: "has something like this been done?" Semantic search
    /// over past sessions (episodic memory), surfacing each match's recency /
    /// staleness, errors, and commits so you can judge whether to trust it.
    /// Distinct from `recall`: pulled on demand, never asserted as truth.
    Recollect {
        /// Free-text query. Omit when using --similar-to.
        query: Option<String>,
        /// R7 — find sessions similar to THIS session id (its digest is the
        /// query). Mutually exclusive with the positional query.
        #[arg(long, conflicts_with = "query")]
        similar_to: Option<String>,
        /// Restrict to one project folder (basename or full cwd).
        #[arg(long)]
        folder: Option<String>,
        /// W4/W3.A — restrict to one project (a registered `[projects.*]` id,
        /// or a raw project_key). Composes (AND) with --folder.
        #[arg(long)]
        project: Option<String>,
        /// Recency window: day | week | month | year.
        #[arg(long)]
        since: Option<String>,
        #[arg(long, default_value_t = 8)]
        limit: u32,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
        /// W4/R13/Proposal-3 — print the digest excerpt that actually
        /// matched, per hit (the true rank surface, R1).
        #[arg(long)]
        raw: bool,
    },
    /// v0.9 M5 — forget a memory by id. MI-W2.3: soft-forgets by default
    /// (tombstones the artifact in place — `kb-status: forgotten`, still
    /// on disk, still listed by a memory census, dropped from `recall`);
    /// pass `--purge` for the old hard delete (irreversible, no trace).
    Forget {
        id: String,
        /// The memory corpus the id lives in (else the sole configured kb).
        #[arg(long)]
        kb: Option<String>,
        /// MI-W2.3 — hard-delete instead of soft-forgetting: removes the
        /// source file + drops the index row immediately, with no trace
        /// anywhere in kb. Irreversible.
        #[arg(long)]
        purge: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// W2.15b — queue a memory CANDIDATE for human review instead of
    /// writing it directly. This is the agent-layer submit verb: a skill
    /// that wants the proposal-inbox human gate (rather than `kb remember`'s
    /// immediate write) calls this — e.g. a future /kb-distill-style flow
    /// that queues candidates instead of remembering them outright. Approve
    /// via `kb proposals approve <id>`.
    Propose {
        #[arg(long)]
        title: String,
        /// The candidate body (markdown). Pass `-` to read from stdin.
        #[arg(long)]
        body: String,
        /// Target a memory corpus by name (else the sole configured kb).
        #[arg(long)]
        kb: Option<String>,
        /// Comma-separated tags.
        #[arg(long)]
        tags: Option<String>,
        /// Make the eventual memory recallable from every kb. Default when
        /// neither `--global` nor `--link` is set (mirrors `kb remember`).
        #[arg(long, conflicts_with = "link")]
        global: bool,
        /// Scope the eventual memory to an explicit comma-separated kb list.
        #[arg(long, conflicts_with = "global")]
        link: Option<String>,
        /// Importance 0..1 (clamped).
        #[arg(long)]
        salience: Option<f32>,
        /// Claude Code session id this candidate originated from.
        #[arg(long)]
        session_id: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// W2.15b — the proposal inbox: list/approve/reject queued memory
    /// candidates. Approving fires the EXACT memory-write path `kb remember`
    /// uses, carrying the candidate's provenance (`session_id` etc) along —
    /// the human gate the daemon's no-in-daemon-LLM invariant requires.
    Proposals {
        #[command(subcommand)]
        action: Option<ProposalsAction>,
    },
    /// MI-W2.4a — memory lineage. `kb memory log <id>` walks one supersede
    /// chain in both directions (what it supersedes, what superseded it)
    /// with timestamps and forgotten-state, rendered as a timeline.
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },
    /// SL3 — `kb slate`: this project's shared working state (who is on
    /// what, open questions, hypotheses, dead ends). NOT memory: a slate is
    /// per-project, mutable through later posts, and read by sessions of
    /// every harness. `kb slate open` first; `take` before touching a path
    /// another session may be on. Design: docs/research/kb-slate-design-2026-09.html.
    Slate {
        #[command(flatten)]
        common: SlateCommon,
        #[command(subcommand)]
        action: SlateAction,
    },
    /// Embedding-model lifecycle (v0.1). Manages the XDG cache and
    /// per-kb model selection in kb.toml.
    Model {
        #[command(subcommand)]
        action: ModelAction,
    },
    /// Open the artifact in $BROWSER (xdg-open). Auto-detects the
    /// daemon (GC-B5 / roadmap G17): daemon reachable → opens the served
    /// SPA permalink; unreachable → falls back to the local file path.
    Read {
        id: String,
        #[arg(long)]
        kb: Option<String>,
        /// Skip daemon HTTP and open the local lance-resolved file
        /// directly (read-only, previous behaviour).
        #[arg(long)]
        offline: bool,
        /// Force HTTP against this daemon URL.
        #[arg(long)]
        daemon: Option<String>,
        /// Skip recording this read into `history` (default: recorded
        /// when served via a daemon).
        #[arg(long)]
        no_record: bool,
        /// Record this read into `history` even when served from local
        /// lance (`--offline` or daemon-unreachable fallback); errors if
        /// no daemon is reachable to store it.
        #[arg(long)]
        record: bool,
    },
    /// Dump artifact HTML to stdout. Auto-detects the daemon (GC-B5 /
    /// roadmap G17): daemon reachable → `GET /api/kb/{kb}/artifact/{id}`;
    /// unreachable → falls back to a local read-only lance open.
    Cat {
        id: String,
        #[arg(long)]
        kb: Option<String>,
        /// Skip daemon HTTP and read lance directly (read-only, previous
        /// behaviour).
        #[arg(long)]
        offline: bool,
        /// Force HTTP against this daemon URL.
        #[arg(long)]
        daemon: Option<String>,
        /// Skip recording this read into `history` (default: recorded
        /// when served via a daemon).
        #[arg(long)]
        no_record: bool,
        /// Record this read into `history` even when served from local
        /// lance (`--offline` or daemon-unreachable fallback); errors if
        /// no daemon is reachable to store it.
        #[arg(long)]
        record: bool,
    },
    /// U3 (v0.25 quick capture) — stage file(s) into a kb's `capture/`
    /// folder, provenance-stamped, via `POST /api/kb/{kb}/capture`. With no
    /// FILES, `--url`/`--text` writes a url-stub `.md` capture instead (the
    /// daemon never fetches the URL).
    Capture {
        /// One or more local file paths, or a single `-` to read one
        /// document from stdin (filename stem from `--name`, always
        /// written as `.md`). Omit entirely for a `--url`/`--text`-only
        /// stub capture.
        files: Vec<String>,
        #[arg(long)]
        kb: Option<String>,
        /// Title stamped into the capture's provenance metadata, and used
        /// as the url/text stub's heading.
        #[arg(long)]
        title: Option<String>,
        /// Comma-separated tags, slugified and stamped alongside
        /// `source:upload, from:cli`.
        #[arg(long)]
        tags: Option<String>,
        /// Opt-in HTML sanitize (ammonia) at capture time. No-op for
        /// Markdown captures (U1: sanitize is HTML-pipeline-only, v1).
        #[arg(long)]
        sanitize: bool,
        /// Shared/saved-page URL. With no FILES, writes a url-stub `.md`
        /// capture; with FILES, stamped as `kb-capture-url` provenance.
        #[arg(long)]
        url: Option<String>,
        /// Shared text, folded into a url/text stub's body.
        #[arg(long)]
        text: Option<String>,
        /// Filename stem for a stdin (`-`) capture. Default "capture".
        #[arg(long)]
        name: Option<String>,
        /// Force HTTP against this daemon URL.
        #[arg(long)]
        daemon: Option<String>,
        /// Emit the raw JSON response instead of the human summary. Only
        /// recognised value: "json".
        #[arg(long)]
        output: Option<String>,
    },
    /// Ephemeral LLM↔human handoff (`POST /api/kb/{kb}/desk` and friends).
    /// Offer a draft, wait for comments, re-push the same path, expire.
    Desk {
        #[command(subcommand)]
        action: DeskAction,
    },
    /// Start the HTTP+SSE daemon in this process. With no subcommand:
    /// run in the foreground. `kb daemon stop` signals a running
    /// daemon via its pid file and waits for graceful exit.
    Daemon {
        #[command(subcommand)]
        action: Option<DaemonAction>,
    },
    /// Inspect / validate / edit the resolved kb.toml on disk. (The web
    /// Settings → Config tab does LIVE edits + a daemon restart; this is
    /// file-direct, also handy as a CI / pre-commit gate.)
    Config {
        #[command(subcommand)]
        action: Option<ConfigAction>,
    },
    /// Atomic snapshot of a kb's state directory.
    Backup {
        kb: String,
        /// Output tarball path (defaults to <state>/exports/<kb>-<timestamp>.tar.gz).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Restore a `kb backup` tarball into a kb's state directory. Stop the
    /// daemon for that kb first; refuses a non-empty state unless --force.
    Restore {
        /// Path to the .tar.gz produced by `kb backup`.
        tarball: PathBuf,
        /// Knowledge base to restore into.
        #[arg(long)]
        kb: String,
        /// Replace an existing non-empty state dir (wipes it first).
        #[arg(long)]
        force: bool,
    },
    /// Print sqlite-backed observability snapshot. When a daemon is
    /// reachable, lists its kbs from `/api/stats` even if this host has
    /// no local kb.toml (docker / remote daemon).
    Status {
        /// v0.4 D1 — emit JSON instead of the human-readable layout.
        #[arg(long)]
        json: bool,
        /// Repeat the sweep every N seconds (clears the screen between
        /// renders). Mutually exclusive with `--json`.
        #[arg(long, value_name = "SECS")]
        watch: Option<u64>,
        /// Force HTTP against this daemon URL (default 127.0.0.1:4000;
        /// `KB_DAEMON_URL` is the env fallback).
        #[arg(long)]
        daemon: Option<String>,
    },
    /// v0.38 CT-C6 — read-only diagnostics distinct from `kb daemon doctor`
    /// (which probes the daemon's own HTTP health). `--hooks` is the first
    /// mode: the provenance-chain integrity check. Walks the fragile chain
    /// of session marker files → kb-memory plugin hooks → the git
    /// `Kb-Session` trailer hook → the `memory_recalls` ledger → kb-code's
    /// why-hook, printing PASS/WARN/SKIP + a one-line fix per link so a
    /// broken link is a 30-second diagnosis instead of a silent no-op.
    Doctor {
        /// Provenance-chain / hooks integrity check. The only mode today
        /// (more may follow) — required so a bare `kb doctor` doesn't look
        /// silently broken.
        #[arg(long)]
        hooks: bool,
        /// Repo to check the git trailer hook + repo-keyed session marker
        /// against. Defaults to the current directory.
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Force HTTP against this daemon URL (default 127.0.0.1:4000).
        #[arg(long)]
        daemon: Option<String>,
        /// Emit `{checks: [...], notes: [...]}` instead of the
        /// human-readable PASS/WARN/SKIP report.
        #[arg(long)]
        json: bool,
        /// D30 (v0.42) — remove `~/.cache/kb/slate-cursor-*` and
        /// `slate-topic-*` markers older than 30 days. Scoped to that ONE
        /// check; every other `--hooks` check stays read-only.
        #[arg(long)]
        fix: bool,
    },
    /// TM-track — print the daemon's request + pipeline timing snapshot
    /// (`GET /api/metrics`). The coarse per-route latency table is always
    /// shown; the search-stage / per-kb / pipeline tables appear only when
    /// the daemon runs with `[server] metrics = true`.
    Metrics {
        /// Force HTTP against this daemon URL (default 127.0.0.1:4000).
        #[arg(long)]
        daemon: Option<String>,
        /// Emit the raw JSON snapshot instead of the human-readable tables.
        #[arg(long)]
        json: bool,
    },
    /// Whoami — print the daemon-resolved identity for this request
    /// (`GET /api/identity`). Human form is `user (source)`.
    Whoami {
        /// Force HTTP against this daemon URL (default 127.0.0.1:4000).
        #[arg(long)]
        daemon: Option<String>,
        /// Emit the raw JSON response. Only recognised value: "json".
        #[arg(long)]
        output: Option<String>,
    },
    /// Users — list configured ∪ observed users (`GET /api/users`).
    Users {
        /// Force HTTP against this daemon URL (default 127.0.0.1:4000).
        #[arg(long)]
        daemon: Option<String>,
        /// Emit the raw JSON response. Only recognised value: "json".
        #[arg(long)]
        output: Option<String>,
    },
    /// History — list per-kb activity history (`GET /api/kb/{kb}/history`).
    History {
        #[arg(long)]
        kb: Option<String>,
        /// Filter by kind: open | search | comment | all (default all).
        #[arg(long)]
        kind: Option<String>,
        /// Filter by attribution username (`?user=`).
        #[arg(long)]
        user: Option<String>,
        /// Cap the number of entries (daemon default 200, max 1000).
        #[arg(long)]
        limit: Option<u32>,
        /// Force HTTP against this daemon URL (default 127.0.0.1:4000).
        #[arg(long)]
        daemon: Option<String>,
        /// Emit the raw JSON response instead of the human table.
        #[arg(long)]
        json: bool,
    },
    /// Comments — read kb-comments/1 review files (v0.2).
    Comments {
        #[command(subcommand)]
        action: CommentsAction,
    },
    /// Sessions — captured Claude Code transcripts (v0.14 Track S).
    /// `kb sessions list` prints the daemon's session enrichment rows;
    /// `kb sessions show <session_id>` includes the produced memories
    /// + touched artifacts.
    Sessions {
        #[command(subcommand)]
        action: SessionsAction,
    },
    /// Import — backfill external data into a kb corpus (filesystem-only; the
    /// daemon's watcher does the indexing). `kb import claude-history` wraps
    /// every historical Claude Code transcript under `~/.claude/projects` in
    /// the same envelope the live capture hook produces and drops it in a
    /// sessions corpus, so months of past sessions become episodic memory.
    Import {
        #[command(subcommand)]
        action: ImportAction,
    },
    /// Reading — RP-track reading-progress for an artifact: how far it was
    /// read, what was read vs skimmed, where the reader stopped, and which
    /// sections held their attention. `<target>` is a 12-hex id, a
    /// source-relative path, or a unique filename. `--lite` = whole-page
    /// only; `--json` for Claude Code (consult before revising a doc).
    Reading {
        /// 12-hex artifact id, source-relative path, or unique filename.
        target: String,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        json: bool,
        /// Whole-page summary only (skip the per-section breakdown).
        #[arg(long)]
        lite: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Resurface — pull-only queue of artifacts worth picking back up:
    /// open (unresolved) comments + unfinished reads, deterministically
    /// scored with reasons on every item. Nothing pushes, nothing nags;
    /// acting on an item clears it and idle reads fade out on their own.
    Resurface {
        #[arg(long)]
        kb: Option<String>,
        /// Max items (1-50).
        #[arg(long, default_value_t = 8)]
        limit: u32,
        /// Print the scoring arithmetic under each item.
        #[arg(long)]
        explain: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Timeline — four synchronized, day-bucketed lanes over one UTC-day
    /// axis: artifacts created, artifacts read, work sessions captured, and
    /// comments raised. A pure view (`GET /api/kb/{kb}/timeline`, C-a); no
    /// streak, no best-day, no goal, no `--follow`.
    Timeline {
        #[arg(long)]
        kb: Option<String>,
        /// Window lower bound. Accepts `YYYY-MM-DD` or bare unix seconds.
        /// Defaults to `--to` minus 365 days (server-side default).
        #[arg(long)]
        from: Option<String>,
        /// Window upper bound — same accepted formats as `--from`.
        /// Defaults to now.
        #[arg(long)]
        to: Option<String>,
        /// Restrict to a csv subset of `created,read,session,comment`
        /// (any order); defaults to all four.
        #[arg(long)]
        tracks: Option<String>,
        /// Dump the resolved artifact-id set (one per line) for the
        /// selected tracks instead of rendering the sparklines.
        #[arg(long)]
        ids: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Daycard — the CLI parity twin of the e-ink desk radiator (`GET
    /// /api/kb/{kb}/daycard`, Unit 2): a deterministic "day at a glance"
    /// digest (worth-picking-back-up + today's activity + a couple of
    /// recent/never-opened artifacts). Pull-only; no streak, no goal, no
    /// `--watch`. `--html` prints the exact bytes an e-ink panel would fetch.
    /// `--since <WHEN>` (CT-E1) switches to "what happened while I was
    /// away" instead — sessions/memories/artifacts/comments over a rolling
    /// window; mutually exclusive with `--day`.
    Daycard {
        #[arg(long)]
        kb: Option<String>,
        /// `YYYY-MM-DD`, UTC. Defaults to today. Mutually exclusive with
        /// `--since`.
        #[arg(long, conflicts_with = "since")]
        day: Option<String>,
        /// CT-E1 — "what happened while I was away": unix seconds or a bare
        /// `YYYY-MM-DD` UTC date. No relative forms (`3d`/`12h`) yet —
        /// mutually exclusive with `--day`.
        #[arg(long, conflicts_with = "day")]
        since: Option<String>,
        /// Print the raw HTML+inline-SVG document instead of the text digest.
        #[arg(long)]
        html: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Reading lists — multiple named, ordered lists per kb whose
    /// entries target a whole artifact or a §section of it. Read state
    /// is derived from your reading progress (override with
    /// `update --read/--unread`). `import`/`export` speak the portable
    /// kb-list/1 Markdown/JSON document.
    List {
        #[command(subcommand)]
        action: ListAction,
    },
    /// Boards v1 — a JSON Canvas (jsoncanvas.org) geometry sidecar for a
    /// reading list. `kb board <list>` prints the canvas; `kb board set
    /// <list> --file <path|->` replaces it. Geometry lives in the corpus
    /// as `boards/<list_id>.canvas` — a sidecar, NOT part of the
    /// kb-list/1 document (`kb list import`/`export` never touch it).
    Board {
        #[command(subcommand)]
        action: BoardAction,
    },
    /// Notes — free-standing notes / todo-lists attached to a kb or a
    /// folder within it. A note is a Markdown artifact (`kb-category=note`),
    /// so it's searchable + commentable like any artifact; these verbs add
    /// checklist editing (check/uncheck/append) on top.
    Notes {
        #[command(subcommand)]
        action: NotesAction,
    },
    /// Links — CT-F3 unlinked mentions: "the graph you wrote is half the
    /// graph you meant." `suggest` lists docs whose prose names another
    /// artifact's exact title or unique basename with no link edge to show
    /// for it (derived per request, never stored); `apply` authors one of
    /// those suggestions as a real `[[wikilink]]` in the Markdown source.
    /// A source that cannot carry a wikilink (an HTML artifact, a memory
    /// body — invariant #29) is listed with its reason and refused by
    /// `apply`, never rewritten.
    Links {
        #[command(subcommand)]
        action: LinksAction,
    },
    /// Backlinks — what references THIS artifact (notes that `[[wikilink]]` it,
    /// or other artifacts that link it). Works on any artifact, not just notes.
    Backlinks {
        /// Artifact: 12-hex id, source-relative path, or unique filename.
        target: String,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Refs — the code references extracted from an artifact's own bytes
    /// (paths, `path:line`, `Namespace::Class`, `Class#method`, gem paths,
    /// GitHub issues). kb reports HINTS only: it has no working tree and no
    /// symbol index, so nothing here says whether a path exists or a line
    /// still holds — that is kb-code's `doc-lens` (invariant #2/#4).
    ///
    /// `--lint` reports refs that were INFERRED rather than declared with
    /// `<code data-kb-ref="…">`, with the attribute you would paste to
    /// declare each — the authoring loop this convention asks for.
    ///
    /// With no <TARGET>, walks the whole corpus feed (requires --kb).
    ///
    /// `--by-target <PATH>` flips to the reverse lookup instead: every doc
    /// citing that exact path (`?by_target=` on the feed route, CT-B3) —
    /// "who cites config/importmap.rb". Conflicts with <TARGET>.
    Refs {
        /// Artifact: 12-hex id, source-relative path, or unique filename.
        /// Omit to walk the whole corpus.
        #[arg(conflicts_with = "by_target")]
        target: Option<String>,
        /// Reverse lookup: every doc whose extracted refs cite this exact
        /// path (`path_hint` match). Conflicts with <TARGET>.
        #[arg(long = "by-target", value_name = "PATH")]
        by_target: Option<String>,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        json: bool,
        /// Report inferred-but-undeclared refs (+ the `data-kb-ref` to paste).
        #[arg(long)]
        lint: bool,
        /// Corpus-walk page size (server clamps to 1..=100).
        #[arg(long, default_value_t = 25)]
        limit: u32,
        /// CT-B3 — print a ready `?ids=` gallery URL (invariant #35) over
        /// the resolved doc-id set instead of (or alongside, with --json)
        /// the normal report. Degrades LOUDLY over the 500-id cap: prints
        /// a warning to stderr and omits the URL rather than emitting a
        /// link the gallery would 400 on.
        #[arg(long)]
        gallery: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Versions — list an artifact's version timeline (git commits, kb
    /// index snapshots, and the working tree), per the kb's `versions`
    /// mode (auto|git|index|both|off).
    Versions {
        /// Artifact: 12-hex id, source-relative path, or unique filename.
        target: String,
        /// CT-F6 (RFC 7089 Memento) — resolve the timeline AT an instant:
        /// which version of THIS artifact stood at that moment. Same date
        /// grammar as `kb diff --between` (`YYYY-MM-DD` = end of that day
        /// UTC; a full RFC 3339 timestamp is exact). The answer is the
        /// NEAREST version at or before the instant — the resolved row is
        /// marked `→` and the header line says whether the hit was exact or
        /// merely nearest-prior. A date older than the oldest known version
        /// says so and names that floor; it NEVER falls back to the oldest
        /// version. Per-artifact only — not a corpus timeline.
        #[arg(long, value_name = "DATE")]
        at: Option<String>,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Diff — what changed between two versions of an artifact. Defaults
    /// to "most recent prior version → working tree". HTML diffs the
    /// rendered prose (markup-agnostic); `--raw` diffs the source bytes.
    Diff {
        /// Artifact: 12-hex id, source-relative path, or unique filename.
        target: String,
        /// Older side: a git sha, `index:<id>`, or `WORKING`. Defaults to
        /// the version just before `--to`. Conflicts with `--between`.
        #[arg(long, conflicts_with = "between")]
        from: Option<String>,
        /// Newer side. Defaults to the working tree (current file).
        /// Conflicts with `--between`.
        #[arg(long, conflicts_with = "between")]
        to: Option<String>,
        /// MI-W2.4b (2026-07 temporal-query design) — resolve BOTH sides
        /// from calendar dates / RFC 3339 instants instead of explicit
        /// refs: for each date, the nearest version AT OR BEFORE it (a
        /// pure `kb_core::versions::resolve_as_of` walk over the same
        /// timeline `kb versions` lists — zero server delta). `YYYY-MM-DD`
        /// means end of that day, 23:59:59 UTC ("as of June 5" = after
        /// June 5's changes); a full RFC 3339 timestamp is exact. A date
        /// older than the oldest known version is a hard error naming the
        /// oldest version's own date — never a silent empty diff.
        #[arg(long, num_args = 2, value_names = ["FROM_DATE", "TO_DATE"])]
        between: Option<Vec<String>>,
        /// Diff the raw source bytes instead of rendered prose.
        #[arg(long)]
        raw: bool,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Prompt — read an artifact's stored generation prompt (the
    /// `<template id="kb-prompt">` bundle it was authored with, 8 KiB-capped
    /// at index time). LOCAL-RENDER ONLY on a strip-configured corpus: a
    /// remote daemon (`--daemon http://...`) whose kb has `[kb.*.outbound]
    /// strip_kb_prompt = true` withholds the text just like it would for a
    /// browser — an honest `stripped` flag, never a silent 404. A loopback
    /// daemon (the default) always serves it verbatim.
    Prompt {
        /// Artifact id (the 12-hex canonical id `kb docs`/`kb search` print).
        id: String,
        #[arg(long)]
        kb: Option<String>,
        /// Print only the prompt text (nothing when withheld/absent) —
        /// script-friendly, like `kb cat`'s bytes-only stdout.
        #[arg(long, conflicts_with = "json")]
        raw: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Share — publish an artifact/folder to an OAuth-gated static host
    /// (Cloudflare Pages + Access) or a public one (GitHub Pages).
    /// `kb share <target>` deploys; `kb share list` / `kb share revoke
    /// <name>` manage existing shares.
    Share(ShareArgs),
    /// Atlas — manage the per-kb 2-D layout (v0.3).
    Atlas {
        #[command(subcommand)]
        action: AtlasAction,
    },
    /// Token — bearer-token lifecycle for v0.4 self-host.
    Token {
        #[command(subcommand)]
        action: TokenAction,
    },
    /// Push — tail /api/events as Claude-Code-friendly markdown blocks.
    /// Reconnects on disconnect with Last-Event-ID + exponential backoff.
    Push {
        /// Filter by event kind (repeatable). Default: print every event.
        #[arg(long)]
        filter: Vec<String>,
        /// Force HTTP against this daemon URL (default 127.0.0.1:4000).
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Events — operator tail of /api/events, one line per event
    /// (v0.24 T1, the TUI EVENTS tab replacement). Same reconnect loop
    /// as `kb push` (Last-Event-ID resume + exponential backoff), but
    /// filters run SERVER-SIDE: `--types` globs plus the `--kb` /
    /// `--artifact` payload filters.
    Events {
        /// Follow the stream (required — the only mode today; a one-shot
        /// ring dump may drop the requirement later).
        #[arg(long, required = true)]
        follow: bool,
        /// Event-type glob(s), e.g. `index.*` or `error,query`
        /// (repeatable and/or comma-separated). Default: every type.
        #[arg(long)]
        types: Vec<String>,
        /// Only events whose payload `kb` equals this kb name.
        #[arg(long)]
        kb: Option<String>,
        /// Only events referencing this artifact id (12-hex).
        #[arg(long)]
        artifact: Option<String>,
        /// Emit NDJSON envelopes ({id, type, ts, v, payload}) instead of
        /// the human `id  type  payload` lines.
        #[arg(long)]
        json: bool,
        /// Force HTTP against this daemon URL (default 127.0.0.1:4000).
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Fleet — cross-daemon coverage report (Q4). Reads
    /// `~/.config/kb/daemons.toml` and compares each daemon's artifact
    /// set for a given kb.
    Fleet {
        #[command(subcommand)]
        action: FleetAction,
    },
    /// Pull — one-shot fetch of every artifact in a remote kb that's
    /// missing from a local folder, written as `<id>.html` for the
    /// local daemon's watcher to ingest. For a kb behind Authelia, pass
    /// `--oidc-token-url` + `--oidc-client-id` (and set
    /// `KB_OIDC_CLIENT_SECRET` in the env) to authenticate via the
    /// client_credentials grant; otherwise the local kb token is used.
    Pull {
        /// Remote kb base URL, e.g. https://kb.example.com.
        #[arg(long)]
        from: String,
        /// Remote kb name to pull from.
        #[arg(long)]
        kb: String,
        /// Local folder to write `<id>.html` into — typically the
        /// watched source dir of a local kb. Created if absent.
        #[arg(long)]
        into: PathBuf,
        /// OIDC token endpoint for the client_credentials grant, e.g.
        /// https://auth.example.com/api/oidc/token. Requires
        /// `--oidc-client-id` and the `KB_OIDC_CLIENT_SECRET` env var.
        #[arg(long)]
        oidc_token_url: Option<String>,
        /// OIDC client id (e.g. kb-bot). Pairs with `--oidc-token-url`.
        #[arg(long)]
        oidc_client_id: Option<String>,
        /// OAuth scope to request. Authelia's bearer-authz scope is the
        /// default.
        #[arg(long, default_value = "authelia.bearer.authz")]
        scope: String,
    },
    /// Get — fetch a single artifact's metadata or HTML body via the daemon.
    Get {
        id: String,
        #[arg(long)]
        kb: Option<String>,
        /// Output format: json (full metadata), md (one-pager), html
        /// (raw artifact bytes). Default: json.
        #[arg(long, default_value = "json")]
        format: String,
        #[arg(long)]
        daemon: Option<String>,
        /// Skip recording this read into `history` (default: recorded —
        /// `get` is always daemon-served).
        #[arg(long)]
        no_record: bool,
    },
    /// Download an artifact's raw source, or a folder / whole kb as a
    /// `.zip`, via the daemon. Streams to stdout (pipe-friendly) unless
    /// `-o FILE` is given; refuses to write a `.zip` to a terminal.
    Download {
        /// Artifact id or source-relative path/filename. Omit when using
        /// --folder or --all.
        target: Option<String>,
        /// Zip every artifact under this source-relative folder
        /// (descendant-inclusive).
        #[arg(long, conflicts_with_all = ["target", "all"])]
        folder: Option<String>,
        /// Zip every artifact in the kb.
        #[arg(long, conflicts_with_all = ["target", "folder"])]
        all: bool,
        #[arg(long)]
        kb: Option<String>,
        /// Write to this file instead of stdout.
        #[arg(short = 'o', long)]
        out: Option<PathBuf>,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Graph — deterministic corpus graph report: hubs by in-degree,
    /// orphans (never linked AND never opened), dead-edge link-rot, and
    /// dangling/ambiguous wikilinks re-resolved over Markdown sources
    /// (GET /api/kb/{kb}/graph/report, GS-track).
    Graph {
        /// Corpus name (e.g. `research`).
        kb: String,
        /// Hubs-list cap (server clamps to 1..=100).
        #[arg(long, default_value_t = 10)]
        top: usize,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// SLO — CT-F5 corpus-health indicators over EXISTING tables: code-ref
    /// path shape, orphan `kb_session` docs, recall-ledger parse failures,
    /// and capture freshness. Targets come from `[kb.<name>.slo]` in
    /// kb.toml; every key is optional, and an unconfigured indicator is
    /// still MEASURED (its status just reads `unknown` for want of
    /// something to judge it against).
    ///
    /// SURFACED, NEVER ENFORCED — nothing changes behaviour on a missed
    /// target, and `status` exits 0 even on a warn (see the module docs for
    /// why this is deliberately not a check command).
    Slo {
        #[command(subcommand)]
        action: SloAction,
    },
    /// Queries — GC-B3, surface zero-hit search queries as a
    /// corpus-gap signal (grouped by normalized text, occurrence
    /// counts). `--scope one` (default) reads this kb's ring
    /// (`--kb` required unless the daemon serves exactly one);
    /// `--scope all` fans out server-side across every kb on the
    /// daemon (invariant #28). W3 C-c: `list`/`save`/`rm` close the CLI
    /// parity gap on the daemon-wide `/api/saved-queries` store (the same
    /// store the SPA's saved-query ribbon and the reflection canvas's
    /// "save as scene" chip both write to — a scene IS a saved query,
    /// see `commands::queries` doc comment). Modelled on `AtlasAction::Field`:
    /// the bare zero-hit report has no positional, so `action` is a plain
    /// `Option<_>` living beside it — no `external_subcommand` catch-all
    /// needed.
    Queries {
        #[command(subcommand)]
        action: Option<QueriesAction>,
        #[arg(long)]
        kb: Option<String>,
        /// "one" (default, this kb only) | "all" (every kb on the daemon).
        #[arg(long, default_value = "one")]
        scope: String,
        /// The only supported report today — the recent-queries raw
        /// list already exists via the SPA; this flag is required so a
        /// bare `kb queries` doesn't look silently broken.
        #[arg(long = "zero-hit")]
        zero_hit: bool,
        /// Drop groups seen fewer than this many times. Default 1 (no
        /// filter beyond "occurred at least once").
        #[arg(long = "min-count", default_value_t = 1)]
        min_count: u64,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Related — print outbound link graph for an artifact (uses
    /// /api/kb/{kb}/graph/{id}?depth=N from v0.3 F2).
    Related {
        id: String,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long, default_value_t = 1)]
        depth: u32,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Similar — true (embedding-space) nearest neighbors for an artifact
    /// (uses /api/kb/{kb}/atlas/similar/{id}?limit=N, W2.3a). Distinct from
    /// `kb related`'s link graph — this is vector-space cosine similarity.
    Similar {
        id: String,
        #[arg(long)]
        kb: Option<String>,
        /// Neighbor count. Server default 8, capped at 24.
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Tools — emit a Claude-prompt-friendly markdown manifest of every
    /// kb subcommand (synopsis + description + example). Drop into a
    /// system prompt to teach Claude how to drive kb.
    Tools,
    /// Reindex — force the daemon to re-walk a kb's source folder and
    /// re-emit `watch.modify` for every HTML file (bypasses the
    /// content-hash dedup gate via `force=true`). Reach for this when
    /// the SPA / popover is missing files — usually inotify dropped
    /// events under a burst and the next reconcile tick hasn't run.
    Reindex {
        /// kb name. Defaults to the only configured kb when there's
        /// just one; required when there's >1.
        #[arg(long)]
        kb: Option<String>,
        /// Force HTTP against this daemon URL.
        #[arg(long)]
        daemon: Option<String>,
        /// Emit JSON instead of the human-readable line.
        #[arg(long)]
        json: bool,
    },
    /// Exclude — per-file index exclusion (v0.24). An excluded file is
    /// removed from the index via the keep-user-data cascade (comments +
    /// reading history survive) and ignored by every ingest path until
    /// re-included with `--rm` (which reindexes it immediately, comments
    /// re-anchoring from the preserved sidecar). `--list` shows the
    /// current exclusions.
    Exclude {
        /// What to exclude: 12-hex id, source-relative path, or unique
        /// filename suffix. A path-shaped target that matches no indexed
        /// artifact is excluded verbatim (pre-emptive exclusion). Optional
        /// with --list.
        target: Option<String>,
        /// kb name. Defaults to the only configured kb when there's
        /// just one; required when there's >1.
        #[arg(long)]
        kb: Option<String>,
        /// Re-include a previously excluded file (resolved against the
        /// exclusion list, since the index no longer knows it).
        #[arg(long)]
        rm: bool,
        /// List current exclusions instead of mutating.
        #[arg(long)]
        list: bool,
        /// Optional operator note stored with the exclusion ("why").
        #[arg(long)]
        note: Option<String>,
        /// Force HTTP against this daemon URL.
        #[arg(long)]
        daemon: Option<String>,
        /// Emit the raw daemon response JSON.
        #[arg(long)]
        json: bool,
    },
    /// Pause — stop a kb source's ingest (watcher, reconcile walk, and
    /// reindex nudges are all gated) until `kb resume`. Enforced since
    /// v0.24 (D6) — a paused source genuinely goes stale by design.
    Pause {
        /// kb name. Defaults to the only configured kb when there's
        /// just one; required when there's >1.
        #[arg(long)]
        kb: Option<String>,
        /// Force HTTP against this daemon URL.
        #[arg(long)]
        daemon: Option<String>,
        /// Emit the raw daemon response JSON.
        #[arg(long)]
        json: bool,
    },
    /// Resume — undo `kb pause`; ingest re-enables live (the next
    /// reconcile pass catches anything that changed while paused).
    Resume {
        /// kb name. Defaults to the only configured kb when there's
        /// just one; required when there's >1.
        #[arg(long)]
        kb: Option<String>,
        /// Force HTTP against this daemon URL.
        #[arg(long)]
        daemon: Option<String>,
        /// Emit the raw daemon response JSON.
        #[arg(long)]
        json: bool,
    },
    /// Compact — run lance maintenance on the kb's dataset: merge small
    /// data fragments, rebuild indices, prune old manifest versions.
    /// Reach for this when search latency has crept up after many
    /// reindex cycles (each upsert commits a fragment + manifest, so
    /// thousands of small writes shred the dataset). Synchronous; the
    /// daemon's startup heuristic also covers the common case on
    /// restart.
    Compact {
        /// kb name. Defaults to the only configured kb when there's
        /// just one; required when there's >1.
        #[arg(long)]
        kb: Option<String>,
        /// Force HTTP against this daemon URL.
        #[arg(long)]
        daemon: Option<String>,
        /// Emit the raw daemon response JSON instead of the
        /// human-readable two-line summary.
        #[arg(long)]
        json: bool,
    },
    /// Find — resolve a 12-hex id, source-relative path, or unique
    /// filename suffix to an artifact id via the daemon's
    /// `/api/kb/{kb}/lookup` endpoint. Prints the id on success; exits
    /// 1 with candidates on ambiguity, exits 2 on no match. Composes
    /// (`kb find atlas.html | xargs kb cat`).
    Find {
        /// What to resolve: 12-hex id, `folder/file.html`, or a unique
        /// basename like `atlas.html`.
        input: String,
        /// kb name. Defaults to the only configured kb when there's
        /// just one; required when there's >1.
        #[arg(long)]
        kb: Option<String>,
        /// Emit the full hit JSON (id, path, source_relative, folder,
        /// title) instead of just the id.
        #[arg(long)]
        json: bool,
        /// Force HTTP against this daemon URL.
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Move/rename an artifact (or an indexed folder prefix) without
    /// losing identity-keyed state (comments, lists, history, …).
    /// Resolves `<TARGET>` via lookup; if it names a folder and not an
    /// artifact, renames the folder. Trailing slash on `<TARGET>` forces
    /// folder mode. Ambiguity (artifact AND folder) errors.
    Mv {
        /// Artifact id / source-rel / unique filename, or folder prefix.
        target: String,
        /// New source-relative path (file) or folder prefix (folder mode).
        new_path: String,
        #[arg(long)]
        kb: Option<String>,
        /// Emit the raw daemon JSON response.
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// New — scaffold an HTML artifact from a template. Substitutes
    /// `{{title}}`, `{{date}}`, `{{slug}}`, and any `--var key=value`
    /// placeholders, writes to `--out` or stdout. `--template` is
    /// either a path (`./templates/idea.html`) or a short name
    /// resolved against `[kb.<name>.templates]` in kb.toml.
    New {
        /// Path to a template HTML file, OR a short name configured
        /// under `[kb.<name>.templates]` in kb.toml.
        #[arg(long)]
        template: String,
        /// Title for the new artifact. Substituted as `{{title}}`;
        /// also slugified into `{{slug}}`.
        #[arg(long)]
        title: String,
        /// Output path. Stdout if absent. Parent directories are
        /// created on demand.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Extra placeholder values, `key=value` form (repeatable).
        /// Unknown keys in the template are left intact; missing keys
        /// emit a stderr note listing them.
        #[arg(long = "var")]
        vars: Vec<String>,
        /// Kb name for template-name lookup against
        /// `[kb.<name>.templates]`. Defaults to the sole configured
        /// kb when there's exactly one.
        #[arg(long)]
        kb: Option<String>,
    },
    /// IndexPage — generate a self-contained HTML index of a kb's
    /// artifacts, grouped by status/category/severity. Designed to
    /// replace hand-maintained INDEX.md ledgers; output is itself a
    /// kb artifact (carries `<meta name="kb-category"
    /// content="index-page">`).
    IndexPage {
        /// Knowledge base to index.
        #[arg(long)]
        kb: String,
        /// Filter rows by `key=value` (repeatable). Recognised keys:
        /// `kb-category`, `kb-status`, `kb-severity`, `tag`,
        /// `longread`. Multiple filters AND together.
        #[arg(long = "filter")]
        filters: Vec<String>,
        /// Group rows by `kb-status` (default), `kb-category`, or
        /// `kb-severity`. Rows whose value is unset fall into the
        /// "(unset)" bucket.
        #[arg(long = "group-by", default_value = "kb-status")]
        group_by: String,
        /// Output path. Stdout if absent. Parent directories are
        /// created on demand.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Optional template HTML. Supports `{{title}}`,
        /// `{{generated_at}}`, `{{group_by}}`, `{{kb}}`, and
        /// `{{groups}}` (the rendered body). Without a template, kb
        /// emits a built-in styled shell.
        #[arg(long)]
        template: Option<PathBuf>,
        /// Force HTTP against this daemon URL.
        #[arg(long)]
        daemon: Option<String>,
        /// Max rows to fetch from `/api/kb/{kb}/docs`. Defaults to
        /// 500 — bump for very large corpora.
        #[arg(long, default_value_t = 500)]
        limit: u32,
        /// Override the auto-derived page title.
        #[arg(long)]
        title: Option<String>,
    },
    /// Reset — wipe a kb's index state (Lance + SQLite) so the next
    /// daemon start reindexes from scratch. Preserves `.review/`
    /// comments by default; pass `--all` to drop them too. Refuses
    /// to run when it sees the daemon up unless `--force` is given.
    Reset {
        /// Knowledge base to reset.
        #[arg(long)]
        kb: String,
        /// Skip the interactive confirmation prompt.
        #[arg(long)]
        yes: bool,
        /// Also delete `.review/` (user-authored comments).
        #[arg(long)]
        all: bool,
        /// Override the running-daemon safety check.
        #[arg(long)]
        force: bool,
    },
    /// Synth — generate a directory of synthetic HTML artifacts for
    /// stress-testing the daemon at scale (S-milestone S8). Output is
    /// a corpus-shaped tree (changelog/, ideas/, incidents/, ...) the
    /// indexer can ingest as-is. Determinism: identical `--seed`
    /// values produce byte-identical corpora.
    Synth {
        /// Number of HTML files to generate.
        #[arg(long, default_value_t = 1000)]
        docs: u32,
        /// Output directory. Created if missing. Must be empty or absent
        /// to keep the output deterministic.
        #[arg(long)]
        out: PathBuf,
        /// RNG seed. Same seed = byte-identical corpus.
        #[arg(long, default_value_t = 0x5e7d)]
        seed: u64,
    },
    /// Bench — retrieval-quality bake-off across embedding models.
    /// Scaffolds query sets, discovers candidate-relevant artifact ids,
    /// and (in C2) drives the daemon to compute Recall@k / MRR / nDCG
    /// per (corpus, model, mode). One subcommand per stage.
    Bench {
        #[command(subcommand)]
        action: BenchAction,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum DaemonAction {
    /// Stop a running daemon via the pid file at
    /// `<XDG_STATE_HOME>/kb/<daemon-name>/kb-daemon.pid`. Sends
    /// SIGTERM and waits up to 10 s for the process to exit. Exits
    /// non-zero if no pid file is found, the pid is stale, or the
    /// daemon doesn't exit in time.
    Stop,
    /// P5: pokes the daemon's HTTP API and prints a green/yellow/red
    /// health report. Checks identity reachability, kb configuration,
    /// open errors per kb, stats responsiveness, the embedder/semantic
    /// path (one tiny semantic search per kb — catches a wedged or
    /// missing onnxruntime), and the event bus. Exits 0 if HEALTHY,
    /// 1 if any check failed.
    Doctor {
        /// Daemon endpoint to check. Defaults to http://127.0.0.1:4000.
        #[arg(long)]
        endpoint: Option<String>,
        /// Emit a single JSON object instead of human-readable output.
        #[arg(long)]
        json: bool,
        /// 4: continuously re-run the checks every N seconds, clearing
        /// the screen between renders. Ctrl+C to exit. Ignored when
        /// --json is set (scripted callers want one-shot output).
        #[arg(long, value_name = "SECS")]
        watch: Option<u64>,
    },
    /// L2 — read or set the daemon's FILE log level (the ndjson layer's
    /// EnvFilter) at runtime via GET/PUT /api/log-level. No FILTER reads
    /// the current one; with FILTER (`debug`, `info,kb_core=debug`, …)
    /// the flip is live — no restart. The stderr layer (RUST_LOG) is
    /// untouched; the boot default comes from KB_LOG_FILE_LEVEL.
    LogLevel {
        /// EnvFilter directives to set (omit to read the current filter).
        filter: Option<String>,
        /// Daemon endpoint. Defaults to http://127.0.0.1:4000.
        #[arg(long)]
        endpoint: Option<String>,
        /// Emit the raw JSON response.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum ConfigAction {
    /// Print the resolved config path + the parsed config as TOML.
    Show,
    /// Validate the config; exits non-zero on hard errors (bad addr /
    /// trusted-proxy IP, uncompilable redaction regex, zero rate-limit).
    /// Warnings (missing kb dir, unknown model) don't fail.
    Validate,
    /// Open `$VISUAL`/`$EDITOR` on the config file, then validate it.
    /// Applies after a daemon restart (or use the web Config tab).
    Edit,
}

#[derive(Subcommand, Debug, Clone)]
enum TokenAction {
    /// Generate a new token. Refuses to overwrite an existing file
    /// (use `kb token rotate` for that). Writes mode 0600.
    Generate,
    /// Overwrite the existing token (or create if absent). Daemon
    /// picks up the new value on next restart.
    Rotate,
    /// Print the token. Default to stderr; --print echoes to stdout
    /// for shell capture (TOKEN=$(kb token show --print)).
    Show {
        #[arg(long)]
        print: bool,
    },
    /// Print the resolved file path (XDG_CONFIG_HOME/kb/token).
    Path,
    /// Issue a per-user registry token (`<config>/tokens`). Prints the
    /// plaintext once; store is `user:sha256:<hex>`. Restart the daemon
    /// to load. Refuses a duplicate user unless `--force`.
    Issue {
        /// Lowercase-valid username (`^[a-z0-9._@-]{1,64}$`).
        user: String,
        /// Replace an existing registry line for this user.
        #[arg(long)]
        force: bool,
    },
    /// Revoke every registry line for a user. Atomic rewrite; reports
    /// how many lines were removed. Restart the daemon to drop it.
    Revoke {
        /// Username whose registry line(s) to remove.
        user: String,
    },
}

/// `kb share` args. The optional positional + optional subcommand give
/// `kb share <target>` (deploy), `kb share list`, and `kb share revoke
/// <name>` over one verb (clap's default-subcommand pattern).
#[derive(Args, Debug)]
#[command(args_conflicts_with_subcommands = true)]
pub(crate) struct ShareArgs {
    /// Source-relative file or folder to publish. Omit when using a
    /// `list` / `revoke` subcommand.
    target: Option<String>,
    /// kb name (optional when only one kb is configured).
    #[arg(long)]
    kb: Option<String>,
    /// Static host: `cloudflare-pages` (default, gateable) or
    /// `github-pages` (public-only).
    #[arg(long, default_value = "cloudflare-pages")]
    host: String,
    /// Access gate rule, repeatable: email:DOMAIN | email:a@x,b@y |
    /// google | github. Cloudflare only.
    #[arg(long)]
    gate: Vec<String>,
    /// Publish ungated (world-readable, secret-URL only).
    #[arg(long)]
    public: bool,
    /// Cross-artifact link handling: `warn` (default) or `absolute`.
    #[arg(long, default_value = "warn")]
    links: String,
    /// Re-deploy to the same recorded deployment for this target.
    #[arg(long)]
    update: bool,
    /// Opt OUT of the export scrub (LEAKS the generation prompt — loud).
    #[arg(long)]
    no_scrub: bool,
    /// Y-track — also publish each artifact's comment thread + attachments
    /// into the static site. PUBLISHES otherwise-private review state.
    #[arg(long = "with-comments")]
    with_comments: bool,
    /// Open the resulting URL in a browser.
    #[arg(long)]
    open: bool,
    /// Write a self-contained OFFLINE bundle to PATH instead of publishing to a
    /// host. A `PATH` ending in `.zip` writes the zip; any other PATH is treated
    /// as a directory the bundle is extracted into. In-share cross-artifact
    /// links are relativized; `--host`/`--gate`/`--public`/`--update` are ignored.
    /// Required with `--list`.
    #[arg(long, value_name = "PATH")]
    local: Option<PathBuf>,
    /// Export a whole reading list as the offline bundle (ordered entries +
    /// generated TOC). Mutually exclusive with a path `target`. Requires
    /// `--local`. Address by `l_…` id or case-insensitive unique title.
    /// With `--list`, `--host`/`--gate`/`--public`/`--update` are ignored
    /// (same documented-ignore precedent as `--local`).
    #[arg(long = "list", value_name = "ID_OR_TITLE", conflicts_with = "page")]
    list: Option<String>,
    /// Write a single UNCOMPRESSED page to PATH in its native format — a
    /// scrubbed, self-contained `.html`, or the raw `.md` SOURCE for a Markdown
    /// artifact. One file only (no asset closure / zip); mutually exclusive
    /// with `--local`. `--host`/`--gate`/`--public`/`--update`/`--links` are
    /// ignored.
    #[arg(long, value_name = "PATH", conflicts_with = "local")]
    page: Option<PathBuf>,
    /// Emit the raw JSON response instead of a status line.
    #[arg(long)]
    json: bool,
    #[arg(long)]
    daemon: Option<String>,
    #[command(subcommand)]
    action: Option<ShareAction>,
}

#[derive(Subcommand, Debug)]
enum ShareAction {
    /// List active shares recorded for this kb.
    List {
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Revoke a share: tear down the host objects + drop the registry row.
    Revoke {
        /// The share name (from `kb share list`).
        name: String,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum ListAction {
    /// Create a reading list.
    Create {
        title: String,
        #[arg(long)]
        description: Option<String>,
        /// Pin to the top of the index.
        #[arg(long)]
        pin: bool,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// List reading lists across every kb (or one with `--kb`).
    Ls {
        #[arg(long)]
        kb: Option<String>,
        /// Include archived lists.
        #[arg(long)]
        archived: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Show one list: ordered entries with read state + time estimates.
    Show {
        /// List: `l_…` id or unique title (case-insensitive).
        list: String,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Add an artifact (or a §section of it) to a list.
    Add {
        list: String,
        /// Artifact: 12-hex id, source-relative path, or unique filename.
        target: String,
        /// Target a section (heading id) instead of the whole artifact.
        #[arg(long)]
        section: Option<String>,
        #[arg(long)]
        note: Option<String>,
        /// Insert before this entry (`le_…` id or 1-based index).
        #[arg(long, conflicts_with = "after")]
        before: Option<String>,
        /// Insert after this entry (`le_…` id or 1-based index).
        #[arg(long)]
        after: Option<String>,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Remove an entry from a list.
    Rm {
        list: String,
        /// Entry: `le_…` id or 1-based index.
        entry: String,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Edit an entry: note, §section anchor, or the manual read override.
    Update {
        list: String,
        /// Entry: `le_…` id or 1-based index.
        entry: String,
        #[arg(long)]
        note: Option<String>,
        #[arg(long, conflicts_with = "note")]
        clear_note: bool,
        /// Re-anchor onto this heading id (clears any stale flag).
        #[arg(long)]
        section: Option<String>,
        /// Drop the anchor (back to whole-artifact).
        #[arg(long, conflicts_with = "section")]
        clear_section: bool,
        /// Override the derived state as read.
        #[arg(long, conflicts_with_all = ["unread", "clear_read"])]
        read: bool,
        /// Override the derived state as unread.
        #[arg(long, conflicts_with = "clear_read")]
        unread: bool,
        /// Drop the override (back to the derived state).
        #[arg(long)]
        clear_read: bool,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Reorder an entry within its list.
    Move {
        list: String,
        /// Entry: `le_…` id or 1-based index.
        entry: String,
        /// Place before this entry (`le_…` id or 1-based index).
        #[arg(long, conflicts_with_all = ["after", "to"])]
        before: Option<String>,
        /// Place after this entry (`le_…` id or 1-based index).
        #[arg(long, conflicts_with = "to")]
        after: Option<String>,
        /// Move to this 1-based position.
        #[arg(long)]
        to: Option<usize>,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Rename a list.
    Rename {
        list: String,
        new_title: String,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Edit list metadata: description, pin, archive.
    Edit {
        list: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long, conflicts_with = "description")]
        clear_description: bool,
        #[arg(long, conflicts_with = "unpin")]
        pin: bool,
        #[arg(long)]
        unpin: bool,
        #[arg(long, conflicts_with = "unarchive")]
        archive: bool,
        #[arg(long)]
        unarchive: bool,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Delete a list and all its entries. Requires `--yes`.
    Delete {
        list: String,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Remove tombstoned entries (artifacts no longer in the index).
    /// Without `--yes`, lists what would be removed and exits.
    Prune {
        /// List: `l_…` id or unique title (case-insensitive).
        list: String,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Point a stale entry at a new §section (heading id).
    Reanchor {
        list: String,
        /// Entry: `le_…` id or 1-based index.
        entry: String,
        #[arg(long)]
        section: String,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Import a kb-list/1 document (Markdown or JSON; `-` = stdin).
    /// Targeting: `--into` > the doc's round-trip list_id > create a
    /// fresh list named by the document title.
    Import {
        /// File path, or `-` for stdin.
        file: String,
        /// Import into an existing list (id or title).
        #[arg(long)]
        into: Option<String>,
        /// `replace` (default — true round-trip) or `append`.
        #[arg(long, default_value = "replace")]
        mode: String,
        /// `md` | `json`; default by extension (stdin → md).
        #[arg(long)]
        format: Option<String>,
        /// Parse + report the plan without writing anything.
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Export a list as a kb-list/1 document (stdout unless `-o`).
    Export {
        list: String,
        /// `md` (default) | `json`.
        #[arg(long)]
        format: Option<String>,
        /// Write to a file instead of stdout.
        #[arg(short, long)]
        out: Option<String>,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
    },
}

/// `kb board <list> [--kb] [--json]` has no subcommand keyword for its
/// default (show) action, but `set` is a named one — the same shape
/// `cargo <plugin>` uses. `#[command(external_subcommand)]` on the tuple
/// variant catches any first token that isn't literally `set` (so
/// `<list>` is never mistaken for an unknown subcommand) and hands the
/// raw tokens to `commands::board::parse_show_args` for manual `--kb`/
/// `--json` parsing — clap's typed per-field parsing doesn't reach a
/// catch-all variant's `Vec<String>`.
#[derive(Subcommand, Debug, Clone)]
enum BoardAction {
    /// Replace the board's canvas wholesale (geometry only — never
    /// touches list membership/order). `--file -` reads the JSON Canvas
    /// document from stdin.
    Set {
        list: String,
        #[arg(long)]
        file: String,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
    },
    #[command(external_subcommand)]
    Show(Vec<String>),
}

/// `kb atlas field [--kb] [--json]` (the bare/show form) lives on the
/// `AtlasAction::Field` variant itself — see its doc comment for why
/// `action` here is a plain `Option<_>` rather than `BoardAction`'s
/// `external_subcommand` catch-all.
#[derive(Subcommand, Debug, Clone)]
enum AtlasFieldAction {
    /// Replace the operator field wholesale from a file/stdin. `--file -`
    /// reads the JSON Canvas document from stdin.
    Set {
        #[arg(long)]
        file: String,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Diff — the top-N most-disagreeing artifacts between the machine
    /// layout and the operator field (`GET .../atlas/field/disagreement`).
    Diff {
        #[arg(long)]
        kb: Option<String>,
        /// How many rows to print. Default 20.
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
}

/// `kb queries list|save|rm` — daemon-wide saved-query store
/// (`/api/saved-queries`, v0.13 Q4). Distinct from the bare `kb queries
/// --zero-hit` report on the parent `Cmd::Queries` variant, which reads a
/// different in-memory ring; these subcommands are the CLI's other half of
/// the SPA's saved-query ribbon (and, as of W3 C-c, the reflection canvas's
/// "save as scene" chip — a scene is nothing but a name plus a stored
/// `path`/`search`, so it rides this same store rather than inventing a
/// fourth one).
#[derive(Subcommand, Debug, Clone)]
enum QueriesAction {
    /// List every saved query on the daemon (name, path, search, saved_at).
    List {
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Save (or overwrite by case-insensitive name) a query. `--path`
    /// defaults to `/` (the gallery route) since that's the common case —
    /// a saved *search* against the default view.
    Save {
        name: String,
        /// `location.search`, including the leading `?` (e.g.
        /// `?q=foo&kb=x`). Defaults to empty (no filter).
        #[arg(long, default_value = "")]
        search: String,
        /// `location.pathname` this query restores to.
        #[arg(long, default_value = "/")]
        path: String,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Delete a saved query by name (case-insensitive). Idempotent —
    /// succeeds even if the name isn't there.
    Rm {
        name: String,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

/// CT-F3 — `kb links` verbs. Read the queue, then apply one row at a time;
/// there is deliberately no "apply all" (a suggestion is a claim, and a
/// human decides which claims become edges).
#[derive(Subcommand, Debug, Clone)]
enum LinksAction {
    /// List unlinked mentions: docs whose prose names another artifact with
    /// no link edge to show for it. Derived per request — nothing stored,
    /// nothing rewritten.
    Suggest {
        #[arg(long)]
        kb: Option<String>,
        /// Queue size (server clamps to 1..=200; default 50).
        #[arg(long)]
        limit: Option<u32>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Author one suggested wikilink into `<SRC>`'s Markdown source.
    /// Refuses loudly (tree untouched) when the source is HTML or a memory
    /// body, when the mention is already linked, or when the text moved.
    Apply {
        /// Mentioning doc (the file that gets edited): 12-hex id,
        /// source-relative path, or unique filename.
        src: String,
        /// Mentioned doc (the link destination), same grammar.
        dst: String,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum NotesAction {
    /// List notes across every kb (or one with `--kb`), newest-first.
    List {
        #[arg(long)]
        kb: Option<String>,
        /// Descendant-inclusive folder filter.
        #[arg(long)]
        folder: Option<String>,
        /// Exact-match kb-status filter (active|done|archived|…).
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Show one note's body + checklist + comment count.
    Show {
        /// Note: 12-hex id, source-relative path, or unique filename.
        target: String,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Create a note (or the scope's canonical notepad with `--notepad`).
    New {
        #[arg(long)]
        kb: Option<String>,
        /// Scope folder ("" / omitted = kb root).
        #[arg(long)]
        folder: Option<String>,
        #[arg(long)]
        title: Option<String>,
        /// Inline Markdown body (use `--stdin` to read it from stdin).
        /// `allow_hyphen_values` so a todo body like `- [ ] task` (leading
        /// `-`) isn't mistaken for a flag.
        #[arg(long, conflicts_with = "stdin", allow_hyphen_values = true)]
        body: Option<String>,
        #[arg(long)]
        stdin: bool,
        /// Repeatable tag(s).
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long)]
        status: Option<String>,
        /// Write/return the scope's canonical `_notepad.md`.
        #[arg(long)]
        notepad: bool,
        /// CT-A4 — opt out of auto-stamping `kb-session` from the current
        /// Claude Code session marker (`~/.cache/kb/current-session`,
        /// written by the SessionStart / UserPromptSubmit hooks). Skips the
        /// marker-file read entirely, mirroring `kb remember --no-session`.
        #[arg(long)]
        no_session: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Edit a note's title / body / status / tags.
    Edit {
        target: String,
        #[arg(long)]
        title: Option<String>,
        /// `allow_hyphen_values` so a todo body (leading `-`) isn't mistaken
        /// for a flag — same as `new --body`.
        #[arg(long, conflicts_with = "stdin", allow_hyphen_values = true)]
        body: Option<String>,
        #[arg(long)]
        stdin: bool,
        #[arg(long)]
        status: Option<String>,
        #[arg(long = "tag")]
        tags: Vec<String>,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Check (tick) the Nth checklist item (0-based, document order).
    Check {
        target: String,
        #[arg(long)]
        item: usize,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Uncheck the Nth checklist item.
    Uncheck {
        target: String,
        #[arg(long)]
        item: usize,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Append a new unchecked task line to a note.
    Append {
        target: String,
        /// `allow_hyphen_values` so task text starting with `-` is accepted.
        #[arg(long, allow_hyphen_values = true)]
        item: String,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Mark a note done (sets `kb-status: done`).
    Done {
        target: String,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Archive a note (sets `kb-status: archived`).
    Archive {
        target: String,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Delete a note (requires `--yes`).
    Rm {
        target: String,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Show a note's wikilinks: outgoing `[[…]]` (resolved + dangling) and
    /// backlinks (what links here). The connective-tissue view.
    Links {
        /// Note: 12-hex id, source-relative path, or unique filename.
        target: String,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum SessionsAction {
    /// Build a session capture artifact from a transcript file — the Rust
    /// engine `kb-capture.sh` shells out to (W0.4). Resolves each detected
    /// git commit sha (one `git show -s` per sha) against the session's
    /// recorded cwd before writing, something the bash heredoc fallback
    /// can't do. Filesystem-only: never talks to the daemon (the watcher
    /// indexes the written file like any other artifact). Distinct from the
    /// daemon-facing `kb capture` quick-capture verb.
    Capture {
        /// Path to the raw JSONL transcript (Claude Code's own
        /// `transcript_path`).
        #[arg(long)]
        transcript: PathBuf,
        /// The hook's own `.session_id` — used ONLY when the transcript
        /// carries no `sessionId` of its own (the JSONL field is ground
        /// truth, invariant #11).
        #[arg(long = "session-id")]
        session_id: Option<String>,
        /// Working directory to resolve detected commits against. Defaults
        /// to the transcript's own modal `cwd`.
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Target sessions-corpus source dir. Defaults to
        /// `$KB_SESSIONS_DIR` (the same env var `kb-capture.sh` uses).
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        json: bool,
        /// Capture a transcript over
        /// `kb_core::sessions::CAPTURE_MAX_TRANSCRIPT_BYTES` anyway (2026-
        /// 08-21 ci-host incident: RAW transcripts entering capture whole are
        /// refused past this cap by default, never truncated — the
        /// main-transcript `<pre>` is a byte-identical `claude -r` resume
        /// contract).
        #[arg(long)]
        allow_oversized: bool,
    },
    /// List captured sessions newest-first.
    List {
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
        /// Cap on rows returned (default 50).
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// A1 — restrict to one working directory (full cwd path, as shown
        /// by `kb sessions folders`).
        #[arg(long)]
        folder: Option<String>,
        /// W4/W3.A — restrict to one project (a registered `[projects.*]` id,
        /// or a raw project_key). Composes (AND) with --folder.
        #[arg(long)]
        project: Option<String>,
        /// W4/W3.A/S1 — csv over the triage enum (trivial|routine|substantive).
        #[arg(long)]
        substance: Option<String>,
        /// W5/I — csv over the closed harness set
        /// (claude|codex|opencode|grok|kimi|omp).
        #[arg(long)]
        harness: Option<String>,
    },
    /// List the folders (working directories) sessions ran in, with counts.
    Folders {
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// R9 — top research queries per project folder (or overall).
    Rollup {
        #[arg(long)]
        folder: Option<String>,
        /// W4/W3.A — restrict to one project (registered id or raw project_key).
        #[arg(long)]
        project: Option<String>,
        /// L1/F1 — csv over the triage enum (trivial|routine|substantive).
        #[arg(long)]
        substance: Option<String>,
        #[arg(long, default_value_t = 5)]
        limit: u32,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// R9 — the activity funnel: searched → opened → edited → committed → commented.
    Funnel {
        #[arg(long)]
        folder: Option<String>,
        /// W4/W3.A — restrict to one project (registered id or raw project_key).
        #[arg(long)]
        project: Option<String>,
        /// L1/F1 — csv over the triage enum (trivial|routine|substantive).
        #[arg(long)]
        substance: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// W6 (moonshots M4) — the project ledger: sessions/commits/decisions/
    /// research for a project, grouped by UTC day over a trailing window.
    Ledger {
        /// Restrict to one project (registered id or raw project_key).
        /// Absent = every project in the window.
        #[arg(long)]
        project: Option<String>,
        /// Trailing window in UTC calendar days (default 7, max 31).
        #[arg(long)]
        days: Option<u32>,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Search sessions by keyword (title / first prompt / folder).
    Search {
        query: String,
        #[arg(long)]
        folder: Option<String>,
        /// W4/W3.A — restrict to one project (registered id or raw project_key).
        #[arg(long)]
        project: Option<String>,
        /// W4/W3.A/S1 — csv over the triage enum (trivial|routine|substantive).
        #[arg(long)]
        substance: Option<String>,
        /// W5/I — csv over the closed harness set
        /// (claude|codex|opencode|grok|kimi|omp).
        #[arg(long)]
        harness: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// cli-grok Proposal 2 #2 — which session(s) touched this artifact, and
    /// how (reverse link, `GET /artifacts/{kb}/{artifact_id}/sessions`).
    Of {
        /// A 12-hex artifact id, or a source-relative path (fuzzy-resolved).
        target: String,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// cli-grok Proposal 2 #3 — the flat bulk commit↔session feed
    /// (`GET /sessions/commit-map`), previously reachable only as an
    /// internal paging helper for `provenance-report`.
    CommitMap {
        /// Floor on the owning session's `started_at` (unix seconds).
        #[arg(long)]
        since: Option<i64>,
        #[arg(long, default_value_t = 200)]
        limit: u32,
        #[arg(long, default_value_t = 0)]
        offset: u32,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// memo R8/ADD-2 — the grokclaude job join: every Claude Code session
    /// whose transcript invoked (or, from W5, IS) this grokclaude job ulid.
    ByJob {
        ulid: String,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// cli-grok Proposal 1 / memo R13, windowed default per PF-R1 — the
    /// interpreted terminal transcript reader: header (title/harness/
    /// project/asked/closed/honest metrics) + the last `--tail` turns + an
    /// outcome footer, over the SAME `session-view/1` engine the HTML
    /// renderer and the wire consume (ONE interpretation, three
    /// presenters). The default fetch is windowed server-side
    /// (`?turns=<n>`) — `--full`/`--turn`/`--grep` each fetch the whole
    /// transcript instead, since they need to see turns outside the tail.
    Read {
        session_id: String,
        /// Fetch and render the WHOLE transcript instead of the default
        /// tail window (PF-R1: the default now asks the server for only
        /// the last `--tail` turns, not the full session).
        #[arg(long)]
        full: bool,
        /// PF-R1 — the tail window's turn count, both on the wire
        /// (`?turns=<n>`, so the default read no longer over-fetches) and
        /// in the render (default 10). Ignored by `--full`/`--turn`/
        /// `--grep`, which always fetch everything for correctness.
        #[arg(long)]
        tail: Option<u32>,
        /// An explicit turn ordinal window: `N` or `A..B` (inclusive).
        /// Overrides head/tail windowing.
        #[arg(long)]
        turn: Option<String>,
        /// Filter to turns whose text matches PAT (case-insensitive
        /// substring), ± --context turns either side. Overrides windowing.
        #[arg(long)]
        grep: Option<String>,
        #[arg(long, default_value_t = 0)]
        context: u32,
        /// The decoded transcript JSONL, verbatim (`GET /{sid}/raw`).
        #[arg(long)]
        raw: bool,
        /// W7 (R15/LF-6) — render the LIVE transcript (`[sessions]
        /// live_transcripts_dir`), direct-disk via the shared
        /// `resolve_live_transcript` resolver — zero daemon required. Exits
        /// 2 with an honest hint if the session isn't resolvably live.
        /// Implied by `--follow`.
        #[arg(long)]
        live: bool,
        /// W7 (R15/LF-6) — `tail -f`: after the one-shot `--live` render,
        /// poll the file (750ms) and print interpreted turns as they
        /// complete; a live-header line refreshes in place on a TTY.
        /// Ctrl-C exits. Implies `--live`.
        #[arg(long)]
        follow: bool,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long = "no-color")]
        no_color: bool,
        /// Wrap width for the presenter (default 100 — goldens are pinned
        /// under `--no-color --width 100`).
        #[arg(long)]
        width: Option<usize>,
    },
    /// LSC-1/LSC-2 (`docs/research/kb-live-sessions-cockpit-2026-08.html`
    /// §8) — the live-sessions snapshot: who holds the ball, right now,
    /// grouped into IN PROGRESS / WAITING ON YOU / FINISHED·COLD. Two
    /// sources, one presenter: `--local` reads the Claude Code transcript
    /// tree directly (zero daemon, mirrors `read --live`'s "zero daemon
    /// required" precedent); the default (daemon-backed) mode hits `GET
    /// /api/sessions/live-status` — the merged view of every beat-tracked
    /// session PLUS the Tier-0 degraded layer rebuilt from landed captures.
    /// Both modes render through the exact same filter/sort/print code, so
    /// `--json` output has the same shape either way.
    Status {
        /// Direct-disk snapshot, zero daemon required — LSC-5 fans this out
        /// across EVERY harness (claude/codex/opencode/grok/kimi), not just
        /// Claude Code: `kb_core::sessions::live_adapters::scan_all`. Omit
        /// to use the daemon-backed mode instead.
        #[arg(long)]
        local: bool,
        /// Override the Claude Code transcripts root (`--local` only; the
        /// other four harnesses use their real default locations — no
        /// per-harness root flags in this phase). Defaults to `[sessions]
        /// live_transcripts_dir` from `kb.toml` when configured, else
        /// `~/.claude/projects`.
        #[arg(long)]
        root: Option<PathBuf>,
        /// csv over the six-state vocabulary: working, stalled, waiting,
        /// cold, finished, presumed_ended.
        #[arg(long)]
        state: Option<String>,
        /// Restrict to one project (matched against the derived `project`
        /// field: the transcript's `cwd` basename, else the containing
        /// project-slug directory name).
        #[arg(long)]
        project: Option<String>,
        /// Restrict to one harness — csv-free, single value, one of
        /// `kb_core::sessions::HARNESSES` (claude/codex/opencode/grok/kimi/
        /// omp; rejected otherwise).
        #[arg(long)]
        harness: Option<String>,
        /// Cap rows PER LANE (both the human table and the `--json` array
        /// are built from the same per-lane-capped rows). Absent =
        /// unlimited.
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        json: bool,
        #[arg(long = "no-color")]
        no_color: bool,
        /// Daemon URL override for the daemon-backed mode (ignored under
        /// `--local`). Defaults to the usual `kb` daemon-detection ladder.
        #[arg(long)]
        daemon: Option<String>,
    },
    /// kb-code Wave 0 (W0.6) — the sha→session reverse lookup: which
    /// session(s) recorded a commit matching this full or short sha
    /// (>=7 hex chars).
    ByCommit {
        sha: String,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// kb-code Wave 0 (W0.6) — the WEDGE INSTRUMENT: classify every commit in
    /// `--repo`'s history (from HEAD) into trailer / recorded / pre-capture /
    /// non-session, plus an orphan (rebased-or-squashed) pass. Probe-grade
    /// measurement of how much of a repo's history kb-memory can join to a
    /// session — not the final wave-3 join ladder.
    ProvenanceReport {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// kb-code Wave 0 (W0.6) — a THIN probe (not the final wave-3 ladder):
    /// `git blame <file>:<line>` → the blamed commit's `Kb-Session` trailer,
    /// else the daemon's `by-commit` lookup → confidence + session, if any.
    WhyLine {
        /// `<file>:<line>`, e.g. `crates/kb-core/src/sessions.rs:120`.
        file_line: String,
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Narrative threads — sessions clustered into continued efforts by folder
    /// + time.
    Threads {
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Save a folder's most-recent thread as an editable reading list.
    SaveThread {
        /// Folder path or basename to save the latest thread from.
        folder: String,
        #[arg(long)]
        title: Option<String>,
        /// CT-E5 — order the list as each session's STORY (capture, files
        /// touched, memories produced, memories recalled) instead of one
        /// entry per transcript; the description carries the session ids,
        /// capture dates and — when the kb has a `code_url` — the kb-code
        /// session-diff links.
        #[arg(long)]
        narrative: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Show one session's detail (started, message_count, memories
    /// produced, artifacts touched, decisions, effort).
    Show {
        session_id: String,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
        /// cli-grok Proposal 2 #4 — csv, repeatable: fetch ONLY these
        /// sections (files,decisions,commits,research,memories,touches,
        /// comments,readings,outcome). Absent = every section (unchanged).
        #[arg(long, value_delimiter = ',')]
        section: Vec<String>,
    },
    /// Replay a captured session beat by beat on the transcript's own clock:
    /// prompts, reads/edits/writes, searches, commits and steering decisions,
    /// each resolved to the artifact (and heading) it touched. `--json` prints
    /// the daemon's `session-replay/1` wire verbatim — the same bytes the SPA
    /// renders.
    Replay {
        session_id: String,
        /// Keep only beats that resolved to this artifact id.
        #[arg(long)]
        artifact: Option<String>,
        /// R7/S6 — serve-window offset (the daemon computes the FULL
        /// timeline; this slices it), applied after `--artifact` and before
        /// `--limit`. Reach the tail of a long session with e.g.
        /// `--from-seq 4000`.
        #[arg(long)]
        from_seq: Option<usize>,
        /// Cap the beats printed (applied after `--artifact`/`--from-seq`).
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Print a deterministic resume-context block (goal + branch + edited
    /// files + decisions) to pick up where a session left off.
    Resume {
        session_id: String,
        #[arg(long)]
        daemon: Option<String>,
        /// cli-grok Proposal 2 #5 — emit the combined resume payload as JSON
        /// (the one sessions verb that previously had no `--json` at all).
        #[arg(long)]
        json: bool,
    },
    /// Bundle a captured session (transcript + manifest) into a portable
    /// `<sid>.kbsession.zip` for cross-machine `claude -r` resume. Offline:
    /// reads the sessions corpus on disk (`--from`, default `KB_SESSIONS_DIR`).
    Export {
        /// The Claude Code session id (as shown by `kb sessions list`).
        session_id: String,
        /// Sessions corpus dir to read captures from. Default `KB_SESSIONS_DIR`.
        #[arg(long)]
        from: Option<PathBuf>,
        /// Output bundle path. Default `./<sid>.kbsession.zip`.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Redact secrets before bundling (cross-account): the safe floor —
        /// known token shapes + labeled secrets + secret-named env vars.
        #[arg(long)]
        scrub: bool,
        /// Also anonymise `/home/<user>` & `/Users/<user>` usernames (implies
        /// --scrub).
        #[arg(long)]
        scrub_paths: bool,
        /// Also sweep long high-entropy blobs (implies --scrub; may over-redact
        /// real base64/hashes).
        #[arg(long)]
        scrub_entropy: bool,
        /// Skip the interactive confirmation of a redacted bundle (required for
        /// non-interactive / --json scrubbing).
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
    /// Place a bundle's transcript under `~/.claude/projects/<slug>/` so
    /// `claude -r <id>` finds it on this machine.
    Rehydrate {
        /// The `.kbsession.zip` produced by `kb sessions export`.
        bundle: PathBuf,
        /// Target working directory (drives the project slug). Default: cwd.
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Report the destination without writing.
        #[arg(long)]
        dry_run: bool,
        /// Overwrite an existing transcript for this session id.
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
    /// Fetch a session bundle from a REMOTE kb daemon (`--from <url>`) over its
    /// authenticated API, then optionally rehydrate it here. A public remote
    /// forces the redaction floor before streaming (invariant #4).
    Pull {
        /// The session id on the remote daemon.
        session_id: String,
        /// Remote daemon base URL, e.g. `https://kb.example.com`.
        #[arg(long)]
        from: String,
        /// Save the bundle here. Default `./<sid>.kbsession.zip`.
        #[arg(long)]
        out: Option<PathBuf>,
        /// After downloading, place the transcript for `claude -r` on this box.
        #[arg(long)]
        rehydrate: bool,
        /// Target working dir for --rehydrate (drives the project slug).
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Overwrite an existing transcript when --rehydrate.
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum ImportAction {
    /// Retroactive backfill of historical Claude Code session transcripts.
    /// Walks `--dir` (default `~/.claude/projects`) for every `*.jsonl`
    /// transcript, recovers each session's canonical id from the JSONL itself,
    /// and writes the exact live-capture envelope into `--into` — the target
    /// should be a configured sessions corpus's source dir so the daemon
    /// indexes each import as episodic memory. Deduped + idempotent: a session
    /// already captured (live or previously imported) is skipped, so re-running
    /// imports 0.
    ClaudeHistory {
        /// Project tree to scan. Default `~/.claude/projects`.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Target sessions-corpus source dir. Defaults to `$KB_SESSIONS_DIR`
        /// (the same env var the capture hook uses); errors if neither is set.
        #[arg(long)]
        into: Option<PathBuf>,
        /// Report what would import/skip (and why) without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Cap the number of NEW captures written (duplicates/skips don't
        /// count) — import a handful to try it out.
        #[arg(long)]
        limit: Option<u32>,
        /// Emit a machine summary + per-transcript items as JSON.
        #[arg(long)]
        json: bool,
        /// Suppress the per-transcript lines (keep the one-line summary).
        #[arg(long)]
        quiet: bool,
        /// Instead of importing new transcripts, re-walk EXISTING captures
        /// already in `--into` and rewrite each one's subagent digest +
        /// sidecar text blocks from `--transcripts-root`'s sidecar
        /// directories (`<project>/<session-id>/subagents/agent-*.jsonl`),
        /// leaving the `<pre>` transcript untouched. Idempotent — a second
        /// run against unchanged sidecars is a no-op.
        #[arg(long)]
        refresh_subagents: bool,
        /// The transcripts tree `--refresh-subagents` searches for sidecar
        /// directories. Default `~/.claude/projects`; override to point at
        /// an extracted archive dir. Ignored without `--refresh-subagents`.
        #[arg(long)]
        transcripts_root: Option<PathBuf>,
        /// Import transcripts over
        /// `kb_core::sessions::CAPTURE_MAX_TRANSCRIPT_BYTES` anyway (2026-
        /// 08-21 ci-host incident hardening: RAW transcripts over this cap are
        /// skipped by default rather than read into memory whole, with a
        /// warning to stderr per skip — a backfill sweep must not abort on
        /// one monster file).
        #[arg(long)]
        allow_oversized: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum DeskAction {
    /// Push a draft to `handoff/<slug>.<ext>` (stable name, overwrites).
    Offer {
        /// Local file, or `-` to read Markdown from stdin.
        file: String,
        /// Stable slug (output filename is `<slug>.<ext>`).
        #[arg(long = "as")]
        as_slug: String,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        title: Option<String>,
        /// Comma-separated extra tags (`draft` is always added).
        #[arg(long)]
        tags: Option<String>,
        /// Humane duration: `45m`, `24h`, or `7d`. Stamped as display-only
        /// `kb-expires-at`.
        #[arg(long)]
        ttl: Option<String>,
        #[arg(long)]
        sanitize: bool,
        /// Best-effort `xdg-open` on the reader permalink.
        #[arg(long)]
        open: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Replace an artifact's source bytes (`PUT …/content`).
    Update {
        /// 12-hex id or source-relative path.
        target: String,
        file: String,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// List desk drafts via `GET /api/desk`.
    Ls {
        #[arg(long)]
        kb: Option<String>,
        /// Fleet-wide (no kb filter); adds a KB column.
        #[arg(long, conflicts_with = "kb")]
        all: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Watch for `you`-authored comments (delegates to `kb comments watch`).
    Wait {
        /// Artifact or folder; default `handoff`.
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        once: bool,
        #[arg(long)]
        timeout: Option<u64>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Hard-delete a draft (`DELETE …/artifacts/{id}?purge=true`).
    Expire {
        /// 12-hex id or source-relative path.
        target: String,
        #[arg(long)]
        kb: Option<String>,
        /// Override the open-comment refusal.
        #[arg(long)]
        force: bool,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Graduate a draft out of `handoff/` (relocate + drop `draft` tag).
    Promote {
        /// 12-hex id or source-relative path.
        target: String,
        /// Destination source-relative path (must not be under `handoff/`).
        #[arg(long = "to")]
        to: String,
        #[arg(long)]
        category: Option<String>,
        /// Leave the `draft` tag in place.
        #[arg(long)]
        keep_draft_tag: bool,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum CommentsAction {
    /// List comments via the daemon (`GET /reviews`). Lists every
    /// configured kb unless `--kb`/`--path` narrows it.
    List {
        #[arg(long)]
        kb: Option<String>,
        /// Include resolved comments too (status=all).
        #[arg(long)]
        all: bool,
        /// Emit JSON rows instead of the column table.
        #[arg(long)]
        json: bool,
        /// Filter to a single artifact by source-relative path or unique
        /// filename (resolved via `/lookup`).
        #[arg(long)]
        path: Option<String>,
        /// Only comments by this author (you|claude).
        #[arg(long)]
        author: Option<String>,
        /// Only comments by this attribution username (`?user=`).
        #[arg(long)]
        user: Option<String>,
        /// Only comments whose anchor is currently stale.
        #[arg(long)]
        stale: bool,
        /// Only artifacts in this folder (relative to the kb source root).
        #[arg(long)]
        folder: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Fleet-wide open-comments inbox (`GET /api/inbox`) — every OPEN
    /// comment across every configured kb, newest activity first. One place
    /// to catch replies that would otherwise sit silently per-artifact.
    Inbox {
        /// Restrict to one corpus (default: every configured kb).
        #[arg(long)]
        kb: Option<String>,
        /// Cap the number of comments shown (default 50).
        #[arg(long)]
        limit: Option<u32>,
        /// Emit the raw `{items,total_open}` JSON instead of the table.
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Print one artifact's full comment thread (bodies, replies,
    /// choices, timestamps) via `GET /review/{id}`.
    Show {
        /// kb name (positional). Optional when `--path` is supplied.
        kb: Option<String>,
        /// 12-hex artifact id (positional). Conflicts with `--path`.
        artifact_id: Option<String>,
        /// kb name (flag form). Wins over the positional when both given.
        #[arg(long = "kb")]
        kb_flag: Option<String>,
        /// 12-hex artifact id (flag form). Wins over the positional.
        #[arg(long = "artifact-id")]
        artifact_id_flag: Option<String>,
        #[arg(long, conflicts_with_all = ["artifact_id", "artifact_id_flag"])]
        path: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Stream the daemon-rendered export of one artifact's review to
    /// stdout (`POST .../export`). Pipe into `claude code "..."`.
    Export {
        /// kb name (positional). Optional when `--path` is supplied
        /// (defaults to the single configured kb).
        kb: Option<String>,
        /// 12-hex artifact id (positional). Conflicts with `--path`.
        artifact_id: Option<String>,
        /// kb name (flag form). Wins over the positional when both given.
        #[arg(long = "kb")]
        kb_flag: Option<String>,
        /// 12-hex artifact id (flag form). Wins over the positional.
        #[arg(long = "artifact-id")]
        artifact_id_flag: Option<String>,
        /// L1 — alternative to <artifact_id>. Resolved via /lookup;
        /// ambiguous matches exit non-zero.
        #[arg(long, conflicts_with_all = ["artifact_id", "artifact_id_flag"])]
        path: Option<String>,
        /// Output format: claude (default) | json | md.
        #[arg(long, default_value = "claude")]
        format: String,
        /// Y-track — write a self-contained bundle to this directory
        /// (`review.<ext>` with attachment refs rewritten to relative paths
        /// plus every attachment blob under `attachments/`) instead of
        /// streaming to stdout.
        #[arg(long = "out-dir")]
        out_dir: Option<String>,
        /// v0.19 — bake the review state into a standalone copy of the
        /// artifact HTML (an inert `kb-review-state` block), so the file
        /// travels with its comments. Read back with `kb comments import`.
        #[arg(long)]
        embed: bool,
        /// Destination file for `--embed` (defaults to stdout).
        #[arg(long, short = 'o')]
        out: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// v0.19 — apply an ordered batch of comment mutations atomically
    /// (one daemon round-trip, all-or-nothing). The payload is a JSON
    /// array of ops — each `{"op":"add_comment"|"add_reply"|"resolve"|
    /// "unresolve"|"resolve_all"|"unresolve_all"|"edit_comment"|
    /// "edit_reply"|"set_anchor"|"delete_comment"|"delete_reply", …}` — or
    /// an object `{"ops":[…]}`. Mirrors redline's `apply`.
    Apply {
        /// kb name (positional). Optional when `--path` is supplied.
        kb: Option<String>,
        /// 12-hex artifact id (positional). Conflicts with `--path`.
        artifact_id: Option<String>,
        /// kb name (flag form). Wins over the positional when both given.
        #[arg(long = "kb")]
        kb_flag: Option<String>,
        /// 12-hex artifact id (flag form). Wins over the positional.
        #[arg(long = "artifact-id")]
        artifact_id_flag: Option<String>,
        /// L1 — alternative to <artifact_id>.
        #[arg(long, conflicts_with_all = ["artifact_id", "artifact_id_flag"])]
        path: Option<String>,
        /// Read the ops JSON from this file (`-` is not special; use
        /// `--ops-json` for inline). One of `--ops-file`/`--ops-json` is
        /// required.
        #[arg(long = "ops-file")]
        ops_file: Option<String>,
        /// Inline ops JSON (array or `{"ops":[…]}`).
        #[arg(long = "ops-json", conflicts_with = "ops_file")]
        ops_json: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// v0.19 — import comments embedded in an HTML file (produced by
    /// `export --embed`) back into the daemon's sidecar, restoring ids /
    /// statuses / replies. Refuses to overwrite existing non-empty comments
    /// unless `--force`.
    Import {
        /// The HTML file carrying an embedded `kb-review-state` block.
        file: String,
        /// kb name (positional). Optional when `--path` is supplied.
        kb: Option<String>,
        /// 12-hex artifact id (positional, the import target). Conflicts
        /// with `--path`.
        artifact_id: Option<String>,
        /// kb name (flag form). Wins over the positional when both given.
        #[arg(long = "kb")]
        kb_flag: Option<String>,
        /// 12-hex artifact id (flag form). Wins over the positional.
        #[arg(long = "artifact-id")]
        artifact_id_flag: Option<String>,
        /// L1 — alternative to <artifact_id>.
        #[arg(long, conflicts_with_all = ["artifact_id", "artifact_id_flag"])]
        path: Option<String>,
        /// Overwrite existing non-empty comments on the target.
        #[arg(long)]
        force: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// v0.3 — flip a comment's status to "resolved" via the daemon.
    /// Uses If-Match for safe concurrent updates.
    Resolve {
        /// kb name (positional). Optional when `--path` is supplied.
        kb: Option<String>,
        /// 12-hex artifact id (positional). Conflicts with `--path`.
        artifact_id: Option<String>,
        /// Specific comment id to resolve. Required unless `--all` is
        /// supplied; mutually exclusive with `--all`.
        comment_id: Option<String>,
        /// kb name (flag form). Wins over the positional when both given.
        #[arg(long = "kb")]
        kb_flag: Option<String>,
        /// 12-hex artifact id (flag form). Wins over the positional.
        #[arg(long = "artifact-id")]
        artifact_id_flag: Option<String>,
        /// L1 — alternative to <artifact_id>.
        #[arg(long, conflicts_with_all = ["artifact_id", "artifact_id_flag"])]
        path: Option<String>,
        /// L1 — flip every open comment on this artifact to resolved
        /// in one daemon roundtrip. Useful after Claude Code applies
        /// every fix.
        #[arg(long, conflicts_with = "comment_id")]
        all: bool,
        /// Force HTTP against this daemon URL (defaults to
        /// 127.0.0.1:4000 from kb-cli's daemons resolver).
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Append a Claude reply to an existing comment via the daemon.
    /// Fires `comments.updated` so the SPA renders the reply live —
    /// the Claude-Code half of a review conversation.
    ///
    /// `comment_id` is the sole positional (it is required, so it can't
    /// trail the optional kb/artifact positionals without ambiguity);
    /// the artifact is named via `--path` (the usual loop form) or
    /// `--artifact-id` + optional `--kb`.
    Reply {
        /// The comment id to reply to (from `kb comments list`/`watch`).
        comment_id: String,
        /// kb name. Optional when only one kb is configured.
        #[arg(long)]
        kb: Option<String>,
        /// 12-hex artifact id. Conflicts with `--path`.
        #[arg(long)]
        artifact_id: Option<String>,
        /// Source-relative path / unique filename, resolved via /lookup.
        #[arg(long, conflicts_with = "artifact_id")]
        path: Option<String>,
        #[arg(long)]
        body: String,
        /// Attach a quick-response button (repeatable). Each value is a
        /// JSON object: `{"label":"Apply","reply":"yes, apply it","resolve":true}`.
        /// The SPA renders these as one-tap buttons on this reply; a click
        /// posts the `reply` as a "you" reply (and resolves if `resolve`).
        #[arg(long = "choice-json")]
        choice_json: Vec<String>,
        /// Y-track — attach file(s)/image(s) (repeatable). Each is staged,
        /// adopted onto the new reply, and its inline ref appended to --body.
        #[arg(long)]
        attach: Vec<String>,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// v0.4 D3 — append a new comment to an artifact via the daemon.
    /// Defaults to --author claude (this verb is the Claude-Code
    /// path; humans use the SPA annotator).
    Add {
        /// kb name (positional). Optional when `--path` is supplied.
        kb: Option<String>,
        /// 12-hex artifact id (positional). Conflicts with `--path`.
        artifact_id: Option<String>,
        /// kb name (flag form). Wins over the positional when both given.
        #[arg(long = "kb")]
        kb_flag: Option<String>,
        /// 12-hex artifact id (flag form). Wins over the positional.
        #[arg(long = "artifact-id")]
        artifact_id_flag: Option<String>,
        /// L1 — alternative to <artifact_id>.
        #[arg(long, conflicts_with_all = ["artifact_id", "artifact_id_flag"])]
        path: Option<String>,
        #[arg(long)]
        body: String,
        /// One of: file | chapter:PATH | section:ID | selection:CSS:OFFSET:SNIPPET
        #[arg(long, default_value = "file")]
        anchor: String,
        #[arg(long, default_value = "claude")]
        author: String,
        /// Anchor the comment to a specific page src for a multi-page
        /// artifact. Defaults to the artifact id.
        #[arg(long)]
        page: Option<String>,
        /// Attach a quick-response button (repeatable). Each value is a
        /// JSON object: `{"label":"Apply","reply":"yes, apply it","resolve":true}`.
        /// Intended for --author claude; the SPA only renders choice
        /// buttons on Claude-authored open comments.
        #[arg(long = "choice-json")]
        choice_json: Vec<String>,
        /// Y-track — attach file(s)/image(s) (repeatable). Each is staged,
        /// adopted onto the new comment, and its inline ref appended to --body.
        #[arg(long)]
        attach: Vec<String>,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Monitor (SSE) for new `you`-authored review activity — new
    /// top-level comments AND your replies on Claude's comments (a
    /// two-way loop) — scoped to one artifact or a whole folder. Emits a
    /// Claude-friendly block (or `--json` line, with a `kind` field) per
    /// item as it arrives. Designed to be driven inside a Claude `/loop`.
    Watch {
        /// Source-relative path / unique filename (one artifact) OR a
        /// folder path (all descendants).
        #[arg(long)]
        path: String,
        /// kb name. Optional when only one kb is configured.
        #[arg(long)]
        kb: Option<String>,
        /// Emit one JSON object per line instead of a human block.
        #[arg(long)]
        json: bool,
        /// Exit after surfacing the first new comment — one comment per
        /// loop turn.
        #[arg(long)]
        once: bool,
        /// Maximum seconds to wait before exiting cleanly (exit 0) with
        /// nothing surfaced. Omit to wait indefinitely.
        #[arg(long)]
        timeout: Option<u64>,
        /// Also surface comments that are already open at startup (the
        /// backlog), not just ones that arrive after.
        #[arg(long)]
        backlog: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Reopen a resolved comment (flip status back to open), or `--all`.
    Unresolve {
        /// kb name (positional). Optional when `--path` is supplied.
        kb: Option<String>,
        /// 12-hex artifact id (positional). Conflicts with `--path`.
        artifact_id: Option<String>,
        /// Specific comment id to reopen. Required unless `--all`.
        comment_id: Option<String>,
        /// kb name (flag form). Wins over the positional when both given.
        #[arg(long = "kb")]
        kb_flag: Option<String>,
        /// 12-hex artifact id (flag form). Wins over the positional.
        #[arg(long = "artifact-id")]
        artifact_id_flag: Option<String>,
        #[arg(long, conflicts_with_all = ["artifact_id", "artifact_id_flag"])]
        path: Option<String>,
        /// Reopen every resolved comment on this artifact in one call.
        #[arg(long, conflicts_with = "comment_id")]
        all: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Edit a comment's body (or a reply's, with `--reply <reply_id>`).
    Edit {
        /// The comment id to edit.
        comment_id: String,
        /// kb name. Optional when only one kb is configured.
        #[arg(long)]
        kb: Option<String>,
        /// 12-hex artifact id. Conflicts with `--path`.
        #[arg(long)]
        artifact_id: Option<String>,
        #[arg(long, conflicts_with = "artifact_id")]
        path: Option<String>,
        /// Edit this reply (nested in the comment) instead of the comment.
        #[arg(long)]
        reply: Option<String>,
        /// The replacement body.
        #[arg(long)]
        body: String,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// W2.15a — set (or `--clear`) the artifact's three-state review-pass
    /// verdict: comment | approve | request-changes. Distinct from any
    /// individual comment's open/resolved status — this is the review-
    /// pass-level signal (`ReviewFile.verdict`). The daemon also mirrors it
    /// onto the artifact's own kb-tags as a `status-approved` /
    /// `status-changes-requested` display shortcut.
    Verdict {
        /// comment | approve | request-changes. Required unless `--clear`.
        #[arg(required_unless_present = "clear")]
        state: Option<String>,
        /// kb name. Optional when only one kb is configured.
        #[arg(long)]
        kb: Option<String>,
        /// 12-hex artifact id. Conflicts with `--path`.
        #[arg(long)]
        artifact_id: Option<String>,
        #[arg(long, conflicts_with = "artifact_id")]
        path: Option<String>,
        /// Attach a short note explaining the verdict.
        #[arg(long)]
        note: Option<String>,
        /// Clear any existing verdict instead of setting a new one.
        #[arg(long, conflicts_with = "state")]
        clear: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Delete a comment (or a reply, with `--reply <reply_id>`). Requires
    /// `--yes` — the deletion is permanent.
    Delete {
        /// The comment id to delete.
        comment_id: String,
        /// kb name. Optional when only one kb is configured.
        #[arg(long)]
        kb: Option<String>,
        /// 12-hex artifact id. Conflicts with `--path`.
        #[arg(long)]
        artifact_id: Option<String>,
        #[arg(long, conflicts_with = "artifact_id")]
        path: Option<String>,
        /// Delete this reply (nested in the comment) instead of the comment.
        #[arg(long)]
        reply: Option<String>,
        /// Confirm the permanent deletion.
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// R9 — re-point a comment's anchor after the anchored element moved
    /// or was renamed during an edit. An explicit, ground-truth override
    /// of the frozen original anchor (the indexer's fuzzy resolver is left
    /// untouched). Use after editing the HTML when an open comment's target
    /// moved — distinct from `resolve`, which marks a comment addressed.
    Reanchor {
        /// The comment id to re-point (from `kb comments list`/`watch`).
        comment_id: String,
        /// kb name. Optional when only one kb is configured.
        #[arg(long)]
        kb: Option<String>,
        /// 12-hex artifact id. Conflicts with `--path`.
        #[arg(long)]
        artifact_id: Option<String>,
        #[arg(long, conflicts_with = "artifact_id")]
        path: Option<String>,
        /// The new anchor: file | chapter:PATH | section:ID |
        /// selection:CSS:OFFSET:SNIPPET (same grammar as `add --anchor`).
        #[arg(long)]
        anchor: String,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Y-track — stage attachment(s) WITHOUT adopting them, printing the
    /// inline `attachment:<aid>` token for each (splice into a `--body`).
    /// Target the artifact via `--path` or `--artifact-id` (+ optional `--kb`).
    Upload {
        /// Local file path(s) to upload.
        #[arg(required = true)]
        files: Vec<String>,
        /// kb name. Optional when only one kb is configured.
        #[arg(long)]
        kb: Option<String>,
        /// 12-hex artifact id. Conflicts with `--path`.
        #[arg(long)]
        artifact_id: Option<String>,
        /// Source-relative path / unique filename, resolved via /lookup.
        #[arg(long, conflicts_with = "artifact_id")]
        path: Option<String>,
        /// Emit the staged Attachment JSON instead of the human summary.
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Y-track — upload + adopt attachment(s) onto an existing comment
    /// (or a reply, with `--reply <rid>`). Prints each inline ref token.
    Attach {
        /// The comment id to attach to.
        comment_id: String,
        /// Local file path(s) to attach.
        #[arg(required = true)]
        files: Vec<String>,
        /// kb name. Optional when only one kb is configured.
        #[arg(long)]
        kb: Option<String>,
        /// 12-hex artifact id. Conflicts with `--path`.
        #[arg(long)]
        artifact_id: Option<String>,
        #[arg(long, conflicts_with = "artifact_id")]
        path: Option<String>,
        /// Attach to this reply (nested in the comment) instead of the comment.
        #[arg(long)]
        reply: Option<String>,
        /// Emit the adopted Attachment JSON instead of the human summary.
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum MemoryAction {
    /// MI-W2.4a — walk one supersede chain, both directions, with
    /// timestamps and forgotten-state. The endorsed salvage from the
    /// 2026-07 temporal-query design in place of a rejected `recall
    /// --as-of` (kb forget's pre-W2.3 hard delete made that answer
    /// undetectably incomplete — see the EPOCH HONESTY caveat this prints
    /// when the chain predates the tombstone era).
    Log {
        /// Artifact id (the 12-hex canonical id `kb docs`/`kb search` print).
        id: String,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// CT-B2 — every session that recalled this memory: the memory-side
    /// reverse of the `memory_recalls` ledger. Fans out across the whole
    /// daemon (the ledger lives with the RECALLING session's kb, not
    /// necessarily this memory's own). Read-only.
    RecalledBy {
        /// Memory (artifact) id.
        id: String,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// MI-W3.1 — on-demand cross-corpus duplicate report: likely-redundant
    /// memory PAIRS (high embedding similarity, not already linked by
    /// `kb-supersedes`, neither forgotten), fanned out across every memory
    /// corpus on the daemon. NOT a contradiction detector (see the route's
    /// doc comment for why) and NEVER mutates anything — resolve a real
    /// duplicate with `kb remember --supersedes` or `kb forget`.
    Dupes {
        /// Cosine similarity floor. Default 0.90 — see the route doc
        /// comment (`kb_core::memory::find_duplicate_pairs`) for why.
        #[arg(long)]
        threshold: Option<f32>,
        #[arg(long)]
        limit: Option<usize>,
        /// Restrict the scan to ONE memory corpus (disables cross-corpus
        /// comparison — there's only one corpus in scope).
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// MI-W4.4 — the bounded, DERIVED hygiene queue: the memories most
    /// worth 90 seconds right now, each with a one-line justification.
    /// Read-only; never mutates anything (act on an item with `pin`,
    /// `salience`, `remember --supersedes`, or `forget`).
    Triage {
        /// Restrict the scan to ONE memory corpus.
        #[arg(long)]
        kb: Option<String>,
        /// Queue size. Default `kb_core::triage::DEFAULT_QUEUE_SIZE` (~10).
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// CT-B4 — from a highlight-born memory's one-liner back to the origin
    /// passage it was lifted from. Reuses kb-comments/1's EXISTING anchor
    /// resolution ladder (never a second heuristic) against the origin
    /// artifact's CURRENT source; a memory with no recorded origin, a gone
    /// origin artifact, or a stale anchor each render an honest line —
    /// never a guessed passage.
    Expand {
        /// The memory's artifact id (the 12-hex id `kb recall`/`kb memory
        /// census` print).
        id: String,
        /// The memory corpus the id lives in. Without it, every
        /// memory-scoped kb on the daemon is searched.
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// CT-C1 — the missing in-session correction verb: an agent that
    /// discovers a recalled memory is WRONG posts an ordinary `[kb-flag]`
    /// comment (kb-comments/1, invariant #6) rather than silently ignoring
    /// it or reaching for a full `remember --supersedes`/`forget`. Refuses
    /// when an OPEN flag already exists on this memory (resolve it first).
    Flag {
        /// Memory (artifact) id.
        id: String,
        /// Why this memory is wrong.
        #[arg(long)]
        reason: String,
        /// The memory corpus the id lives in. Without it, every
        /// memory-scoped kb on the daemon is searched.
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum FleetAction {
    /// Status — health sweep across every daemons.toml entry (v0.24 T1):
    /// identity (version/build/uptime) + per-kb docs and open errors.
    /// Unreachable daemons are report rows, not failures.
    Status {
        /// Emit the raw per-daemon JSON array instead of the table.
        #[arg(long)]
        json: bool,
    },
    /// Replicate — diff artifact ids across every configured daemon
    /// for the given kb. Prints per-daemon `have / missing` counts +
    /// the missing-id lists. Optional `--copy-to <DIR>` downloads each
    /// missing-anywhere artifact into a local directory keyed by id
    /// (the operator's transport — rsync/scp/manual — moves it into
    /// the replica's source path).
    Replicate {
        #[arg(long)]
        kb: String,
        /// Daemon name (from daemons.toml) to use as the canonical
        /// source when `--copy-to` is set. Defaults to "first
        /// reachable daemon that holds the doc".
        #[arg(long)]
        src: Option<String>,
        /// Local directory to write `<id>.html` files into for every
        /// artifact missing on at least one daemon. Created if absent.
        #[arg(long)]
        copy_to: Option<PathBuf>,
    },
}

/// CT-F5 — the three SLO verbs. `--kb` is optional everywhere: it resolves
/// through `http::resolve_default_kb`, so a single-kb daemon needs no flag.
#[derive(Subcommand, Debug, Clone)]
enum SloAction {
    /// Current indicators for one corpus (`GET /api/kb/{kb}/slo`). Reads
    /// only; nothing is written and nothing is judged beyond printing the
    /// status each target implies.
    Status {
        #[arg(long)]
        kb: Option<String>,
        /// Force HTTP against this daemon URL.
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Append one reading to the append-only snapshot log. Every run lands
    /// — there is no skip-if-unchanged, because a flat line is itself the
    /// signal. The daemon never snapshots on its own; this verb (or an
    /// operator's cron) is the only writer.
    Snapshot {
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Read the append-only snapshot log, newest first.
    Log {
        #[arg(long)]
        kb: Option<String>,
        /// Rows to return (server clamps to 1..=1000).
        #[arg(long, default_value_t = 100)]
        limit: u32,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum AtlasAction {
    /// Trigger an atlas recompute on the daemon and tail SSE for the
    /// `atlas.recompute.complete` frame.
    Recompute {
        #[arg(long)]
        kb: String,
        /// Force HTTP against this daemon URL.
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Recluster only — re-runs k-means on existing atlas coords
    /// without touching the UMAP/PCA layout. Cheap (no O(n²) KNN);
    /// rebalances cluster colors when `--k` differs from the default.
    Recluster {
        #[arg(long)]
        kb: String,
        /// Cluster count. Defaults to the kb's resolved √n (capped
        /// at `MAX_CLUSTERS = 12`).
        #[arg(long)]
        k: Option<usize>,
        /// Force HTTP against this daemon URL.
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Labels — deterministic c-TF-IDF top terms per atlas cluster (W1.B),
    /// last refreshed at the most recent recompute/recluster.
    Labels {
        #[arg(long)]
        kb: String,
        #[arg(long)]
        json: bool,
        /// Force HTTP against this daemon URL.
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Points — the FULL-corpus atlas point set (M-a), fixing the paged
    /// gallery `?projection=atlas` route's silent truncation past its
    /// 200-doc default `limit` on large corpora. One `GET
    /// /api/kb/{kb}/atlas/points` round-trip; memoised server-side per
    /// index generation, so repeat calls are cheap.
    Points {
        #[arg(long)]
        kb: String,
        #[arg(long)]
        json: bool,
        /// Force HTTP against this daemon URL.
        #[arg(long)]
        daemon: Option<String>,
    },
    /// History — the corpus time-lapse frame list (W3 T-b, V0028), newest
    /// first. Frames start EMPTY: nothing in kb retains a past layout or a
    /// past embedding, so there is nothing to show until recomputes
    /// accumulate going forward.
    History {
        #[arg(long)]
        kb: String,
        #[arg(long)]
        json: bool,
        /// Force HTTP against this daemon URL.
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Show — one time-lapse frame's points, Procrustes-aligned
    /// SERVER-SIDE against `--align-to` (default: the newest frame), plus
    /// the fit's residual. Every frame is labelled 'recorded' or
    /// 'reconstructed'.
    Show {
        /// Frame id (from `kb atlas history`).
        id: i64,
        #[arg(long)]
        kb: String,
        /// Align this frame onto another frame's coordinate space.
        /// Defaults to the newest frame.
        #[arg(long = "align-to")]
        align_to: Option<i64>,
        #[arg(long)]
        json: bool,
        /// Force HTTP against this daemon URL.
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Backfill — seed the time-lapse with RECONSTRUCTED frames (W3 T-d):
    /// for each of `--frames` evenly-spaced mtime cut points, today's
    /// embeddings laid out over the docs that existed then. Prints the plan
    /// (each cut point + its doc count) BEFORE any work runs, then the
    /// result. A reconstruction is NOT history — nothing in kb retains a
    /// past layout or a past embedding — and every frame it writes is
    /// stamped `reconstructed` on every surface that shows it.
    Backfill {
        #[arg(long)]
        kb: String,
        /// Cut points to reconstruct (1–12; server default 8). Each frame is
        /// a full layout pass over its subset, so this is a direct
        /// multiplier on the most expensive per-kb job.
        #[arg(long)]
        frames: Option<u32>,
        #[arg(long)]
        json: bool,
        /// Force HTTP against this daemon URL.
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Prune — explicit operator retention: drop all but the newest
    /// `--keep` frames. The insert path already self-prunes to
    /// `DEFAULT_ATLAS_FRAMES_KEEP` (24) on every recompute/recluster; this
    /// is for an operator who wants a tighter bound right now.
    Prune {
        #[arg(long)]
        kb: String,
        /// Frames to retain (newest-first). Required — there is no
        /// sensible implicit default for an explicit prune.
        #[arg(long)]
        keep: u32,
        #[arg(long)]
        json: bool,
        /// Force HTTP against this daemon URL.
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Field — the dual-field atlas's operator half (W3 F-b): a JSON
    /// Canvas overlay the operator hand-positions, sitting alongside the
    /// machine layout above. `kb atlas field` prints the raw canvas;
    /// `kb atlas field set --file <path|->` replaces it; `kb atlas field
    /// diff` scores where the two layouts disagree. Geometry lives in the
    /// corpus as `atlas/operator.canvas` — ONE sidecar per kb, not part of
    /// any reading list or board. Unlike `Board`/`BoardAction`, the bare
    /// (show) form takes no positional at all, so `action` is a plain
    /// `Option<_>` with its own `--kb`/`--json`/`--daemon` living directly
    /// on this variant — no `external_subcommand` catch-all needed (that
    /// trick exists only to disambiguate a bare positional from an unknown
    /// subcommand name, which doesn't arise here).
    Field {
        #[command(subcommand)]
        action: Option<AtlasFieldAction>,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        json: bool,
        /// Force HTTP against this daemon URL.
        #[arg(long)]
        daemon: Option<String>,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum BenchAction {
    /// Scaffold a `queries.jsonl` template by sampling N artifacts at
    /// random from a corpus dir. Writes lines with an empty `query`
    /// field and a `relevant` array pre-seeded with the sampled
    /// artifact id — the operator edits the file by hand, replacing
    /// the empty query with the search phrase they want to evaluate
    /// and (optionally) extending the relevant array.
    Init {
        /// Corpus directory (the kb's `path` in kb.toml).
        #[arg(long)]
        corpus: PathBuf,
        /// Output file path. Must not exist (the bench refuses to
        /// overwrite labelled query sets accidentally; delete the
        /// file by hand if you want to re-scaffold).
        #[arg(long)]
        output: PathBuf,
        /// How many queries to scaffold. The operator may add more
        /// by hand later; the bench doesn't care how many lines the
        /// final file has.
        #[arg(long, default_value_t = 40)]
        n: usize,
        /// RNG seed for the random sample. Identical seeds produce
        /// identical scaffolds (useful when several labellers want
        /// to start from the same set).
        #[arg(long, default_value_t = 0xb33f)]
        seed: u64,
    },
    /// Drive `kb search` against a live daemon and print the top hits
    /// (id + title + path) so the operator can pick which artifact
    /// ids belong in a query's `relevant` array. Read-only.
    Discover {
        /// The kb to query (must exist on the daemon).
        #[arg(long)]
        kb: String,
        /// The search query.
        #[arg(long, short = 'q')]
        query: String,
        /// Number of hits to print.
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Search mode (hybrid / semantic / keyword).
        #[arg(long, default_value = "hybrid")]
        mode: String,
        /// Override daemon URL. Defaults to the daemon resolved from
        /// `~/.config/kb/daemons.toml` or `http://127.0.0.1:4000`.
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Drive the bake-off: load a queries.jsonl, run each query
    /// against every (kb × mode), compute Recall@{1,5,10}, MRR, and
    /// nDCG@10, write a markdown + json report side-by-side. Refuses
    /// to run if any query row has an empty `query` field.
    Run {
        /// Path to the labelled queries.jsonl file. One row = one
        /// query + relevant-ids array (see `kb bench init`).
        #[arg(long)]
        queries: PathBuf,
        /// Comma-separated kb names to drive. Each kb must already be
        /// up on the daemon. Example:
        /// `--kbs research-small,research-base,research-large`.
        #[arg(long, value_delimiter = ',')]
        kbs: Vec<String>,
        /// Comma-separated search modes. Defaults to hybrid+semantic
        /// so the operator can compare embedding-only vs full-fusion
        /// performance per model.
        #[arg(long, value_delimiter = ',', default_value = "hybrid,semantic")]
        modes: Vec<String>,
        /// k for Recall@k (multi-value: `--k 1 --k 5 --k 10`).
        /// nDCG is always computed at the largest k.
        #[arg(long, default_values_t = [1u32, 5, 10])]
        k: Vec<u32>,
        /// Override daemon URL.
        #[arg(long)]
        daemon: Option<String>,
        /// Markdown report path. Empty = print to stdout.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Machine-readable JSON report path. Empty = skip.
        #[arg(long)]
        json: Option<PathBuf>,
        /// Prior `--json` report to diff against. When set, every metric
        /// cell in the markdown report gains a signed Δ-from-baseline
        /// annotation (e.g. `0.812 (+0.031)`) — the gate for shipping a
        /// relevance change.
        #[arg(long)]
        baseline: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum ModelAction {
    /// Print the model registry: name, dim, license, status (downloaded /
    /// remote), used-by which kbs.
    List,
    /// Download a model into the XDG cache. Triggers fastembed's hf-hub
    /// pull on first call (~130 MB for bge-small).
    Download {
        name: String,
        /// Air-gapped install from a pre-downloaded tarball. v0.2.
        #[arg(long)]
        from: Option<PathBuf>,
    },
    /// Set the embedding model for a kb (writes to kb.toml).
    Set {
        name: String,
        #[arg(long)]
        kb: Option<String>,
        /// v0.3 G4 — when the new model has the SAME embedding
        /// dimension as the old one, prime the lance table by NULLing
        /// the embedding column (per-row UPDATE, no table drop). The
        /// indexer's next pass repopulates them. Different-dim swaps
        /// still require a full reindex; the daemon's `Storage::open`
        /// enforces dim agreement between disk and the configured
        /// model, so a different-dim swap would refuse to open the
        /// kb until either the dataset is dropped or the config is
        /// reverted. The CLI surfaces a clear error.
        #[arg(long)]
        in_place: bool,
    },
    /// Remove a model from the cache. Refuses if any kb references it
    /// (override with --force; those kbs lose hybrid+semantic search).
    Rm {
        name: String,
        #[arg(long)]
        force: bool,
    },
}

/// W2.15b — `kb proposals` subcommands. Bare `kb proposals` (no
/// subcommand) defaults to `list`, mirroring `kb daemon`/`kb config`.
#[derive(Subcommand, Debug)]
enum ProposalsAction {
    /// List queued proposals (`GET /api/proposals`), fleet-wide unless
    /// `--kb` narrows it. This is the default when no subcommand is given.
    List {
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        daemon: Option<String>,
    },
    /// Approve a proposal — writes the memory (the exact `kb remember`
    /// path) and removes it from the queue.
    Approve {
        id: String,
        /// The kb the proposal lives in (else the sole configured kb).
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Reject (discard) a proposal — removes it from the queue, no memory
    /// is written.
    Reject {
        id: String,
        #[arg(long)]
        kb: Option<String>,
        #[arg(long)]
        daemon: Option<String>,
    },
}

/// SL3 — the flags every `kb slate` verb shares (design §9 "CLI (the
/// protocol)"). `global = true` so they may be written before OR after the
/// verb: the hooks call `kb slate open --hybrid --budget 2000
/// --session-id … --cwd … --json`, and an agent typing
/// `kb slate found "…" --ref path:x` must work the same way.
#[derive(Args, Debug)]
pub(crate) struct SlateCommon {
    /// The slate's slug. Default: the git MAIN checkout's basename for
    /// `--cwd` (never `--show-toplevel` — a linked worktree must not
    /// fragment into its own slate).
    #[arg(long, global = true)]
    slate: Option<String>,
    /// Derive the slug (and stamp `prov.cwd`) from this directory instead
    /// of the process cwd. Hooks pass the session's real cwd.
    #[arg(long, global = true)]
    cwd: Option<PathBuf>,
    /// Topic for this post. On `open` it also DECLARES the session topic
    /// (written to `~/.cache/kb/slate-topic-<sid>`), which later posting
    /// verbs then default to. On a read it is a `?topic=` filter.
    #[arg(long, global = true)]
    topic: Option<String>,
    /// Transcript session id. Ladder: this flag > `$KB_SESSION_ID` >
    /// `~/.cache/kb/current-session`. NEVER a job id (invariant #11).
    #[arg(long = "session-id", global = true)]
    session_id: Option<String>,
    /// claude | codex | opencode | grok | kimi | omp. Default `$KB_HARNESS`
    /// else `claude`; an unknown value is SL2's 400.
    #[arg(long, global = true)]
    harness: Option<String>,
    /// Model id to record on the post's provenance.
    #[arg(long, global = true)]
    model: Option<String>,
    /// agent (default) | human | import. Client-declared, never verified.
    #[arg(long, global = true)]
    origin: Option<String>,
    /// `--as you` — post as the operator (`origin: human`). Required for
    /// `pin`/`unpin`.
    #[arg(long = "as", global = true)]
    as_who: Option<String>,
    /// Dispatcher job id: rides `prov.job_id` plus a `job:<ulid>` ref, and
    /// implies `--origin import`.
    #[arg(long, global = true)]
    job: Option<String>,
    /// Typed ref, repeatable (≤8): path:… | kb:<kb>/<id> | mem:<id> |
    /// session:<id> | job:<ulid> | commit:<sha> | post:#n | plan:<f>#<a>.
    #[arg(long = "ref", global = true)]
    r#ref: Vec<String>,
    /// The post this one is about (`answer`/`done`/`drop`/`mark` set it
    /// from their positional; this is the escape hatch).
    #[arg(long, global = true)]
    re: Option<u64>,
    /// Supersede this post (a change, same kind — `edit` is the sugar).
    #[arg(long, global = true)]
    supersedes: Option<u64>,
    /// Markdown body (`-` reads stdin). Everything after a line's first
    /// newline becomes the body when this is absent.
    #[arg(long, global = true)]
    body: Option<String>,
    #[arg(long, global = true)]
    daemon: Option<String>,
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand, Debug)]
enum SlateAction {
    /// The digest — read this first, and again after /compact.
    Open {
        /// Character budget (never tokens). Default 6000, or 2000 with
        /// `--hybrid`.
        #[arg(long)]
        budget: Option<usize>,
        /// No budget truncation at all.
        #[arg(long)]
        all: bool,
        /// The session-start block: NOW/WARN/unacknowledged HAND in full,
        /// counts for the rest.
        #[arg(long)]
        hybrid: bool,
    },
    /// Unfold one post: its body, resolved refs and the thread beneath it.
    Show { post: String },
    /// Posts and hides since your cursor (the per-prompt hook lane).
    Delta {
        #[arg(long)]
        since: Option<u64>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        budget: Option<usize>,
        /// D26 (v0.42) — narrow to these kinds, csv (e.g.
        /// `now,warn,hand,ask,answer`, the push adapters' hybrid subset).
        /// An unknown word is the server's 400 `bad-kind`.
        #[arg(long)]
        kinds: Option<String>,
    },
    /// What was dropped or edited away, by whom, why.
    History {
        #[arg(long)]
        since: Option<u64>,
        #[arg(long)]
        limit: Option<usize>,
    },
    /// D27 (v0.42) — explicitly report a cursor for adapters that don't
    /// route through `open`/`delta`. Defaults `--seq` to the local cursor
    /// marker's value.
    Cursor {
        #[arg(long)]
        seq: Option<u64>,
    },
    /// The pinned status line for a topic (only the session driving the
    /// milestone rewrites it).
    Now {
        #[arg(required = true, num_args = 1..)]
        line: Vec<String>,
    },
    /// A standing rule for everyone on this project. Never ages off.
    Warn {
        #[arg(required = true, num_args = 1..)]
        line: Vec<String>,
    },
    /// Claim a path or a task. `<subject>` may be `#n` to accept a hand.
    Take {
        subject: String,
        #[arg(required = true, num_args = 1..)]
        line: Vec<String>,
        /// Post it CONTESTED anyway (the holder is told in their delta).
        #[arg(long)]
        anyway: bool,
        /// Reclaim a stale take. A live one still 409s.
        #[arg(long)]
        over: Option<u64>,
    },
    /// Close a post with its outcome.
    Done {
        post: String,
        #[arg(required = true, num_args = 1..)]
        line: Vec<String>,
        /// Leave the target OPEN with your state attached instead.
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        abandoned: Option<String>,
    },
    /// Offer work to whoever picks it up (the what/where/next/what-is-red
    /// packet; everything after the first newline becomes the body).
    Hand {
        subject: String,
        #[arg(required = true, num_args = 1..)]
        line: Vec<String>,
        /// Suggest a harness.
        #[arg(long)]
        to: Option<String>,
    },
    /// An open question. Must end in `?`.
    Ask {
        #[arg(required = true, num_args = 1..)]
        line: Vec<String>,
    },
    /// Answer an ask.
    Answer {
        post: String,
        #[arg(required = true, num_args = 1..)]
        line: Vec<String>,
    },
    /// A verified fact. Needs at least one `--ref` (post a guess as `idea`).
    Found {
        #[arg(required = true, num_args = 1..)]
        line: Vec<String>,
    },
    /// A hypothesis — no ref required, and never asserted as fact.
    Idea {
        #[arg(required = true, num_args = 1..)]
        line: Vec<String>,
    },
    /// A dead end, so nobody repeats it.
    Tried {
        #[arg(required = true, num_args = 1..)]
        line: Vec<String>,
        /// What went wrong.
        #[arg(long)]
        failed: Option<String>,
        /// This retires idea/found #n — appends `(was #n)` to the line and
        /// drops nothing.
        #[arg(long)]
        was: Option<u64>,
    },
    /// Remove a post (an attributed tombstone; `history` keeps the record).
    Drop {
        post: String,
        #[arg(required = true, num_args = 1..)]
        line: Vec<String>,
        /// Required to drop a LIVE other session's now/warn/take/unack hand.
        #[arg(long)]
        anyway: bool,
    },
    /// Change a post: a superseding post of the same kind.
    Edit {
        post: String,
        #[arg(required = true, num_args = 1..)]
        line: Vec<String>,
        #[arg(long)]
        anyway: bool,
    },
    /// Circle someone else's post (+1, once per session per post).
    Mark {
        post: String,
        #[arg(num_args = 0..)]
        line: Vec<String>,
        /// Operator only (`--as you`).
        #[arg(long, conflicts_with = "unpin")]
        pin: bool,
        /// Operator only (`--as you`).
        #[arg(long)]
        unpin: bool,
    },
    /// Pin a post so it is never truncated or displaced (operator only).
    Pin { post: String },
    /// Remove a pin (operator only).
    Unpin { post: String },
    /// Move a lesson somewhere durable, then `done` the post.
    Promote {
        post: String,
        /// memory | note | plan.
        #[arg(long)]
        to: String,
        /// Plan file for `--to plan`. Default `$KB_PLAN_FILE`.
        #[arg(long)]
        plan: Option<PathBuf>,
        /// Memory corpus for `--to memory` (else the usual resolution).
        #[arg(long)]
        kb: Option<String>,
        /// Which note `--to note` appends to (id / path / unique filename).
        #[arg(long)]
        note: Option<String>,
    },
    /// Freeze the slate with a final NOW line.
    Close { line: Vec<String> },
    /// Unfreeze a closed slate (appends nothing).
    Reopen,
    /// Archive this generation's ledger and start a fresh one.
    Rotate,
    /// Follow `slate.updated` and print other sessions' new posts.
    Watch {
        /// Exit after the first surfaced post.
        #[arg(long)]
        once: bool,
        /// Idle timeout in seconds.
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Counts, never a verdict: hands, takes, asks, tried, provenance.
    Stats,
    /// Every slate on this daemon.
    Ls,
    /// Structural lint: contested takes, stale hands, answered-but-open
    /// asks, caps near their limit, token-shaped lines.
    Doctor,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Lance memory tuning, applied before any lance call. Two knobs:
    //
    // - LANCE_BYPASS_SPILLING (any value) — disable the FairSpillPool +
    //   DiskManager and let DataFusion run unbounded (default pool).
    //   Right for a workstation: no spill-to-disk cost, no per-op cap.
    //   Without this, multi-kb parallel merge_inserts saturate the
    //   shared 100MB default pool and fail with "Resources exhausted:
    //   Failed to allocate additional N MB for ExternalSorter[0]".
    //
    // - LANCE_MEM_POOL_SIZE (bytes) — pool size when spilling IS used.
    //   Bumped 100MB → 512MB as a belt for any operator who explicitly
    //   sets LANCE_BYPASS_SPILLING="" to re-enable spilling.
    //
    // Operator overrides win for both (only set when unset).
    //
    // Safe: we run before any spawned tokio worker touches env, and
    // before any lance call. set_var on a single-threaded process is
    // sound; the `unsafe` is for the Rust 1.86+ API.
    if std::env::var_os("LANCE_BYPASS_SPILLING").is_none() {
        unsafe {
            std::env::set_var("LANCE_BYPASS_SPILLING", "1");
        }
    }
    if std::env::var_os("LANCE_MEM_POOL_SIZE").is_none() {
        unsafe {
            std::env::set_var("LANCE_MEM_POOL_SIZE", "536870912");
        }
    }

    let cli = Cli::parse();
    // L1 — the daemon-start path defers init to `commands::daemon::run`,
    // which layers the ndjson file appender (it needs the config-derived
    // state paths, unknown here) on top of the same stderr layer. Every
    // other verb initializes the stderr writer here so `RUST_LOG` works.
    if !matches!(cli.cmd, Cmd::Daemon { action: None }) {
        init_tracing();
    }

    match cli.cmd {
        Cmd::Add {
            path,
            kb,
            embedding_model,
        } => commands::add::run(cli.config.as_ref(), &path, &kb, embedding_model.as_deref()),
        Cmd::Search {
            q,
            kb,
            mode,
            limit,
            category,
            offline,
            daemon,
            json,
            read_from,
            read_to,
        } => {
            let bearer = read_bearer();
            commands::search::run(
                cli.config.as_ref(),
                &q,
                kb.as_deref(),
                &mode,
                limit,
                category.as_deref(),
                offline,
                daemon.as_deref(),
                bearer.as_deref(),
                json,
                read_from.as_deref(),
                read_to.as_deref(),
            )
            .await
        }
        Cmd::Remember {
            text,
            title,
            summary,
            kb,
            scope,
            category,
            tags,
            salience,
            decay,
            supersedes,
            r#type,
            source,
            failed,
            session_id,
            no_session,
            global,
            link,
            daemon,
            json,
        } => {
            let bearer = read_bearer();
            commands::memory::remember(
                &text,
                title.as_deref(),
                summary.as_deref(),
                kb.as_deref(),
                scope.as_deref(),
                &category,
                tags.as_deref(),
                salience,
                decay.as_deref(),
                supersedes.as_deref(),
                r#type.as_deref(),
                source.as_deref(),
                failed,
                session_id.as_deref(),
                no_session,
                global,
                link.as_deref(),
                daemon.as_deref(),
                bearer.as_deref(),
                json,
            )
            .await
        }
        Cmd::Recall {
            query,
            scope,
            project,
            cwd,
            limit,
            for_kb,
            no_floor,
            explain,
            daemon,
            json,
        } => {
            let bearer = read_bearer();
            commands::memory::recall(
                &query,
                &scope,
                project.as_deref(),
                cwd.as_deref(),
                limit,
                for_kb.as_deref(),
                no_floor,
                explain,
                daemon.as_deref(),
                bearer.as_deref(),
                json,
            )
            .await
        }
        Cmd::Context {
            query,
            cwd,
            budget,
            session,
            no_floor,
            daemon,
            json,
        } => {
            let bearer = read_bearer();
            commands::context::context(
                &query,
                cwd.as_deref(),
                budget,
                session.as_deref(),
                no_floor,
                daemon.as_deref(),
                bearer.as_deref(),
                json,
            )
            .await
        }
        Cmd::Why { path, daemon, json } => {
            let bearer = read_bearer();
            commands::sessions::why(&path, daemon.as_deref(), bearer.as_deref(), json).await
        }
        Cmd::WhyMemory {
            id,
            kb,
            daemon,
            json,
        } => {
            let bearer = read_bearer();
            commands::why_memory::why_memory(
                &id,
                kb.as_deref(),
                daemon.as_deref(),
                bearer.as_deref(),
                json,
            )
            .await
        }
        Cmd::Recollect {
            query,
            similar_to,
            folder,
            project,
            since,
            limit,
            daemon,
            json,
            raw,
        } => {
            let bearer = read_bearer();
            commands::sessions::recollect(
                query.as_deref(),
                similar_to.as_deref(),
                folder.as_deref(),
                project.as_deref(),
                since.as_deref(),
                limit,
                daemon.as_deref(),
                bearer.as_deref(),
                json,
                raw,
            )
            .await
        }
        Cmd::Forget {
            id,
            kb,
            purge,
            daemon,
        } => {
            let bearer = read_bearer();
            commands::memory::forget(
                &id,
                kb.as_deref(),
                purge,
                daemon.as_deref(),
                bearer.as_deref(),
            )
            .await
        }
        Cmd::Propose {
            title,
            body,
            kb,
            tags,
            global,
            link,
            salience,
            session_id,
            daemon,
            json,
        } => {
            let bearer = read_bearer();
            commands::proposals::propose(
                &title,
                &body,
                kb.as_deref(),
                tags.as_deref(),
                global,
                link.as_deref(),
                salience,
                session_id.as_deref(),
                daemon.as_deref(),
                bearer.as_deref(),
                json,
            )
            .await
        }
        Cmd::Proposals { action } => {
            let bearer = read_bearer();
            let b = bearer.as_deref();
            match action.unwrap_or(ProposalsAction::List {
                kb: None,
                json: false,
                daemon: None,
            }) {
                ProposalsAction::List { kb, json, daemon } => {
                    commands::proposals::list(kb.as_deref(), json, daemon.as_deref(), b).await
                }
                ProposalsAction::Approve {
                    id,
                    kb,
                    daemon,
                    json,
                } => {
                    commands::proposals::approve(&id, kb.as_deref(), daemon.as_deref(), b, json)
                        .await
                }
                ProposalsAction::Reject { id, kb, daemon } => {
                    commands::proposals::reject(&id, kb.as_deref(), daemon.as_deref(), b).await
                }
            }
        }
        Cmd::Memory { action } => {
            let bearer = read_bearer();
            match action {
                MemoryAction::Log {
                    id,
                    kb,
                    daemon,
                    json,
                } => {
                    commands::memory::log(
                        &id,
                        kb.as_deref(),
                        daemon.as_deref(),
                        bearer.as_deref(),
                        json,
                    )
                    .await
                }
                MemoryAction::RecalledBy {
                    id,
                    kb,
                    daemon,
                    json,
                } => {
                    commands::memory::recalled_by(
                        &id,
                        kb.as_deref(),
                        daemon.as_deref(),
                        bearer.as_deref(),
                        json,
                    )
                    .await
                }
                MemoryAction::Dupes {
                    threshold,
                    limit,
                    kb,
                    daemon,
                    json,
                } => {
                    commands::memory::dupes(
                        threshold,
                        limit,
                        kb.as_deref(),
                        daemon.as_deref(),
                        bearer.as_deref(),
                        json,
                    )
                    .await
                }
                MemoryAction::Triage {
                    kb,
                    limit,
                    daemon,
                    json,
                } => {
                    commands::memory::triage(
                        kb.as_deref(),
                        limit,
                        daemon.as_deref(),
                        bearer.as_deref(),
                        json,
                    )
                    .await
                }
                MemoryAction::Expand {
                    id,
                    kb,
                    daemon,
                    json,
                } => {
                    commands::memory::expand(
                        &id,
                        kb.as_deref(),
                        daemon.as_deref(),
                        bearer.as_deref(),
                        json,
                    )
                    .await
                }
                MemoryAction::Flag {
                    id,
                    reason,
                    kb,
                    daemon,
                    json,
                } => {
                    commands::memory::flag(
                        &id,
                        kb.as_deref(),
                        &reason,
                        daemon.as_deref(),
                        bearer.as_deref(),
                        json,
                    )
                    .await
                }
            }
        }
        Cmd::Read {
            id,
            kb,
            offline,
            daemon,
            no_record,
            record,
        } => {
            let bearer = read_bearer();
            commands::read::run(
                cli.config.as_ref(),
                &id,
                kb.as_deref(),
                offline,
                daemon.as_deref(),
                bearer.as_deref(),
                no_record,
                record,
            )
            .await
        }
        Cmd::Cat {
            id,
            kb,
            offline,
            daemon,
            no_record,
            record,
        } => {
            let bearer = read_bearer();
            commands::cat::run(
                cli.config.as_ref(),
                &id,
                kb.as_deref(),
                offline,
                daemon.as_deref(),
                bearer.as_deref(),
                no_record,
                record,
            )
            .await
        }
        Cmd::Capture {
            files,
            kb,
            title,
            tags,
            sanitize,
            url,
            text,
            name,
            daemon,
            output,
        } => {
            let bearer = read_bearer();
            commands::capture::run(
                &files,
                kb.as_deref(),
                title.as_deref(),
                tags.as_deref(),
                sanitize,
                url.as_deref(),
                text.as_deref(),
                name.as_deref(),
                daemon.as_deref(),
                output.as_deref(),
                bearer.as_deref(),
            )
            .await
        }
        Cmd::Desk { action } => {
            let bearer = read_bearer();
            match action {
                DeskAction::Offer {
                    file,
                    as_slug,
                    kb,
                    title,
                    tags,
                    ttl,
                    sanitize,
                    open,
                    json,
                    daemon,
                } => {
                    commands::desk::offer(
                        &file,
                        &as_slug,
                        kb.as_deref(),
                        title.as_deref(),
                        tags.as_deref(),
                        ttl.as_deref(),
                        sanitize,
                        open,
                        json,
                        daemon.as_deref(),
                        bearer.as_deref(),
                    )
                    .await
                }
                DeskAction::Update {
                    target,
                    file,
                    kb,
                    json,
                    daemon,
                } => {
                    commands::desk::update(
                        &target,
                        &file,
                        kb.as_deref(),
                        json,
                        daemon.as_deref(),
                        bearer.as_deref(),
                    )
                    .await
                }
                DeskAction::Ls {
                    kb,
                    all,
                    json,
                    daemon,
                } => {
                    commands::desk::ls(
                        kb.as_deref(),
                        all,
                        json,
                        daemon.as_deref(),
                        bearer.as_deref(),
                    )
                    .await
                }
                DeskAction::Wait {
                    path,
                    once,
                    timeout,
                    json,
                    kb,
                    daemon,
                } => {
                    commands::desk::wait(
                        path.as_deref(),
                        once,
                        timeout,
                        json,
                        kb.as_deref(),
                        daemon.as_deref(),
                        bearer.as_deref(),
                    )
                    .await
                }
                DeskAction::Expire {
                    target,
                    kb,
                    force,
                    yes,
                    json,
                    daemon,
                } => {
                    commands::desk::expire(
                        &target,
                        kb.as_deref(),
                        force,
                        yes,
                        json,
                        daemon.as_deref(),
                        bearer.as_deref(),
                    )
                    .await
                }
                DeskAction::Promote {
                    target,
                    to,
                    category,
                    keep_draft_tag,
                    kb,
                    json,
                    daemon,
                } => {
                    commands::desk::promote(
                        &target,
                        &to,
                        category.as_deref(),
                        keep_draft_tag,
                        kb.as_deref(),
                        json,
                        daemon.as_deref(),
                        bearer.as_deref(),
                    )
                    .await
                }
            }
        }
        Cmd::Config { action } => match action.unwrap_or(ConfigAction::Show) {
            ConfigAction::Show => commands::config::show(cli.config.as_ref()),
            ConfigAction::Validate => commands::config::validate(cli.config.as_ref()),
            ConfigAction::Edit => commands::config::edit(cli.config.as_ref()),
        },
        Cmd::Daemon { action } => match action {
            None => commands::daemon::run(cli.config.as_ref()).await,
            Some(DaemonAction::Stop) => commands::daemon::stop(cli.config.as_ref()).await,
            Some(DaemonAction::Doctor {
                endpoint,
                json,
                watch,
            }) => {
                let bearer = read_bearer();
                commands::daemon::doctor(endpoint.as_deref(), json, watch, bearer.as_deref()).await
            }
            Some(DaemonAction::LogLevel {
                filter,
                endpoint,
                json,
            }) => {
                let bearer = read_bearer();
                commands::daemon::log_level(
                    endpoint.as_deref(),
                    filter.as_deref(),
                    json,
                    bearer.as_deref(),
                )
                .await
            }
        },
        Cmd::Backup { kb, out } => {
            commands::backup::run(cli.config.as_ref(), &kb, out.as_deref()).await
        }
        Cmd::Restore { tarball, kb, force } => {
            commands::restore::run(cli.config.as_ref(), &kb, &tarball, force)
        }
        Cmd::Metrics { daemon, json } => {
            let bearer = read_bearer();
            commands::metrics::run(daemon.as_deref(), bearer.as_deref(), json).await
        }
        Cmd::Whoami { daemon, output } => {
            let bearer = read_bearer();
            commands::whoami::run(daemon.as_deref(), bearer.as_deref(), output.as_deref()).await
        }
        Cmd::Users { daemon, output } => {
            let bearer = read_bearer();
            commands::users::run(daemon.as_deref(), bearer.as_deref(), output.as_deref()).await
        }
        Cmd::History {
            kb,
            kind,
            user,
            limit,
            daemon,
            json,
        } => {
            let bearer = read_bearer();
            commands::history::run(
                kb.as_deref(),
                kind.as_deref(),
                user.as_deref(),
                limit,
                daemon.as_deref(),
                bearer.as_deref(),
                json,
            )
            .await
        }
        Cmd::Status {
            json,
            watch,
            daemon,
        } => {
            let bearer = read_bearer();
            commands::status::run(
                cli.config.as_ref(),
                json,
                watch,
                daemon.as_deref(),
                bearer.as_deref(),
            )
            .await
        }
        Cmd::Doctor {
            hooks,
            repo,
            daemon,
            json,
            fix,
        } => {
            if !hooks {
                anyhow::bail!("kb doctor needs a mode — pass --hooks (the only mode today)");
            }
            let bearer = read_bearer();
            commands::doctor::hooks(repo, daemon.as_deref(), json, bearer.as_deref(), fix).await
        }
        Cmd::Share(args) => {
            let bearer = read_bearer();
            match args.action {
                Some(ShareAction::List { kb, json, daemon }) => {
                    commands::share::list(kb.as_deref(), json, daemon.as_deref(), bearer.as_deref())
                        .await
                }
                Some(ShareAction::Revoke {
                    name,
                    kb,
                    json,
                    daemon,
                }) => {
                    commands::share::revoke(
                        &name,
                        kb.as_deref(),
                        json,
                        daemon.as_deref(),
                        bearer.as_deref(),
                    )
                    .await
                }
                None => {
                    // Path target and --list are mutually exclusive; one of
                    // them (or a subcommand) is required.
                    match (
                        args.target.as_deref(),
                        args.list.as_deref(),
                        args.local.as_deref(),
                        args.page.as_deref(),
                    ) {
                        (Some(_), Some(_), _, _) => Err(anyhow::anyhow!(
                            "provide either a path target or --list <id-or-title>, not both"
                        )),
                        (None, None, _, _) => Err(anyhow::anyhow!(
                            "provide a target (source-relative file or folder), \
                             `--list <id-or-title>` with `--local`, or a subcommand: \
                             `kb share list` / `kb share revoke <name>`"
                        )),
                        (None, Some(_list), None, _) => Err(anyhow::anyhow!(
                            "--list requires --local <path> (zip file or extract directory)"
                        )),
                        (None, Some(list), Some(local), _) => {
                            commands::share::export_list(
                                list,
                                local,
                                args.kb.as_deref(),
                                &args.links,
                                args.no_scrub,
                                args.with_comments,
                                args.daemon.as_deref(),
                                bearer.as_deref(),
                            )
                            .await
                        }
                        // --page conflicts_with local at clap parse time, so
                        // the (Some, None, Some, Some) arm is unreachable.
                        (Some(target), None, _, Some(page)) => {
                            commands::share::export_page(
                                target,
                                page,
                                args.kb.as_deref(),
                                args.no_scrub,
                                args.daemon.as_deref(),
                                bearer.as_deref(),
                            )
                            .await
                        }
                        (Some(target), None, Some(local), None) => {
                            commands::share::export_local(
                                target,
                                local,
                                args.kb.as_deref(),
                                &args.links,
                                args.no_scrub,
                                args.with_comments,
                                args.daemon.as_deref(),
                                bearer.as_deref(),
                            )
                            .await
                        }
                        (Some(target), None, None, None) => {
                            commands::share::create(
                                target,
                                args.kb.as_deref(),
                                &args.host,
                                &args.gate,
                                args.public,
                                &args.links,
                                args.update,
                                args.no_scrub,
                                args.with_comments,
                                args.open,
                                args.json,
                                args.daemon.as_deref(),
                                bearer.as_deref(),
                            )
                            .await
                        }
                    }
                }
            }
        }
        Cmd::List { action } => {
            let bearer = read_bearer();
            let b = bearer.as_deref();
            match action {
                ListAction::Create {
                    title,
                    description,
                    pin,
                    kb,
                    json,
                    daemon,
                } => {
                    commands::list::create(
                        &title,
                        description.as_deref(),
                        pin,
                        kb.as_deref(),
                        json,
                        daemon.as_deref(),
                        b,
                    )
                    .await
                }
                ListAction::Ls {
                    kb,
                    archived,
                    json,
                    daemon,
                } => commands::list::ls(kb.as_deref(), archived, json, daemon.as_deref(), b).await,
                ListAction::Show {
                    list,
                    kb,
                    json,
                    daemon,
                } => commands::list::show(&list, kb.as_deref(), json, daemon.as_deref(), b).await,
                ListAction::Add {
                    list,
                    target,
                    section,
                    note,
                    before,
                    after,
                    kb,
                    json,
                    daemon,
                } => {
                    commands::list::add(
                        &list,
                        &target,
                        section.as_deref(),
                        note.as_deref(),
                        before.as_deref(),
                        after.as_deref(),
                        kb.as_deref(),
                        json,
                        daemon.as_deref(),
                        b,
                    )
                    .await
                }
                ListAction::Rm {
                    list,
                    entry,
                    kb,
                    daemon,
                } => commands::list::rm(&list, &entry, kb.as_deref(), daemon.as_deref(), b).await,
                ListAction::Update {
                    list,
                    entry,
                    note,
                    clear_note,
                    section,
                    clear_section,
                    read,
                    unread,
                    clear_read,
                    kb,
                    json,
                    daemon,
                } => {
                    commands::list::update(
                        &list,
                        &entry,
                        note.as_deref(),
                        clear_note,
                        section.as_deref(),
                        clear_section,
                        read,
                        unread,
                        clear_read,
                        kb.as_deref(),
                        json,
                        daemon.as_deref(),
                        b,
                    )
                    .await
                }
                ListAction::Move {
                    list,
                    entry,
                    before,
                    after,
                    to,
                    kb,
                    daemon,
                } => {
                    commands::list::mv(
                        &list,
                        &entry,
                        before.as_deref(),
                        after.as_deref(),
                        to,
                        kb.as_deref(),
                        daemon.as_deref(),
                        b,
                    )
                    .await
                }
                ListAction::Rename {
                    list,
                    new_title,
                    kb,
                    daemon,
                } => {
                    commands::list::rename(&list, &new_title, kb.as_deref(), daemon.as_deref(), b)
                        .await
                }
                ListAction::Edit {
                    list,
                    description,
                    clear_description,
                    pin,
                    unpin,
                    archive,
                    unarchive,
                    kb,
                    daemon,
                } => {
                    commands::list::edit(
                        &list,
                        description.as_deref(),
                        clear_description,
                        pin,
                        unpin,
                        archive,
                        unarchive,
                        kb.as_deref(),
                        daemon.as_deref(),
                        b,
                    )
                    .await
                }
                ListAction::Delete {
                    list,
                    yes,
                    kb,
                    daemon,
                } => commands::list::delete(&list, yes, kb.as_deref(), daemon.as_deref(), b).await,
                ListAction::Prune {
                    list,
                    yes,
                    kb,
                    daemon,
                } => commands::list::prune(&list, yes, kb.as_deref(), daemon.as_deref(), b).await,
                ListAction::Reanchor {
                    list,
                    entry,
                    section,
                    kb,
                    daemon,
                } => {
                    commands::list::reanchor(
                        &list,
                        &entry,
                        &section,
                        kb.as_deref(),
                        daemon.as_deref(),
                        b,
                    )
                    .await
                }
                ListAction::Import {
                    file,
                    into,
                    mode,
                    format,
                    dry_run,
                    kb,
                    json,
                    daemon,
                } => {
                    commands::list::import(
                        &file,
                        kb.as_deref(),
                        into.as_deref(),
                        &mode,
                        format.as_deref(),
                        dry_run,
                        json,
                        daemon.as_deref(),
                        b,
                    )
                    .await
                }
                ListAction::Export {
                    list,
                    format,
                    out,
                    kb,
                    daemon,
                } => {
                    commands::list::export(
                        &list,
                        format.as_deref(),
                        out.as_deref(),
                        kb.as_deref(),
                        daemon.as_deref(),
                        b,
                    )
                    .await
                }
            }
        }
        Cmd::Board { action } => {
            let bearer = read_bearer();
            let b = bearer.as_deref();
            match action {
                BoardAction::Set {
                    list,
                    file,
                    kb,
                    daemon,
                } => commands::board::set(&list, &file, kb.as_deref(), daemon.as_deref(), b).await,
                BoardAction::Show(raw) => {
                    let (list, kb, json, daemon) = commands::board::parse_show_args(&raw)?;
                    commands::board::show(&list, kb.as_deref(), json, daemon.as_deref(), b).await
                }
            }
        }
        Cmd::Notes { action } => match action {
            NotesAction::List {
                kb,
                folder,
                status,
                json,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::notes::list(
                    kb.as_deref(),
                    folder.as_deref(),
                    status.as_deref(),
                    json,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            NotesAction::Show {
                target,
                kb,
                json,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::notes::show(
                    kb.as_deref(),
                    &target,
                    json,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            NotesAction::New {
                kb,
                folder,
                title,
                body,
                stdin,
                tags,
                status,
                notepad,
                no_session,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::notes::new(
                    kb.as_deref(),
                    folder.as_deref(),
                    title.as_deref(),
                    body.as_deref(),
                    stdin,
                    &tags,
                    status.as_deref(),
                    notepad,
                    no_session,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            NotesAction::Edit {
                target,
                title,
                body,
                stdin,
                status,
                tags,
                kb,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::notes::edit(
                    kb.as_deref(),
                    &target,
                    title.as_deref(),
                    body.as_deref(),
                    stdin,
                    status.as_deref(),
                    &tags,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            NotesAction::Check {
                target,
                item,
                kb,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::notes::check(
                    kb.as_deref(),
                    &target,
                    item,
                    false,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            NotesAction::Uncheck {
                target,
                item,
                kb,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::notes::check(
                    kb.as_deref(),
                    &target,
                    item,
                    true,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            NotesAction::Append {
                target,
                item,
                kb,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::notes::append(
                    kb.as_deref(),
                    &target,
                    &item,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            NotesAction::Done { target, kb, daemon } => {
                let bearer = read_bearer();
                commands::notes::set_status(
                    kb.as_deref(),
                    &target,
                    "done",
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            NotesAction::Archive { target, kb, daemon } => {
                let bearer = read_bearer();
                commands::notes::set_status(
                    kb.as_deref(),
                    &target,
                    "archived",
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            NotesAction::Rm {
                target,
                yes,
                kb,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::notes::rm(
                    kb.as_deref(),
                    &target,
                    yes,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            NotesAction::Links {
                target,
                kb,
                json,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::notes::links(
                    kb.as_deref(),
                    &target,
                    json,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
        },
        Cmd::Links { action } => match action {
            LinksAction::Suggest {
                kb,
                limit,
                json,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::links::suggest(
                    kb.as_deref(),
                    limit,
                    daemon.as_deref(),
                    bearer.as_deref(),
                    json,
                )
                .await
            }
            LinksAction::Apply {
                src,
                dst,
                kb,
                json,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::links::apply(
                    kb.as_deref(),
                    &src,
                    &dst,
                    daemon.as_deref(),
                    bearer.as_deref(),
                    json,
                )
                .await
            }
        },
        Cmd::Backlinks {
            target,
            kb,
            json,
            daemon,
        } => {
            let bearer = read_bearer();
            commands::notes::backlinks(
                kb.as_deref(),
                &target,
                json,
                daemon.as_deref(),
                bearer.as_deref(),
            )
            .await
        }
        Cmd::Refs {
            target,
            by_target,
            kb,
            json,
            lint,
            limit,
            gallery,
            daemon,
        } => {
            let bearer = read_bearer();
            commands::refs::run(
                kb.as_deref(),
                target.as_deref(),
                by_target.as_deref(),
                json,
                lint,
                limit,
                gallery,
                daemon.as_deref(),
                bearer.as_deref(),
            )
            .await
        }
        Cmd::Versions {
            target,
            at,
            kb,
            json,
            daemon,
        } => {
            let bearer = read_bearer();
            commands::versions::versions(
                &target,
                at.as_deref(),
                kb.as_deref(),
                json,
                daemon.as_deref(),
                bearer.as_deref(),
            )
            .await
        }
        Cmd::Diff {
            target,
            from,
            to,
            between,
            raw,
            kb,
            json,
            daemon,
        } => {
            let bearer = read_bearer();
            match between {
                Some(dates) => {
                    // clap's `num_args = 2` guarantees exactly two.
                    let (d1, d2) = (&dates[0], &dates[1]);
                    commands::versions::diff_between(
                        &target,
                        d1,
                        d2,
                        raw,
                        kb.as_deref(),
                        json,
                        daemon.as_deref(),
                        bearer.as_deref(),
                    )
                    .await
                }
                None => {
                    commands::versions::diff(
                        &target,
                        from.as_deref(),
                        to.as_deref(),
                        raw,
                        kb.as_deref(),
                        json,
                        daemon.as_deref(),
                        bearer.as_deref(),
                    )
                    .await
                }
            }
        }
        Cmd::Prompt {
            id,
            kb,
            raw,
            json,
            daemon,
        } => {
            let bearer = read_bearer();
            commands::prompt::run(
                &id,
                kb.as_deref(),
                raw,
                json,
                daemon.as_deref(),
                bearer.as_deref(),
            )
            .await
        }
        Cmd::Sessions { action } => match action {
            SessionsAction::Capture {
                transcript,
                session_id,
                cwd,
                out,
                json,
                allow_oversized,
            } => {
                commands::sessions_capture::run(
                    transcript,
                    session_id,
                    cwd,
                    out,
                    json,
                    allow_oversized,
                )
                .await
            }
            SessionsAction::List {
                daemon,
                json,
                limit,
                folder,
                project,
                substance,
                harness,
            } => {
                let bearer = read_bearer();
                commands::sessions::list(
                    daemon.as_deref(),
                    bearer.as_deref(),
                    json,
                    limit,
                    folder.as_deref(),
                    None,
                    project.as_deref(),
                    substance.as_deref(),
                    harness.as_deref(),
                )
                .await
            }
            SessionsAction::Folders { daemon, json } => {
                let bearer = read_bearer();
                commands::sessions::folders(daemon.as_deref(), bearer.as_deref(), json).await
            }
            SessionsAction::Rollup {
                folder,
                project,
                substance,
                limit,
                daemon,
                json,
            } => {
                let bearer = read_bearer();
                commands::sessions::rollup(
                    folder.as_deref(),
                    project.as_deref(),
                    substance.as_deref(),
                    limit,
                    daemon.as_deref(),
                    bearer.as_deref(),
                    json,
                )
                .await
            }
            SessionsAction::Funnel {
                folder,
                project,
                substance,
                daemon,
                json,
            } => {
                let bearer = read_bearer();
                commands::sessions::funnel(
                    folder.as_deref(),
                    project.as_deref(),
                    substance.as_deref(),
                    daemon.as_deref(),
                    bearer.as_deref(),
                    json,
                )
                .await
            }
            SessionsAction::Ledger {
                project,
                days,
                daemon,
                json,
            } => {
                let bearer = read_bearer();
                commands::sessions::ledger(
                    project.as_deref(),
                    days,
                    daemon.as_deref(),
                    bearer.as_deref(),
                    json,
                )
                .await
            }
            SessionsAction::Threads { daemon, json } => {
                let bearer = read_bearer();
                commands::sessions::threads(daemon.as_deref(), bearer.as_deref(), json).await
            }
            SessionsAction::SaveThread {
                folder,
                title,
                narrative,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::sessions::save_thread(
                    &folder,
                    title.as_deref(),
                    narrative,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            SessionsAction::Search {
                query,
                folder,
                project,
                substance,
                harness,
                limit,
                daemon,
                json,
            } => {
                let bearer = read_bearer();
                commands::sessions::list(
                    daemon.as_deref(),
                    bearer.as_deref(),
                    json,
                    limit,
                    folder.as_deref(),
                    Some(&query),
                    project.as_deref(),
                    substance.as_deref(),
                    harness.as_deref(),
                )
                .await
            }
            SessionsAction::Of {
                target,
                kb,
                daemon,
                json,
            } => {
                let bearer = read_bearer();
                commands::sessions::of(
                    &target,
                    kb.as_deref(),
                    daemon.as_deref(),
                    bearer.as_deref(),
                    json,
                )
                .await
            }
            SessionsAction::CommitMap {
                since,
                limit,
                offset,
                daemon,
                json,
            } => {
                let bearer = read_bearer();
                commands::sessions::commit_map(
                    since,
                    limit,
                    offset,
                    daemon.as_deref(),
                    bearer.as_deref(),
                    json,
                )
                .await
            }
            SessionsAction::ByJob { ulid, daemon, json } => {
                let bearer = read_bearer();
                commands::sessions::by_job(&ulid, daemon.as_deref(), bearer.as_deref(), json).await
            }
            SessionsAction::Read {
                session_id,
                full,
                tail,
                turn,
                grep,
                context,
                raw,
                live,
                follow,
                daemon,
                json,
                no_color,
                width,
            } => {
                let bearer = read_bearer();
                commands::session_read::run(
                    cli.config.as_ref(),
                    &session_id,
                    full,
                    tail,
                    turn.as_deref(),
                    grep.as_deref(),
                    context,
                    raw,
                    live || follow,
                    follow,
                    json,
                    no_color,
                    width,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            SessionsAction::Status {
                local,
                root,
                state,
                project,
                harness,
                limit,
                json,
                no_color,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::sessions_status::run(
                    cli.config.as_ref(),
                    local,
                    root.as_ref(),
                    json,
                    state.as_deref(),
                    project.as_deref(),
                    harness.as_deref(),
                    limit,
                    no_color,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            SessionsAction::ByCommit { sha, daemon, json } => {
                let bearer = read_bearer();
                commands::sessions::by_commit(&sha, daemon.as_deref(), bearer.as_deref(), json)
                    .await
            }
            SessionsAction::ProvenanceReport { repo, daemon, json } => {
                let bearer = read_bearer();
                commands::sessions::provenance_report(
                    &repo,
                    daemon.as_deref(),
                    bearer.as_deref(),
                    json,
                )
                .await
            }
            SessionsAction::WhyLine {
                file_line,
                repo,
                daemon,
                json,
            } => {
                let bearer = read_bearer();
                commands::sessions::why_line(
                    &file_line,
                    &repo,
                    daemon.as_deref(),
                    bearer.as_deref(),
                    json,
                )
                .await
            }
            SessionsAction::Show {
                session_id,
                daemon,
                json,
                section,
            } => {
                let bearer = read_bearer();
                commands::sessions::show(
                    &session_id,
                    daemon.as_deref(),
                    bearer.as_deref(),
                    json,
                    &section,
                )
                .await
            }
            SessionsAction::Replay {
                session_id,
                artifact,
                from_seq,
                limit,
                daemon,
                json,
            } => {
                let bearer = read_bearer();
                commands::sessions::replay(
                    &session_id,
                    artifact.as_deref(),
                    from_seq,
                    limit,
                    daemon.as_deref(),
                    bearer.as_deref(),
                    json,
                )
                .await
            }
            SessionsAction::Resume {
                session_id,
                daemon,
                json,
            } => {
                let bearer = read_bearer();
                commands::sessions::resume(&session_id, daemon.as_deref(), bearer.as_deref(), json)
                    .await
            }
            SessionsAction::Export {
                session_id,
                from,
                out,
                scrub,
                scrub_paths,
                scrub_entropy,
                yes,
                json,
            } => {
                // Any scrub flag turns on the safe secrets floor; the extra
                // layers are additive.
                let opts = kb_core::session_scrub::ScrubOptions {
                    secrets: scrub || scrub_paths || scrub_entropy,
                    paths: scrub_paths,
                    entropy: scrub_entropy,
                };
                commands::session_bundle::export(&session_id, from, out, opts, yes, json).await
            }
            SessionsAction::Rehydrate {
                bundle,
                cwd,
                dry_run,
                force,
                json,
            } => commands::session_bundle::rehydrate(&bundle, cwd, dry_run, force, json).await,
            SessionsAction::Pull {
                session_id,
                from,
                out,
                rehydrate,
                cwd,
                force,
                json,
            } => {
                let bearer = read_bearer();
                commands::session_bundle::pull(
                    &session_id,
                    &from,
                    out,
                    rehydrate,
                    cwd,
                    force,
                    bearer.as_deref(),
                    json,
                )
                .await
            }
        },
        Cmd::Reading {
            target,
            kb,
            json,
            lite,
            daemon,
        } => {
            let bearer = read_bearer();
            commands::reading::run(
                &target,
                kb.as_deref(),
                json,
                lite,
                daemon.as_deref(),
                bearer.as_deref(),
            )
            .await
        }
        Cmd::Resurface {
            kb,
            limit,
            explain,
            json,
            daemon,
        } => {
            let bearer = read_bearer();
            commands::resurface::run(
                kb.as_deref(),
                limit,
                explain,
                json,
                daemon.as_deref(),
                bearer.as_deref(),
            )
            .await
        }
        Cmd::Timeline {
            kb,
            from,
            to,
            tracks,
            ids,
            json,
            daemon,
        } => {
            let bearer = read_bearer();
            commands::timeline::run(
                kb.as_deref(),
                from.as_deref(),
                to.as_deref(),
                tracks.as_deref(),
                ids,
                json,
                daemon.as_deref(),
                bearer.as_deref(),
            )
            .await
        }
        Cmd::Daycard {
            kb,
            day,
            since,
            html,
            json,
            daemon,
        } => {
            let bearer = read_bearer();
            commands::daycard::run(
                kb.as_deref(),
                day.as_deref(),
                since.as_deref(),
                html,
                json,
                daemon.as_deref(),
                bearer.as_deref(),
            )
            .await
        }
        Cmd::Comments { action } => match action {
            CommentsAction::List {
                kb,
                all,
                json,
                path,
                author,
                user,
                stale,
                folder,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::comments::list(
                    kb.as_deref(),
                    all,
                    json,
                    path.as_deref(),
                    author.as_deref(),
                    user.as_deref(),
                    stale,
                    folder.as_deref(),
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            CommentsAction::Inbox {
                kb,
                limit,
                json,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::comments::inbox(
                    kb.as_deref(),
                    limit,
                    json,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            CommentsAction::Show {
                kb,
                artifact_id,
                kb_flag,
                artifact_id_flag,
                path,
                daemon,
            } => {
                let bearer = read_bearer();
                let (kb, artifact_id, _) = reconcile_comment_target(
                    kb_flag,
                    artifact_id_flag,
                    vec![kb, artifact_id],
                    false,
                );
                commands::comments::show(
                    kb.as_deref(),
                    artifact_id.as_deref(),
                    path.as_deref(),
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            CommentsAction::Export {
                kb,
                artifact_id,
                kb_flag,
                artifact_id_flag,
                path,
                format,
                out_dir,
                embed,
                out,
                daemon,
            } => {
                let bearer = read_bearer();
                let (kb, artifact_id, _) = reconcile_comment_target(
                    kb_flag,
                    artifact_id_flag,
                    vec![kb, artifact_id],
                    false,
                );
                commands::comments::export(
                    kb.as_deref(),
                    artifact_id.as_deref(),
                    path.as_deref(),
                    &format,
                    out_dir.as_deref(),
                    embed,
                    out.as_deref(),
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            CommentsAction::Apply {
                kb,
                artifact_id,
                kb_flag,
                artifact_id_flag,
                path,
                ops_file,
                ops_json,
                daemon,
            } => {
                let bearer = read_bearer();
                let (kb, artifact_id, _) = reconcile_comment_target(
                    kb_flag,
                    artifact_id_flag,
                    vec![kb, artifact_id],
                    false,
                );
                commands::comments::apply(
                    kb.as_deref(),
                    artifact_id.as_deref(),
                    path.as_deref(),
                    ops_file.as_deref(),
                    ops_json.as_deref(),
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            CommentsAction::Import {
                file,
                kb,
                artifact_id,
                kb_flag,
                artifact_id_flag,
                path,
                force,
                daemon,
            } => {
                let bearer = read_bearer();
                let (kb, artifact_id, _) = reconcile_comment_target(
                    kb_flag,
                    artifact_id_flag,
                    vec![kb, artifact_id],
                    false,
                );
                commands::comments::import(
                    kb.as_deref(),
                    artifact_id.as_deref(),
                    path.as_deref(),
                    &file,
                    force,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            CommentsAction::Resolve {
                kb,
                artifact_id,
                comment_id,
                kb_flag,
                artifact_id_flag,
                path,
                all,
                daemon,
            } => {
                let bearer = read_bearer();
                let (kb, artifact_id, comment_id) = reconcile_comment_target(
                    kb_flag,
                    artifact_id_flag,
                    vec![kb, artifact_id, comment_id],
                    true,
                );
                commands::comments::resolve(
                    kb.as_deref(),
                    artifact_id.as_deref(),
                    comment_id.as_deref(),
                    path.as_deref(),
                    all,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            CommentsAction::Unresolve {
                kb,
                artifact_id,
                comment_id,
                kb_flag,
                artifact_id_flag,
                path,
                all,
                daemon,
            } => {
                let bearer = read_bearer();
                let (kb, artifact_id, comment_id) = reconcile_comment_target(
                    kb_flag,
                    artifact_id_flag,
                    vec![kb, artifact_id, comment_id],
                    true,
                );
                commands::comments::unresolve(
                    kb.as_deref(),
                    artifact_id.as_deref(),
                    comment_id.as_deref(),
                    path.as_deref(),
                    all,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            CommentsAction::Reply {
                comment_id,
                kb,
                artifact_id,
                path,
                body,
                choice_json,
                attach,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::comments::reply(
                    kb.as_deref(),
                    artifact_id.as_deref(),
                    &comment_id,
                    path.as_deref(),
                    &body,
                    &choice_json,
                    &attach,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            CommentsAction::Add {
                kb,
                artifact_id,
                kb_flag,
                artifact_id_flag,
                path,
                body,
                anchor,
                author,
                page,
                choice_json,
                attach,
                daemon,
            } => {
                let bearer = read_bearer();
                let (kb, artifact_id, _) = reconcile_comment_target(
                    kb_flag,
                    artifact_id_flag,
                    vec![kb, artifact_id],
                    false,
                );
                commands::comments::add(
                    kb.as_deref(),
                    artifact_id.as_deref(),
                    path.as_deref(),
                    &body,
                    &anchor,
                    &author,
                    &choice_json,
                    page.as_deref(),
                    &attach,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            CommentsAction::Edit {
                comment_id,
                kb,
                artifact_id,
                path,
                reply,
                body,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::comments::edit(
                    kb.as_deref(),
                    artifact_id.as_deref(),
                    &comment_id,
                    reply.as_deref(),
                    path.as_deref(),
                    &body,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            CommentsAction::Verdict {
                state,
                kb,
                artifact_id,
                path,
                note,
                clear,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::comments::verdict(
                    kb.as_deref(),
                    artifact_id.as_deref(),
                    path.as_deref(),
                    state.as_deref(),
                    note.as_deref(),
                    clear,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            CommentsAction::Reanchor {
                comment_id,
                kb,
                artifact_id,
                path,
                anchor,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::comments::reanchor(
                    kb.as_deref(),
                    artifact_id.as_deref(),
                    &comment_id,
                    path.as_deref(),
                    &anchor,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            CommentsAction::Delete {
                comment_id,
                kb,
                artifact_id,
                path,
                reply,
                yes,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::comments::delete(
                    kb.as_deref(),
                    artifact_id.as_deref(),
                    &comment_id,
                    reply.as_deref(),
                    path.as_deref(),
                    yes,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            CommentsAction::Upload {
                files,
                kb,
                artifact_id,
                path,
                json,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::comments::upload(
                    kb.as_deref(),
                    artifact_id.as_deref(),
                    path.as_deref(),
                    &files,
                    json,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            CommentsAction::Attach {
                comment_id,
                files,
                kb,
                artifact_id,
                path,
                reply,
                json,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::comments::attach(
                    kb.as_deref(),
                    artifact_id.as_deref(),
                    &comment_id,
                    reply.as_deref(),
                    path.as_deref(),
                    &files,
                    json,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            CommentsAction::Watch {
                path,
                kb,
                json,
                once,
                timeout,
                backlog,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::comments_watch::run(
                    kb.as_deref(),
                    &path,
                    json,
                    once,
                    timeout,
                    backlog,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
        },
        Cmd::Atlas { action } => match action {
            AtlasAction::Recompute { kb, daemon } => {
                let bearer = read_bearer();
                commands::atlas::recompute(&kb, daemon.as_deref(), bearer.as_deref()).await
            }
            AtlasAction::Recluster { kb, k, daemon } => {
                let bearer = read_bearer();
                commands::atlas::recluster(&kb, k, daemon.as_deref(), bearer.as_deref()).await
            }
            AtlasAction::Labels { kb, json, daemon } => {
                let bearer = read_bearer();
                commands::atlas::labels(&kb, json, daemon.as_deref(), bearer.as_deref()).await
            }
            AtlasAction::Points { kb, json, daemon } => {
                let bearer = read_bearer();
                commands::atlas::points(&kb, json, daemon.as_deref(), bearer.as_deref()).await
            }
            AtlasAction::History { kb, json, daemon } => {
                let bearer = read_bearer();
                commands::atlas::history(&kb, json, daemon.as_deref(), bearer.as_deref()).await
            }
            AtlasAction::Show {
                id,
                kb,
                align_to,
                json,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::atlas::show(
                    &kb,
                    id,
                    align_to,
                    json,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            AtlasAction::Backfill {
                kb,
                frames,
                json,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::atlas::backfill(&kb, frames, json, daemon.as_deref(), bearer.as_deref())
                    .await
            }
            AtlasAction::Prune {
                kb,
                keep,
                json,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::atlas::prune(&kb, keep, json, daemon.as_deref(), bearer.as_deref()).await
            }
            AtlasAction::Field {
                action,
                kb,
                json,
                daemon,
            } => {
                let bearer = read_bearer();
                let b = bearer.as_deref();
                match action {
                    Some(AtlasFieldAction::Set {
                        file,
                        kb: sub_kb,
                        daemon: sub_daemon,
                    }) => {
                        commands::atlas_field::set(
                            &file,
                            sub_kb.as_deref(),
                            sub_daemon.as_deref(),
                            b,
                        )
                        .await
                    }
                    Some(AtlasFieldAction::Diff {
                        kb: sub_kb,
                        limit,
                        json: sub_json,
                        daemon: sub_daemon,
                    }) => {
                        commands::atlas_field::diff(
                            sub_kb.as_deref(),
                            limit,
                            sub_json,
                            sub_daemon.as_deref(),
                            b,
                        )
                        .await
                    }
                    None => {
                        commands::atlas_field::show(kb.as_deref(), json, daemon.as_deref(), b).await
                    }
                }
            }
        },
        Cmd::Token { action } => match action {
            TokenAction::Generate => commands::token::generate(),
            TokenAction::Rotate => commands::token::rotate(),
            TokenAction::Show { print } => commands::token::show(print),
            TokenAction::Path => commands::token::path(),
            TokenAction::Issue { user, force } => commands::token::issue(&user, force),
            TokenAction::Revoke { user } => commands::token::revoke(&user),
        },
        Cmd::Push { filter, daemon } => {
            // Read the bearer token from disk if present (the daemon
            // may be behind a proxy that wants Authorization).
            let bearer = read_bearer();
            commands::push::run(&filter, daemon.as_deref(), bearer.as_deref()).await
        }
        Cmd::Events {
            follow: _,
            types,
            kb,
            artifact,
            json,
            daemon,
        } => {
            let bearer = read_bearer();
            commands::events::follow(commands::events::EventsArgs {
                types: &types,
                kb: kb.as_deref(),
                artifact: artifact.as_deref(),
                daemon: daemon.as_deref(),
                bearer: bearer.as_deref(),
                json,
            })
            .await
        }
        Cmd::Fleet { action } => match action {
            FleetAction::Status { json } => {
                let bearer = read_bearer();
                commands::fleet::status(json, bearer.as_deref()).await
            }
            FleetAction::Replicate { kb, src, copy_to } => {
                let bearer = read_bearer();
                commands::fleet::replicate(
                    &kb,
                    src.as_deref(),
                    copy_to.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
        },
        Cmd::Pull {
            from,
            kb,
            into,
            oidc_token_url,
            oidc_client_id,
            scope,
        } => {
            let bearer = read_bearer();
            commands::pull::run(
                &from,
                &kb,
                &into,
                oidc_token_url.as_deref(),
                oidc_client_id.as_deref(),
                &scope,
                bearer.as_deref(),
            )
            .await
        }
        Cmd::Get {
            id,
            kb,
            format,
            daemon,
            no_record,
        } => {
            let bearer = read_bearer();
            commands::get::run(
                &id,
                kb.as_deref(),
                &format,
                daemon.as_deref(),
                bearer.as_deref(),
                no_record,
            )
            .await
        }
        Cmd::Download {
            target,
            folder,
            all,
            kb,
            out,
            daemon,
        } => {
            let bearer = read_bearer();
            commands::download::run(
                target.as_deref(),
                folder.as_deref(),
                all,
                kb.as_deref(),
                out.as_deref(),
                daemon.as_deref(),
                bearer.as_deref(),
            )
            .await
        }
        Cmd::Graph {
            kb,
            top,
            daemon,
            json,
        } => {
            let bearer = read_bearer();
            commands::graph::run(&kb, top, daemon.as_deref(), bearer.as_deref(), json).await
        }
        Cmd::Slo { action } => {
            let bearer = read_bearer();
            match action {
                SloAction::Status { kb, daemon, json } => {
                    commands::slo::status(kb.as_deref(), daemon.as_deref(), bearer.as_deref(), json)
                        .await
                }
                SloAction::Snapshot { kb, daemon, json } => {
                    commands::slo::snapshot(
                        kb.as_deref(),
                        daemon.as_deref(),
                        bearer.as_deref(),
                        json,
                    )
                    .await
                }
                SloAction::Log {
                    kb,
                    limit,
                    daemon,
                    json,
                } => {
                    commands::slo::log(
                        kb.as_deref(),
                        limit,
                        daemon.as_deref(),
                        bearer.as_deref(),
                        json,
                    )
                    .await
                }
            }
        }
        Cmd::Queries {
            action,
            kb,
            scope,
            zero_hit,
            min_count,
            daemon,
            json,
        } => {
            let bearer = read_bearer();
            let b = bearer.as_deref();
            match action {
                Some(QueriesAction::List {
                    daemon: sub_daemon,
                    json: sub_json,
                }) => commands::queries::list_saved(sub_daemon.as_deref(), b, sub_json).await,
                Some(QueriesAction::Save {
                    name,
                    search,
                    path,
                    daemon: sub_daemon,
                    json: sub_json,
                }) => {
                    commands::queries::save(
                        &name,
                        &path,
                        &search,
                        sub_daemon.as_deref(),
                        b,
                        sub_json,
                    )
                    .await
                }
                Some(QueriesAction::Rm {
                    name,
                    daemon: sub_daemon,
                    json: sub_json,
                }) => commands::queries::rm(&name, sub_daemon.as_deref(), b, sub_json).await,
                None => {
                    commands::queries::run(
                        kb.as_deref(),
                        &scope,
                        zero_hit,
                        min_count,
                        daemon.as_deref(),
                        b,
                        json,
                    )
                    .await
                }
            }
        }
        Cmd::Related {
            id,
            kb,
            depth,
            daemon,
            json,
        } => {
            let bearer = read_bearer();
            commands::related::run(
                &id,
                kb.as_deref(),
                depth,
                daemon.as_deref(),
                bearer.as_deref(),
                json,
            )
            .await
        }
        Cmd::Similar {
            id,
            kb,
            limit,
            daemon,
            json,
        } => {
            let bearer = read_bearer();
            commands::similar::run(
                &id,
                kb.as_deref(),
                limit,
                daemon.as_deref(),
                bearer.as_deref(),
                json,
            )
            .await
        }
        Cmd::Tools => commands::tools::run(),
        Cmd::Find {
            input,
            kb,
            json,
            daemon,
        } => {
            let bearer = read_bearer();
            commands::find::run(
                &input,
                kb.as_deref(),
                json,
                daemon.as_deref(),
                bearer.as_deref(),
            )
            .await
        }
        Cmd::Mv {
            target,
            new_path,
            kb,
            json,
            daemon,
        } => {
            let bearer = read_bearer();
            commands::mv::run(
                &target,
                &new_path,
                kb.as_deref(),
                json,
                daemon.as_deref(),
                bearer.as_deref(),
            )
            .await
        }
        Cmd::Reindex { kb, daemon, json } => {
            let bearer = read_bearer();
            commands::reindex::run(kb.as_deref(), daemon.as_deref(), bearer.as_deref(), json).await
        }
        Cmd::Compact { kb, daemon, json } => {
            let bearer = read_bearer();
            commands::compact::run(kb.as_deref(), daemon.as_deref(), bearer.as_deref(), json).await
        }
        Cmd::Exclude {
            target,
            kb,
            rm,
            list,
            note,
            daemon,
            json,
        } => {
            let bearer = read_bearer();
            commands::exclude::run(
                target.as_deref(),
                kb.as_deref(),
                rm,
                list,
                note,
                daemon.as_deref(),
                bearer.as_deref(),
                json,
            )
            .await
        }
        Cmd::Pause { kb, daemon, json } => {
            let bearer = read_bearer();
            commands::sources::set_paused(
                kb.as_deref(),
                daemon.as_deref(),
                bearer.as_deref(),
                true,
                json,
            )
            .await
        }
        Cmd::Resume { kb, daemon, json } => {
            let bearer = read_bearer();
            commands::sources::set_paused(
                kb.as_deref(),
                daemon.as_deref(),
                bearer.as_deref(),
                false,
                json,
            )
            .await
        }
        Cmd::IndexPage {
            kb,
            filters,
            group_by,
            out,
            template,
            daemon,
            limit,
            title,
        } => {
            let parsed_filters: Result<Vec<(String, String)>> = filters
                .iter()
                .map(|s| commands::index_page::parse_filter(s))
                .collect();
            let parsed_filters = parsed_filters?;
            let bearer = read_bearer();
            commands::index_page::run(
                &kb,
                &parsed_filters,
                &group_by,
                out.as_deref(),
                template.as_deref(),
                daemon.as_deref(),
                bearer.as_deref(),
                limit,
                title.as_deref(),
            )
            .await
        }
        Cmd::New {
            template,
            title,
            out,
            vars,
            kb,
        } => {
            // Parse --var specs early so we error before reading the
            // template if any is malformed.
            let parsed: Result<Vec<(String, String)>> =
                vars.iter().map(|s| commands::new::parse_var(s)).collect();
            let parsed = parsed?;
            commands::new::run(
                cli.config.as_ref(),
                &template,
                &title,
                out.as_deref(),
                &parsed,
                kb.as_deref(),
            )
        }
        Cmd::Reset {
            kb,
            yes,
            all,
            force,
        } => commands::reset::run(cli.config.as_ref(), &kb, yes, all, force),
        Cmd::Synth { docs, out, seed } => commands::synth::run(docs, out, seed),
        Cmd::Import { action } => match action {
            ImportAction::ClaudeHistory {
                dir,
                into,
                dry_run,
                limit,
                json,
                quiet,
                refresh_subagents,
                transcripts_root,
                allow_oversized,
            } => commands::import::run(
                dir,
                into,
                dry_run,
                limit,
                json,
                quiet,
                refresh_subagents,
                transcripts_root,
                allow_oversized,
            ),
        },
        Cmd::Model { action } => {
            let args = match action {
                ModelAction::List => commands::model::ModelArgs::List,
                ModelAction::Download { name, from } => {
                    commands::model::ModelArgs::Download { name, from }
                }
                ModelAction::Set { name, kb, in_place } => {
                    commands::model::ModelArgs::Set { name, kb, in_place }
                }
                ModelAction::Rm { name, force } => commands::model::ModelArgs::Rm { name, force },
            };
            commands::model::run(args, cli.config.as_ref()).await
        }
        Cmd::Slate { common, action } => {
            let bearer = read_bearer();
            let args = commands::slate::CommonArgs {
                slate: common.slate.as_deref(),
                cwd: common.cwd.as_deref(),
                topic: common.topic.as_deref(),
                session_id: common.session_id.as_deref(),
                harness: common.harness.as_deref(),
                model: common.model.as_deref(),
                origin: common.origin.as_deref(),
                as_who: common.as_who.as_deref(),
                job: common.job.as_deref(),
                refs: &common.r#ref,
                re: common.re,
                supersedes: common.supersedes,
                body: common.body.as_deref(),
                daemon: common.daemon.as_deref(),
                bearer: bearer.as_deref(),
                json: common.json,
            };
            let ctx = commands::slate::Ctx::resolve(&args)?;
            // The line positionals are joined with a single space, so a
            // properly quoted line is byte-unchanged and an unquoted one
            // still works (agents forget the quotes).
            let join = |v: Vec<String>| v.join(" ");
            use commands::slate as sl;
            match action {
                SlateAction::Open {
                    budget,
                    all,
                    hybrid,
                } => sl::open(&ctx, budget, all, hybrid, common.topic.is_some()).await,
                SlateAction::Show { post } => sl::show(&ctx, &post).await,
                SlateAction::Delta {
                    since,
                    limit,
                    budget,
                    kinds,
                } => sl::delta(&ctx, since, limit, budget, kinds.as_deref()).await,
                SlateAction::History { since, limit } => sl::history(&ctx, since, limit).await,
                SlateAction::Cursor { seq } => sl::cursor(&ctx, seq).await,
                SlateAction::Now { line } => sl::plain(&ctx, "now", &join(line)).await,
                SlateAction::Warn { line } => sl::plain(&ctx, "warn", &join(line)).await,
                SlateAction::Idea { line } => sl::plain(&ctx, "idea", &join(line)).await,
                SlateAction::Ask { line } => sl::ask(&ctx, &join(line)).await,
                SlateAction::Found { line } => sl::found(&ctx, &join(line)).await,
                SlateAction::Answer { post, line } => {
                    sl::answer(&ctx, sl::parse_seq(&post)?, &join(line)).await
                }
                SlateAction::Tried { line, failed, was } => {
                    sl::tried(&ctx, &join(line), failed.as_deref(), was).await
                }
                SlateAction::Take {
                    subject,
                    line,
                    anyway,
                    over,
                } => sl::take(&ctx, &subject, &join(line), anyway, over).await,
                SlateAction::Hand { subject, line, to } => {
                    sl::hand(&ctx, &subject, &join(line), to.as_deref()).await
                }
                SlateAction::Done {
                    post,
                    line,
                    abandoned,
                } => {
                    sl::done(
                        &ctx,
                        sl::parse_seq(&post)?,
                        &join(line),
                        abandoned.as_deref(),
                    )
                    .await
                }
                SlateAction::Drop { post, line, anyway } => {
                    sl::drop_post(&ctx, sl::parse_seq(&post)?, &join(line), anyway).await
                }
                SlateAction::Edit { post, line, anyway } => {
                    sl::edit(&ctx, sl::parse_seq(&post)?, &join(line), anyway).await
                }
                SlateAction::Mark {
                    post,
                    line,
                    pin,
                    unpin,
                } => {
                    let text = join(line);
                    let pin = if pin {
                        Some(true)
                    } else if unpin {
                        Some(false)
                    } else {
                        None
                    };
                    sl::mark(
                        &ctx,
                        sl::parse_seq(&post)?,
                        Some(text.as_str()).filter(|s| !s.is_empty()),
                        pin,
                    )
                    .await
                }
                SlateAction::Pin { post } => {
                    sl::mark(&ctx, sl::parse_seq(&post)?, None, Some(true)).await
                }
                SlateAction::Unpin { post } => {
                    sl::mark(&ctx, sl::parse_seq(&post)?, None, Some(false)).await
                }
                SlateAction::Promote {
                    post,
                    to,
                    plan,
                    kb,
                    note,
                } => {
                    sl::promote(
                        &ctx,
                        sl::parse_seq(&post)?,
                        &to,
                        plan.as_deref(),
                        kb.as_deref(),
                        note.as_deref(),
                    )
                    .await
                }
                SlateAction::Close { line } => {
                    let text = join(line);
                    sl::lifecycle(&ctx, "close", Some(text.as_str()).filter(|s| !s.is_empty()))
                        .await
                }
                SlateAction::Reopen => sl::lifecycle(&ctx, "reopen", None).await,
                SlateAction::Rotate => sl::lifecycle(&ctx, "rotate", None).await,
                SlateAction::Watch { once, timeout } => sl::watch(&ctx, once, timeout).await,
                SlateAction::Stats => sl::stats(&ctx).await,
                SlateAction::Ls => sl::ls(&ctx).await,
                SlateAction::Doctor => sl::doctor(&ctx).await,
            }
        }
        Cmd::Bench { action } => match action {
            BenchAction::Init {
                corpus,
                output,
                n,
                seed,
            } => commands::bench::init(&corpus, &output, n, seed),
            BenchAction::Discover {
                kb,
                query,
                limit,
                mode,
                daemon,
            } => {
                let bearer = read_bearer();
                commands::bench::discover(
                    &kb,
                    &query,
                    limit,
                    &mode,
                    daemon.as_deref(),
                    bearer.as_deref(),
                )
                .await
            }
            BenchAction::Run {
                queries,
                kbs,
                modes,
                k,
                daemon,
                output,
                json,
                baseline,
            } => {
                let bearer = read_bearer();
                commands::bench::run(
                    &queries,
                    &kbs,
                    &modes,
                    &k,
                    daemon.as_deref(),
                    bearer.as_deref(),
                    output.as_deref(),
                    json.as_deref(),
                    baseline.as_deref(),
                )
                .await
            }
        },
    }
}

/// v0.4 helper — read `~/.config/kb/token` if present. Returns None
/// when the file is absent or empty (the v0.3 personal-mode default).
/// Used by every CLI verb that POSTs/GETs the daemon: when the
/// daemon's behind an auth-on proxy, the CLI sends Authorization;
/// when it's loopback-only, the daemon bypasses anyway.
fn read_bearer() -> Option<String> {
    commands::token::token_path()
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The 12-hex `ArtifactId::from_path` shape that `kb comments list` prints.
fn looks_like_artifact_id(s: &str) -> bool {
    s.len() == 12 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Reconcile the comments verbs' `--kb`/`--artifact-id` flag forms with the
/// legacy positional forms into `(kb, artifact_id, comment_id)`.
///
/// Flags pre-fill their role; the supplied positionals then fill the
/// remaining roles (kb → artifact_id → comment_id) in order. So
/// `--kb X <id>` routes <id> to artifact_id even though clap parked it in
/// the kb positional slot, while the legacy `<kb> <id>` two-positional form
/// is unchanged. With no kb/artifact flags, a leading positional that has
/// the 12-hex artifact-id shape is taken as the artifact id (so `show <id>`
/// auto-resolves the kb); a non-hex leading positional stays the kb,
/// preserving the `<kb> …` contract. `positionals` are the verb's positional
/// fields in declaration order; clap fills them contiguously from the front.
fn reconcile_comment_target(
    kb_flag: Option<String>,
    artifact_id_flag: Option<String>,
    positionals: Vec<Option<String>>,
    want_comment_id: bool,
) -> (Option<String>, Option<String>, Option<String>) {
    let mut kb = kb_flag;
    let mut artifact_id = artifact_id_flag;
    let positionals: Vec<String> = positionals.into_iter().flatten().collect();
    // No flags + leading positional that looks like an artifact id → the
    // user gave a bare id and wants the kb auto-resolved; don't consume it
    // as the kb.
    let auto_kb = kb.is_none()
        && artifact_id.is_none()
        && positionals
            .first()
            .is_some_and(|s| looks_like_artifact_id(s));
    let mut it = positionals.into_iter();
    if !auto_kb && kb.is_none() {
        kb = it.next();
    }
    if artifact_id.is_none() {
        artifact_id = it.next();
    }
    let comment_id = if want_comment_id { it.next() } else { None };
    (kb, artifact_id, comment_id)
}

// Match kb-server: pin lance scanner warnings to ERROR so the `kb daemon`
// (in-process server) log isn't drowned by the lance 4.0.0
// `_score`/`_distance` autoprojection deprecation warns. Shared with
// `commands::daemon::run`, whose L1 layered init (stderr + ndjson file)
// must keep stderr behavior identical to every other verb.
pub(crate) const STDERR_DEFAULT_FILTER: &str = "warn,kb=info,lance::dataset::scanner=error";

fn init_tracing() {
    use tracing_subscriber::{prelude::*, EnvFilter};
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(STDERR_DEFAULT_FILTER));
    let stderr = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(filter);
    tracing_subscriber::registry().with(stderr).init();
}

#[cfg(test)]
mod tests {
    use super::{
        looks_like_artifact_id, reconcile_comment_target, Cli, Cmd, DeskAction, ProposalsAction,
    };
    use clap::Parser;

    fn s(x: &str) -> Option<String> {
        Some(x.to_string())
    }

    /// `Cli::try_parse_from` cases pinning `kb capture`'s clap wiring
    /// (U3) — the FILES positional (incl. `-`), `--name`, and the flag
    /// surface the plan's Architecture section specifies.
    fn parse_capture(args: &[&str]) -> Cmd {
        let mut full = vec!["kb", "capture"];
        full.extend_from_slice(args);
        Cli::try_parse_from(full).unwrap().cmd
    }

    #[test]
    fn capture_parses_bare_files() {
        match parse_capture(&["a.md", "b.html"]) {
            Cmd::Capture { files, .. } => {
                assert_eq!(files, vec!["a.md".to_string(), "b.html".to_string()]);
            }
            other => panic!("expected Cmd::Capture, got {other:?}"),
        }
    }

    #[test]
    fn capture_parses_dash_sentinel_with_name() {
        match parse_capture(&["-", "--name", "meeting-notes"]) {
            Cmd::Capture { files, name, .. } => {
                assert_eq!(files, vec!["-".to_string()]);
                assert_eq!(name.as_deref(), Some("meeting-notes"));
            }
            other => panic!("expected Cmd::Capture, got {other:?}"),
        }
    }

    #[test]
    fn capture_parses_no_files_with_url_and_text() {
        match parse_capture(&[
            "--url",
            "https://example.com/a",
            "--text",
            "shared snippet",
            "--kb",
            "notes",
        ]) {
            Cmd::Capture {
                files,
                kb,
                url,
                text,
                ..
            } => {
                assert!(files.is_empty());
                assert_eq!(kb.as_deref(), Some("notes"));
                assert_eq!(url.as_deref(), Some("https://example.com/a"));
                assert_eq!(text.as_deref(), Some("shared snippet"));
            }
            other => panic!("expected Cmd::Capture, got {other:?}"),
        }
    }

    #[test]
    fn capture_parses_full_flag_surface() {
        match parse_capture(&[
            "a.md",
            "--kb",
            "notes",
            "--title",
            "My Title",
            "--tags",
            "a,b",
            "--sanitize",
            "--daemon",
            "http://127.0.0.1:9999",
            "--output",
            "json",
        ]) {
            Cmd::Capture {
                files,
                kb,
                title,
                tags,
                sanitize,
                daemon,
                output,
                ..
            } => {
                assert_eq!(files, vec!["a.md".to_string()]);
                assert_eq!(kb.as_deref(), Some("notes"));
                assert_eq!(title.as_deref(), Some("My Title"));
                assert_eq!(tags.as_deref(), Some("a,b"));
                assert!(sanitize);
                assert_eq!(daemon.as_deref(), Some("http://127.0.0.1:9999"));
                assert_eq!(output.as_deref(), Some("json"));
            }
            other => panic!("expected Cmd::Capture, got {other:?}"),
        }
    }

    #[test]
    fn capture_sanitize_defaults_false() {
        match parse_capture(&["a.md"]) {
            Cmd::Capture { sanitize, .. } => assert!(!sanitize),
            other => panic!("expected Cmd::Capture, got {other:?}"),
        }
    }

    #[test]
    fn reconcile_legacy_two_positionals() {
        // `export <kb> <id>` — the legacy form is unchanged.
        let (kb, aid, cid) =
            reconcile_comment_target(None, None, vec![s("platform"), s("235395fb9978")], false);
        assert_eq!(kb.as_deref(), Some("platform"));
        assert_eq!(aid.as_deref(), Some("235395fb9978"));
        assert_eq!(cid, None);
    }

    #[test]
    fn reconcile_flag_kb_shifts_bare_id_to_artifact() {
        // `export --kb platform <id>` — clap parks <id> in the kb slot; the
        // reconciler must shift it to artifact_id (the headline complaint).
        let (kb, aid, _) =
            reconcile_comment_target(s("platform"), None, vec![s("235395fb9978"), None], false);
        assert_eq!(kb.as_deref(), Some("platform"));
        assert_eq!(aid.as_deref(), Some("235395fb9978"));
    }

    #[test]
    fn reconcile_all_flags() {
        let (kb, aid, _) =
            reconcile_comment_target(s("platform"), s("235395fb9978"), vec![None, None], false);
        assert_eq!(kb.as_deref(), Some("platform"));
        assert_eq!(aid.as_deref(), Some("235395fb9978"));
    }

    #[test]
    fn reconcile_bare_hex_id_auto_resolves_kb() {
        // `show <id>` with no flags → artifact id, kb left None (auto-resolve).
        let (kb, aid, _) =
            reconcile_comment_target(None, None, vec![s("235395fb9978"), None], false);
        assert_eq!(kb, None);
        assert_eq!(aid.as_deref(), Some("235395fb9978"));
    }

    #[test]
    fn reconcile_bare_non_hex_stays_kb() {
        // `show platform` — a non-hex lone positional is the kb (legacy).
        let (kb, aid, _) = reconcile_comment_target(None, None, vec![s("platform"), None], false);
        assert_eq!(kb.as_deref(), Some("platform"));
        assert_eq!(aid, None);
    }

    #[test]
    fn reconcile_resolve_legacy_three_positionals() {
        let (kb, aid, cid) = reconcile_comment_target(
            None,
            None,
            vec![s("platform"), s("235395fb9978"), s("c_abc")],
            true,
        );
        assert_eq!(kb.as_deref(), Some("platform"));
        assert_eq!(aid.as_deref(), Some("235395fb9978"));
        assert_eq!(cid.as_deref(), Some("c_abc"));
    }

    #[test]
    fn reconcile_resolve_flag_kb_shifts_positionals() {
        // `resolve --kb platform <id> <cid>` — positionals shift past kb.
        let (kb, aid, cid) = reconcile_comment_target(
            s("platform"),
            None,
            vec![s("235395fb9978"), s("c_abc"), None],
            true,
        );
        assert_eq!(kb.as_deref(), Some("platform"));
        assert_eq!(aid.as_deref(), Some("235395fb9978"));
        assert_eq!(cid.as_deref(), Some("c_abc"));
    }

    #[test]
    fn artifact_id_shape() {
        assert!(looks_like_artifact_id("235395fb9978"));
        assert!(!looks_like_artifact_id("platform"));
        assert!(!looks_like_artifact_id("rt1234567890")); // 'r','t' not hex
        assert!(!looks_like_artifact_id("235395fb997")); // 11 chars
    }

    // --- W2.15b — `kb propose` / `kb proposals` clap wiring -------------

    #[test]
    fn propose_parses_required_flags() {
        match Cli::try_parse_from(["kb", "propose", "--title", "T", "--body", "B"])
            .unwrap()
            .cmd
        {
            Cmd::Propose {
                title, body, kb, ..
            } => {
                assert_eq!(title, "T");
                assert_eq!(body, "B");
                assert_eq!(kb, None);
            }
            other => panic!("expected Cmd::Propose, got {other:?}"),
        }
    }

    #[test]
    fn propose_dash_body_is_the_stdin_sentinel() {
        match Cli::try_parse_from(["kb", "propose", "--title", "T", "--body", "-"])
            .unwrap()
            .cmd
        {
            Cmd::Propose { body, .. } => assert_eq!(body, "-"),
            other => panic!("expected Cmd::Propose, got {other:?}"),
        }
    }

    #[test]
    fn propose_parses_full_flag_surface() {
        match Cli::try_parse_from([
            "kb",
            "propose",
            "--title",
            "T",
            "--body",
            "B",
            "--kb",
            "canon",
            "--tags",
            "a,b",
            "--salience",
            "0.8",
            "--session-id",
            "sess-1",
            "--link",
            "other-kb",
            "--json",
        ])
        .unwrap()
        .cmd
        {
            Cmd::Propose {
                kb,
                tags,
                salience,
                session_id,
                link,
                json,
                global,
                ..
            } => {
                assert_eq!(kb.as_deref(), Some("canon"));
                assert_eq!(tags.as_deref(), Some("a,b"));
                assert_eq!(salience, Some(0.8));
                assert_eq!(session_id.as_deref(), Some("sess-1"));
                assert_eq!(link.as_deref(), Some("other-kb"));
                assert!(json);
                assert!(!global);
            }
            other => panic!("expected Cmd::Propose, got {other:?}"),
        }
    }

    #[test]
    fn propose_global_and_link_conflict() {
        assert!(Cli::try_parse_from([
            "kb", "propose", "--title", "T", "--body", "B", "--global", "--link", "x",
        ])
        .is_err());
    }

    #[test]
    fn proposals_bare_defaults_action_to_none() {
        match Cli::try_parse_from(["kb", "proposals"]).unwrap().cmd {
            Cmd::Proposals { action } => assert!(action.is_none()),
            other => panic!("expected Cmd::Proposals, got {other:?}"),
        }
    }

    #[test]
    fn proposals_list_parses_kb_and_json() {
        match Cli::try_parse_from(["kb", "proposals", "list", "--kb", "canon", "--json"])
            .unwrap()
            .cmd
        {
            Cmd::Proposals {
                action: Some(ProposalsAction::List { kb, json, .. }),
            } => {
                assert_eq!(kb.as_deref(), Some("canon"));
                assert!(json);
            }
            other => panic!("expected Cmd::Proposals(List), got {other:?}"),
        }
    }

    #[test]
    fn proposals_approve_parses_id_and_kb() {
        match Cli::try_parse_from([
            "kb",
            "proposals",
            "approve",
            "p_deadbeef0001",
            "--kb",
            "canon",
        ])
        .unwrap()
        .cmd
        {
            Cmd::Proposals {
                action: Some(ProposalsAction::Approve { id, kb, .. }),
            } => {
                assert_eq!(id, "p_deadbeef0001");
                assert_eq!(kb.as_deref(), Some("canon"));
            }
            other => panic!("expected Cmd::Proposals(Approve), got {other:?}"),
        }
    }

    #[test]
    fn proposals_reject_parses_id() {
        match Cli::try_parse_from(["kb", "proposals", "reject", "p_deadbeef0001"])
            .unwrap()
            .cmd
        {
            Cmd::Proposals {
                action: Some(ProposalsAction::Reject { id, .. }),
            } => assert_eq!(id, "p_deadbeef0001"),
            other => panic!("expected Cmd::Proposals(Reject), got {other:?}"),
        }
    }

    fn parse_desk(args: &[&str]) -> Cmd {
        let mut full = vec!["kb", "desk"];
        full.extend_from_slice(args);
        Cli::try_parse_from(full).unwrap().cmd
    }

    #[test]
    fn desk_offer_parses_as_slug_and_ttl() {
        match parse_desk(&["offer", "a.md", "--as", "ticket", "--ttl", "24h"]) {
            Cmd::Desk {
                action:
                    DeskAction::Offer {
                        file, as_slug, ttl, ..
                    },
            } => {
                assert_eq!(file, "a.md");
                assert_eq!(as_slug, "ticket");
                assert_eq!(ttl.as_deref(), Some("24h"));
            }
            other => panic!("expected Desk::Offer, got {other:?}"),
        }
    }

    #[test]
    fn desk_expire_parses_force_and_yes() {
        match parse_desk(&["expire", "abc123def456", "--force", "--yes"]) {
            Cmd::Desk {
                action: DeskAction::Expire { force, yes, .. },
            } => {
                assert!(force);
                assert!(yes);
            }
            other => panic!("expected Desk::Expire, got {other:?}"),
        }
    }

    #[test]
    fn desk_promote_parses_to_category_and_keep_draft() {
        match parse_desk(&[
            "promote",
            "handoff/ticket.md",
            "--to",
            "notes/ticket.md",
            "--category",
            "note",
            "--keep-draft-tag",
        ]) {
            Cmd::Desk {
                action:
                    DeskAction::Promote {
                        target,
                        to,
                        category,
                        keep_draft_tag,
                        ..
                    },
            } => {
                assert_eq!(target, "handoff/ticket.md");
                assert_eq!(to, "notes/ticket.md");
                assert_eq!(category.as_deref(), Some("note"));
                assert!(keep_draft_tag);
            }
            other => panic!("expected Desk::Promote, got {other:?}"),
        }
    }

    #[test]
    fn desk_ls_parses_all() {
        match parse_desk(&["ls", "--all"]) {
            Cmd::Desk {
                action: DeskAction::Ls { all, kb, .. },
            } => {
                assert!(all);
                assert!(kb.is_none());
            }
            other => panic!("expected Desk::Ls, got {other:?}"),
        }
    }
}
