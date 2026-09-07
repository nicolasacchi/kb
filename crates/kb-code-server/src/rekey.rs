//! V75-M1 — the **re-key**: which identity owns each repo-keyed table's
//! rows, and the paged background pass that stamps them.
//!
//! ## The classification
//!
//! Every table in this schema that carries a repo key answers one of two
//! questions, and [`REPO_KEYED_TABLES`] is where each one declares which:
//!
//! * **`object`** — the row is a function of the OBJECT STORE (a blob, a
//!   commit, git history). Two checkouts of one workspace would derive the
//!   identical row, so the row belongs to the WORKSPACE. Gains
//!   `workspace_id`.
//! * **`worktree`** — the row is a property of a PATH ON DISK in one
//!   checkout: the mirror index, its attention lane, working-tree
//!   annotations, review checkouts, and the four authored path-anchored
//!   projections. Sharing one across two checkouts would be a WRONG
//!   ANSWER, not a saving. Gains `worktree_id`.
//! * **`meta`** — the row is about the daemon rather than about code
//!   (`repos` itself, the audit ledger, a cross-daemon pin). Gains
//!   nothing, and says why.
//!
//! `tests::every_repo_keyed_table_is_classified` walks `sqlite_master` on
//! a freshly-migrated volume against this list from BOTH ends: a table
//! with a `repo_id`/`repo` column that is not declared fails by name, and
//! a declared table that no longer exists fails by name. That is the whole
//! teeth of the re-key — a table born after this unit is keyed correctly
//! or the build is red.
//!
//! ## What this unit does NOT do, and why
//!
//! It adds the key. It does **not** collapse two checkouts' duplicate rows
//! into one, and no read here widens from `repo_id = ?` to
//! `workspace_id = ?`. That is a deliberate, named boundary, not an
//! oversight:
//!
//! 1. A shared read over two repo ids returns each row TWICE (one per
//!    registered checkout). Sharing requires the PRIMARY KEY to become
//!    `(workspace_id, …)`, which in SQLite is a table REBUILD.
//! 2. A rebuild is O(every row) and would run inside the migration —
//!    between `Store::open` and the listener bind. Invariant 11's V72-B0(a)
//!    forbids exactly that ("no whole-corpus maintenance pass may ever sit
//!    between `Store::open` and the bind"), and V0029's header states the
//!    house rule positively: a migration is O(1) DDL.
//! 3. There is no surface today that registers two worktrees of one
//!    workspace as two repos, so a widened read would be code no
//!    configuration can reach — the v7.0 dead-surface defect.
//!
//! The collapse therefore belongs with M2's worktree lifecycle + registration,
//! and it needs its own one-way-door treatment (its own backup, its own
//! epoch, its own rehearsal). [`READS_NOT_WIDENED`] is the ledger of the
//! object-class tables that will need it, so the debt is a list rather
//! than a memory — the `UNRESOLVED_ATOMS` / `UNMINTED_KINDS` precedent.
//!
//! What the key DOES buy today, and what a read actually reads:
//! [`crate::workspace::WorkspaceOut::derived`] — a per-table census of the
//! rows each workspace owns, on `GET /api/workspaces` and in
//! `kb-code workspaces --json`.
//!
//! ## Where the value comes from
//!
//! Not from Rust. Each keyed table carries an `AFTER INSERT … WHEN NEW.<key>
//! IS NULL` trigger created by `V0040__workspace_rekey.sql`, which reads
//! the key off the `repos` row this row already points at. One home, next
//! to the column, unforgettable by a future INSERT — see that migration's
//! header for the full argument. This module only backfills the rows that
//! existed BEFORE the trigger did.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use crate::config::RepoEntry;
use crate::store::Store;

// ── the classification ───────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoKeyClass {
    Object,
    Worktree,
    Meta,
}

impl RepoKeyClass {
    pub fn as_str(self) -> &'static str {
        match self {
            RepoKeyClass::Object => "object",
            RepoKeyClass::Worktree => "worktree",
            RepoKeyClass::Meta => "meta",
        }
    }

    /// The column V0040 added for this class, or `None` for `meta`.
    pub fn key_column(self) -> Option<&'static str> {
        match self {
            RepoKeyClass::Object => Some("workspace_id"),
            RepoKeyClass::Worktree => Some("worktree_id"),
            RepoKeyClass::Meta => None,
        }
    }
}

/// One classified table.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct KeyedTable {
    pub table: &'static str,
    pub class: RepoKeyClass,
    /// `repo_id` (an FK into `repos`) or `repo` (the repo NAME — V0011's
    /// `bookmarks` and V0014's `reviews` predate the FK convention).
    /// `""` for `repos` itself.
    pub repo_column: &'static str,
    pub why: &'static str,
}

impl KeyedTable {
    fn source_expr(&self, key: &str) -> String {
        match self.repo_column {
            "repo" => format!("(SELECT {key} FROM repos WHERE name = t.repo)"),
            _ => format!("(SELECT {key} FROM repos WHERE id = t.repo_id)"),
        }
    }
}

/// **The re-key table.** Every table in this schema carrying a repo key,
/// with the identity that owns its rows.
///
/// Order is alphabetical within each class, and the class order is
/// object → worktree → meta, so the list reads as an argument rather than
/// a pile.
pub const REPO_KEYED_TABLES: &[KeyedTable] = &[
    // ── object: a function of the object store ───────────────────────
    KeyedTable {
        table: "author_stats",
        class: RepoKeyClass::Object,
        repo_column: "repo_id",
        why: "per-author commit counts folded from git history; every checkout of one object \
              store sees the same history",
    },
    KeyedTable {
        table: "behavioral_meta",
        class: RepoKeyClass::Object,
        repo_column: "repo_id",
        why: "the high-water commit of the behavioral fold — a position in the object store's \
              own history",
    },
    KeyedTable {
        table: "branch_favourites",
        class: RepoKeyClass::Object,
        repo_column: "repo",
        why: "a starred BRANCH is a ref, and refs live in the common dir every worktree of one \
              object store shares — two checkouts see the same refs/heads/x, so a star that \
              followed the checkout would be a wrong answer (contrast `bookmarks`, which \
              anchors a path on disk)",
    },
    KeyedTable {
        table: "claims",
        class: RepoKeyClass::Object,
        repo_column: "repo_id",
        why: "kbc-claim/1 prose about a subject in the object store, witnessed by a blob_sha; \
              the checkout it was written from is not part of what it is about",
    },
    KeyedTable {
        table: "cochange_pairs",
        class: RepoKeyClass::Object,
        repo_column: "repo_id",
        why: "co-change counts folded from git history",
    },
    KeyedTable {
        table: "comments",
        class: RepoKeyClass::Object,
        repo_column: "repo_id",
        why: "comments/1 rows are extracted from a BLOB and keyed by (path, blob_sha, version, \
              ordinal); the same blob yields the same rows in any checkout",
    },
    KeyedTable {
        table: "commit_sessions",
        class: RepoKeyClass::Object,
        repo_column: "repo_id",
        why: "the commit -> session join is keyed by sha; a sha is an object-store address",
    },
    KeyedTable {
        table: "doc_refs",
        class: RepoKeyClass::Object,
        repo_column: "repo_id",
        why: "DCB's reverse index of which kb doc points at which path in this repo — a fact \
              about the repository, not about a checkout of it",
    },
    KeyedTable {
        table: "entity_defs",
        class: RepoKeyClass::Object,
        repo_column: "repo_id",
        why: "entity claims are derived per blob; the per-checkout part is already INSIDE the \
              key as the `worktree` column (V0029, invariant 13), and the two compose",
    },
    KeyedTable {
        table: "lane_facts",
        class: RepoKeyClass::Object,
        repo_column: "repo_id",
        why: "aug-lane/1 facts carry their own blob_sha and are re-classed per request against \
              it (invariant 21); a fact about a blob is a fact about the object store",
    },
    KeyedTable {
        table: "lane_runs",
        class: RepoKeyClass::Object,
        repo_column: "repo_id",
        why: "the provenance envelope of a lane ingest, which its facts reference",
    },
    KeyedTable {
        table: "path_stats",
        class: RepoKeyClass::Object,
        repo_column: "repo_id",
        why: "per-path revision/churn counts folded from git history",
    },
    KeyedTable {
        table: "rails_edges",
        class: RepoKeyClass::Object,
        repo_column: "repo_id",
        why: "rails-lens/1 edges are content-addressed by (blob_hash, salt, ordinal) and \
              re-derived per blob (invariant 12)",
    },
    KeyedTable {
        table: "recipe_trust",
        class: RepoKeyClass::Object,
        repo_column: "repo_id",
        why: "an operator's trust-on-first-use acceptance of a repo-versioned recipe, keyed on \
              the recipe file's git BLOB OID — a content address, so the approval survives a \
              branch switch (V74-L3a, invariant 25(b))",
    },
    KeyedTable {
        table: "scip_runs",
        class: RepoKeyClass::Object,
        repo_column: "repo_id",
        why: "an ingest stamped with the head sha it covered — an object-store position",
    },
    KeyedTable {
        table: "session_signals",
        class: RepoKeyClass::Object,
        repo_column: "repo_id",
        why: "per-session aggregates joined to the repository, not to a checkout of it",
    },
    // ── worktree: a property of a path on disk ───────────────────────
    KeyedTable {
        table: "annotations",
        class: RepoKeyClass::Worktree,
        repo_column: "repo_id",
        why: "a working-tree annotation anchors to a line in the file as it is CHECKED OUT; \
              two checkouts on two branches are two different files at one path",
    },
    KeyedTable {
        table: "bookmarks",
        class: RepoKeyClass::Worktree,
        repo_column: "repo",
        why: "a bookmark is (path, line) in a checkout — the same anchoring an annotation has",
    },
    KeyedTable {
        table: "canvas_boards",
        class: RepoKeyClass::Worktree,
        repo_column: "repo_id",
        why: "kbc-canvas/1 nodes address paths and lines and are re-resolved against the \
              checkout on every read (invariant 24). NOTE it takes `worktree_id`, not \
              `workspace_id`: invariant 14 records that the canvas tables deliberately carry \
              no column of that name, which is D26's kbc-seq word, not D13's",
    },
    KeyedTable {
        table: "canvas_sets",
        class: RepoKeyClass::Worktree,
        repo_column: "repo_id",
        why: "the pre-kbc-canvas board payload, path-anchored the same way",
    },
    KeyedTable {
        table: "file_opens",
        class: RepoKeyClass::Worktree,
        repo_column: "repo_id",
        why: "the recency signal behind the files lane's frecency blend, over the mirror's own \
              path set",
    },
    KeyedTable {
        table: "files",
        class: RepoKeyClass::Worktree,
        repo_column: "repo_id",
        why: "THE mirror index: one row per (checkout, path) pointing at the blob that is there \
              RIGHT NOW. The definitional worktree-class table",
    },
    KeyedTable {
        table: "reading_sets",
        class: RepoKeyClass::Worktree,
        repo_column: "repo_id",
        why: "an ordered list of path+line spans in a checkout — and it already carries a \
              column literally named `workspace_id` meaning something else entirely (D26's \
              Desk, V0029), which is the second reason it takes `worktree_id`",
    },
    KeyedTable {
        table: "recipe_runs",
        class: RepoKeyClass::Worktree,
        repo_column: "repo_id",
        why: "a materialised recipe run is the answer the runner gave over ONE checkout's \
              state; the mirror index it reads is itself worktree-class, and a run against \
              one branch's tree is not a run against another's",
    },
    KeyedTable {
        table: "reviews",
        class: RepoKeyClass::Worktree,
        repo_column: "repo",
        why: "a review is bound to a checkout: its patchsets are minted from refs in one \
              worktree and `checkout.rs`'s working-tree lane acts on that same path",
    },
    KeyedTable {
        table: "trails",
        class: RepoKeyClass::Worktree,
        repo_column: "repo_id",
        why: "a kbc-trail/1 trail is an ordered list of path+line steps the operator actually \
              walked, each pinned to the blob they were looking at — a record of one CHECKOUT \
              being read",
    },
    // ── meta: about the daemon, not about code ───────────────────────
    KeyedTable {
        table: "repos",
        class: RepoKeyClass::Meta,
        repo_column: "",
        why: "the CANONICAL holder of both identities (V0040 gave it `workspace_id` + \
              `worktree_id`); it does not gain a foreign key, it holds the two",
    },
    KeyedTable {
        table: "worktrees",
        class: RepoKeyClass::Meta,
        repo_column: "repo_id",
        why: "the identity registry itself: its `repo_id` says which `[[repos]]` entry a \
              checkout is MOUNTED through, which is the opposite direction from a row \
              declaring its owner",
    },
    KeyedTable {
        table: "recipes_server",
        class: RepoKeyClass::Meta,
        repo_column: "repo",
        why: "a server-authored recipe whose `repo` is an optional SCOPE, not an owner — NULL \
              means every repo (V74-L3a), and a row that may legitimately belong to no repo \
              cannot be owned by a workspace",
    },
    KeyedTable {
        table: "mutations",
        class: RepoKeyClass::Meta,
        repo_column: "repo",
        why: "the V0027 audit ledger records what HAPPENED under whatever identity was live at \
              the time; re-keying it would rewrite history, which is the one thing an audit \
              ledger may not do",
    },
    KeyedTable {
        table: "doc_lens_pins",
        class: RepoKeyClass::Meta,
        repo_column: "repo",
        why: "a cross-daemon pin keyed (kb, doc_id); its `repo`/`repo_root` are a resolution \
              HINT re-resolved on every read, deliberately not an FK (see doclens/pins.rs)",
    },
];

/// The object-class tables whose READS this unit does not widen from
/// `repo_id` to `workspace_id`, and what each one needs before it can be.
///
/// Named rather than silently omitted (the `UNRESOLVED_ATOMS` /
/// `UNMINTED_KINDS` precedent). Every entry needs the same two things: a
/// PRIMARY KEY collapse onto `(workspace_id, …)` — a SQLite table rebuild,
/// which no migration in this crate may perform on the bind path — and a
/// surface that registers two worktrees of one workspace, which is M2's.
pub const READS_NOT_WIDENED: &[&str] = &[
    "author_stats",
    "behavioral_meta",
    // V75-M3 — a starred branch is object-class (a ref is a property of
    // the shared object store), and its read is not widened for the same
    // two reasons as every other entry here: `branch_favourites`' PK is
    // `(repo, ref_name)`, so sharing needs the same table rebuild, and
    // nothing registers two worktrees of one workspace yet.
    "branch_favourites",
    "claims",
    "cochange_pairs",
    "comments",
    "commit_sessions",
    "doc_refs",
    "entity_defs",
    "lane_facts",
    "lane_runs",
    "path_stats",
    "rails_edges",
    "recipe_trust",
    "scip_runs",
    "session_signals",
];

/// Every table that gains a key column, in backfill order.
pub fn keyed_tables() -> impl Iterator<Item = &'static KeyedTable> {
    REPO_KEYED_TABLES
        .iter()
        .filter(|t| t.class.key_column().is_some())
}

/// The SQL the backfill runs for one table — built here so the test can
/// read it without a database, and so every name in it comes from
/// [`REPO_KEYED_TABLES`] rather than from a caller.
pub fn backfill_sql(t: &KeyedTable) -> Option<String> {
    let key = t.class.key_column()?;
    Some(format!(
        "UPDATE {table} AS t SET {key} = {src} \
         WHERE t.rowid > ?1 AND t.rowid <= ?2 AND t.{key} IS NULL",
        table = t.table,
        key = key,
        src = t.source_expr(key),
    ))
}

// ── the background pass ──────────────────────────────────────────────

/// Rows per page. Bounded so the store's ONE connection mutex is released
/// between pages (invariant 11's V72-B0(b): a long transaction on a
/// background thread does not fix an outage, it moves it).
pub const REKEY_PAGE: usize = 512;

/// Wall-clock budget per boot. Exhausting it is not a failure — the cursor
/// is in the database, so the next boot resumes.
pub const REKEY_BUDGET: std::time::Duration = std::time::Duration::from_secs(120);

/// Slept between pages, for V72-B0's reason: a tight re-lock loop starves
/// readers even when each transaction is short.
pub const REKEY_PAUSE: std::time::Duration = std::time::Duration::from_millis(25);

pub const STATE_PENDING: u8 = 0;
pub const STATE_RUNNING: u8 = 1;
pub const STATE_DONE: u8 = 2;

/// The honesty flag on `GET /api/identity` and `GET /api/workspaces`.
///
/// `pending` is load-bearing: it says "the key columns may be NULL and
/// `GET /api/workspaces` may be empty because resolution has not run yet",
/// which is a different statement from "there are no workspaces".
pub fn state_label(flag: &AtomicU8) -> &'static str {
    match flag.load(Ordering::Relaxed) {
        STATE_RUNNING => "running",
        STATE_DONE => "done",
        _ => "pending",
    }
}

/// FNV-1a 64 over the resolved repo identities. Persisted per
/// `rekey_progress` row, so a repo that was ADDED, REMOVED or RE-POINTED
/// invalidates the recorded progress and the pass walks again — which
/// costs a rowid scan (the `WHERE <key> IS NULL` predicate skips every row
/// already keyed) rather than a re-write.
///
/// Hand-rolled for the same reason `lib::salt_set_fingerprint` is: the
/// value is persisted, and `DefaultHasher` is not stable across Rust
/// releases.
pub fn identity_fingerprint(resolved: &[crate::workspace::Resolved]) -> String {
    let mut parts: Vec<String> = resolved
        .iter()
        .map(|r| format!("{}\u{1}{}\u{1}{}", r.repo, r.workspace_id, r.worktree_id))
        .collect();
    parts.sort();
    // A plan version, so a future change to WHICH tables are keyed
    // invalidates every recorded cursor without needing a migration.
    parts.push(format!("plan=1;tables={}", keyed_tables().count()));
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in &parts {
        for b in p.as_bytes().iter().chain(std::iter::once(&b'\n')) {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// What one boot's pass did — logged, and returned so the rehearsal can
/// report it.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct BackfillReport {
    pub pages: u64,
    pub rows_keyed: u64,
    pub tables_done: usize,
    pub tables_total: usize,
    /// `false` when the wall-clock budget stopped the pass; the cursor is
    /// persisted and the next boot resumes.
    pub complete: bool,
}

/// Run the backfill to completion (or until `budget` runs out) against an
/// already-resolved store. Synchronous — callers hand it a blocking
/// context.
pub fn run_backfill(
    store: &Store,
    fingerprint: &str,
    budget: Option<std::time::Duration>,
    pause: std::time::Duration,
) -> BackfillReport {
    let started = std::time::Instant::now();
    let mut report = BackfillReport {
        tables_total: keyed_tables().count(),
        complete: true,
        ..Default::default()
    };
    for t in keyed_tables() {
        loop {
            match store.rekey_backfill_page(t, fingerprint, REKEY_PAGE) {
                Ok((keyed, done)) => {
                    report.pages += 1;
                    report.rows_keyed += keyed;
                    if done {
                        report.tables_done += 1;
                        break;
                    }
                }
                Err(e) => {
                    // Never fatal: the cursor already on disk means the
                    // next boot retries this table from where it stopped.
                    tracing::warn!(table = t.table, error = %e,
                        "kb-code: re-key backfill page failed");
                    report.complete = false;
                    return report;
                }
            }
            if let Some(b) = budget {
                if started.elapsed() >= b {
                    report.complete = false;
                    return report;
                }
            }
            if !pause.is_zero() {
                std::thread::sleep(pause);
            }
        }
    }
    report
}

/// V75-M1's background task: resolve every configured repo to its
/// workspace + worktree, then backfill the key columns, paged.
///
/// Spawned by `bind_and_spawn` and NEVER awaited, for invariant 11's
/// V72-B0(a) reason — `git rev-list --max-parents=0 HEAD` walks history,
/// and a boot that paid for that on every restart would be the salt-sweep
/// hang again in a new costume.
pub fn spawn_rekey(store: Arc<Store>, repos: Vec<RepoEntry>, flag: Arc<AtomicU8>) {
    tokio::task::spawn_blocking(move || {
        flag.store(STATE_RUNNING, Ordering::Relaxed);
        let resolved = crate::workspace::resolve_and_upsert(&store, &repos);
        for r in &resolved {
            tracing::info!(
                repo = %r.repo,
                workspace = %r.workspace_id,
                worktree = %r.worktree_id,
                worktrees = r.worktrees,
                root_commit = ?r.root_commit,
                "kb-code: resolved a repo to its workspace"
            );
        }
        let fingerprint = identity_fingerprint(&resolved);
        let report = run_backfill(&store, &fingerprint, Some(REKEY_BUDGET), REKEY_PAUSE);
        if report.complete {
            flag.store(STATE_DONE, Ordering::Relaxed);
            if report.rows_keyed > 0 {
                tracing::info!(
                    pages = report.pages,
                    rows = report.rows_keyed,
                    tables = report.tables_total,
                    "kb-code: Workspace re-key backfill complete"
                );
            }
        } else {
            flag.store(STATE_PENDING, Ordering::Relaxed);
            tracing::info!(
                pages = report.pages,
                rows = report.rows_keyed,
                done = report.tables_done,
                total = report.tables_total,
                "kb-code: Workspace re-key backfill hit its per-boot budget — resumes from the \
                 saved cursor on the next boot"
            );
        }
    });
}

pub mod rehearsal;

#[cfg(test)]
mod tests;
