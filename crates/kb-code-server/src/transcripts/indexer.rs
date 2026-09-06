//! Tail indexer (W2.5) — walk-on-startup + a live `notify` watcher over
//! `[transcripts] root` (default `~/.claude/projects`, tilde-expanded —
//! `config::TranscriptsSection::resolved_root`). Two pieces:
//!
//! - [`tail_file`] — the testable primitive: given one transcript JSONL
//!   file's current `(inode, size)`, reparse only the bytes past the
//!   stored `byte_offset` (an ordinary append — the overwhelmingly common
//!   case, since Claude Code only ever appends to a live transcript), or
//!   the WHOLE file from byte 0 when the `inode` has changed (a
//!   rotation/rewrite — rare, but not impossible: a compaction pass, a
//!   manual edit) — see [`crate::store::Store::delete_transcript_turns_for_file`]'s
//!   call site below for how the stale rows get cleared first. A trailing
//!   line with no `\n` yet (the file is mid-write) is never consumed —
//!   the next tail call picks it up once it's complete, so a turn is
//!   never indexed from a JSON fragment that would fail to parse anyway.
//! - [`TranscriptWatcher`] — the live half: ONE recursive `notify-debouncer-full`
//!   instance over the whole root (deliberately simpler than
//!   `crate::mirror::watchset`'s per-repo working-tree/git-dir split — there
//!   is no `.git`-shaped internal structure to a transcripts directory, so a
//!   single recursive watch is the whole watch-set), draining on its own
//!   dedicated `std::thread` (same rationale as `mirror::MirrorWatcher`:
//!   sqlite calls are synchronous, and this must never block the tokio
//!   runtime). RAII-shaped like `mirror::MirrorWatcher` — dropping it stops
//!   the thread.
//!
//! # What gets walked
//!
//! [`walk_transcripts_root`] enumerates every `.jsonl` file under `root`,
//! two levels of inclusion:
//! - `<root>/<project-dir>/<session-uuid>.jsonl` — the top-level session
//!   transcript.
//! - `<root>/<project-dir>/<session-uuid>/subagents/agent-*.jsonl` — a
//!   subagent's own transcript ("sidecar" — 74% of the on-disk corpus in
//!   this fleet's own `~/.claude/projects`, per the grounding pass this
//!   module was built against).
//!
//! EXCLUDED: any path with a `workflows` path component at ANY depth —
//! this drops both `<session>/workflows/**` (workflow definitions/scripts,
//! not transcripts at all) and `<session>/subagents/workflows/**`
//! (per-workflow-run `journal.jsonl` files — real JSONL, but a different
//! shape/purpose than a turn transcript, and the design brief explicitly
//! calls these out as excluded) — one skip rule covers both, rather than
//! two separate path-shape checks. `[transcripts] exclude_projects` is a
//! plain deny-list matched against the immediate child directory name of
//! `root` (a project dir), applied at BOTH the startup walk and the live
//! watcher's per-event filter.

use crate::store::{Store, StoreError};
use crate::transcripts::parse::{self, ParsedTurn};
use notify::RecursiveMode;
use notify_debouncer_full::{new_debouncer, DebounceEventResult, Debouncer, RecommendedCache};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Debounce window for the transcripts watcher — same default as
/// `mirror::DEFAULT_DEBOUNCE_MS`; a SEPARATE debouncer instance from the
/// mirror module's (a different daemon-lifetime concern watching a
/// completely different directory tree), not a shared one.
pub const DEFAULT_DEBOUNCE_MS: u64 = 250;

#[derive(Debug, thiserror::Error)]
pub enum TranscriptIndexError {
    #[error("path {0} is not under the transcripts root {1}")]
    NotUnderRoot(PathBuf, PathBuf),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
}

pub type Result<T> = std::result::Result<T, TranscriptIndexError>;

/// A [`ParsedTurn`] paired with the byte range (within its SOURCE JSONL
/// line — see the `V0003` migration doc on why every turn from one line
/// shares the same range) `store::Store::insert_transcript_turns` needs
/// alongside it.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexedTurn {
    pub turn: ParsedTurn,
    pub byte_offset: i64,
    pub byte_len: i64,
}

/// One [`tail_file`] call's outcome — what `TranscriptWatcher`'s drain loop
/// logs, and what tests pin the offset/reparse math against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TailStats {
    pub turns_indexed: usize,
    /// Bytes newly consumed by this call (0 on a no-op re-tail of an
    /// unchanged file, or when the only new content is a still-incomplete
    /// trailing line).
    pub bytes_advanced: u64,
    /// `true` if this call detected an `inode` change and reparsed the
    /// WHOLE file from byte 0 (after clearing this file's prior
    /// `transcript_turns`/`transcript_fts` rows).
    pub full_reparse: bool,
}

#[cfg(unix)]
fn file_inode(meta: &std::fs::Metadata) -> i64 {
    use std::os::unix::fs::MetadataExt;
    meta.ino() as i64
}

/// Non-unix fallback (native Windows is out of scope per the project
/// CLAUDE.md — WSL2, which reports real unix inodes, is the supported
/// path). A constant inode means an inode-change reparse never triggers
/// here; a truncation is still caught by the `byte_offset > file_len`
/// check in [`tail_file`].
#[cfg(not(unix))]
fn file_inode(_meta: &std::fs::Metadata) -> i64 {
    0
}

/// `abs_path` relative to `root`, forward-slash-joined regardless of host
/// OS (mirrors `sink::relativize`'s convention) — the `transcript_files.src_file`
/// key.
fn rel_src_file(root: &Path, abs_path: &Path) -> Option<String> {
    let rel = abs_path.strip_prefix(root).ok()?;
    let s = rel.to_string_lossy();
    Some(if cfg!(windows) {
        s.replace('\\', "/")
    } else {
        s.into_owned()
    })
}

/// Tail one transcript file: reparse `[stored byte_offset, EOF)` (or the
/// whole file, on an inode change) and index every turn found. `project_dir`
/// is supplied by the caller (the walk/watcher already knows it — the
/// immediate child directory of `root` this path lives under) rather than
/// re-derived here.
pub fn tail_file(
    store: &Store,
    root: &Path,
    abs_path: &Path,
    project_dir: &str,
    index_thinking: bool,
) -> Result<TailStats> {
    let Some(src_file) = rel_src_file(root, abs_path) else {
        return Err(TranscriptIndexError::NotUnderRoot(
            abs_path.to_path_buf(),
            root.to_path_buf(),
        ));
    };

    let meta = std::fs::metadata(abs_path)?;
    let inode = file_inode(&meta);
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let file_len = meta.len();

    let existing = store.get_transcript_file(&src_file)?;
    let mut force_full = existing.as_ref().is_some_and(|e| e.inode != inode);
    let mut start_offset = existing.as_ref().map(|e| e.byte_offset as u64).unwrap_or(0);
    if start_offset > file_len {
        // The file shrank without an inode change (e.g. an in-place
        // truncate+rewrite on a filesystem that reuses inodes) — defensive:
        // treat exactly like an inode change rather than seeking past EOF.
        force_full = true;
    }
    if force_full {
        start_offset = 0;
    }

    let file_id = match &existing {
        Some(e) if !force_full => e.id,
        Some(e) => {
            store.delete_transcript_turns_for_file(e.id)?;
            e.id
        }
        None => store.upsert_transcript_file_state(project_dir, &src_file, inode, 0, mtime)?,
    };

    let mut file = std::fs::File::open(abs_path)?;
    file.seek(SeekFrom::Start(start_offset))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;

    let mut turns_out: Vec<IndexedTurn> = Vec::new();
    let mut consumed: u64 = 0;
    for raw_line in buf.split_inclusive(|&b| b == b'\n') {
        if raw_line.last() != Some(&b'\n') {
            // Trailing partial line — the file is still being written to.
            // Stop here WITHOUT advancing past it; the next tail call
            // re-reads it once the writer has flushed the newline.
            break;
        }
        let line_len = raw_line.len() as u64;
        let content = &raw_line[..raw_line.len() - 1]; // strip the \n
        let line_str = String::from_utf8_lossy(content);
        let line_byte_offset = (start_offset + consumed) as i64;
        let line_byte_len = content.len() as i64;
        for turn in parse::parse_line(&line_str, index_thinking) {
            turns_out.push(IndexedTurn {
                turn,
                byte_offset: line_byte_offset,
                byte_len: line_byte_len,
            });
        }
        consumed += line_len;
    }

    let turns_indexed = turns_out.len();
    if !turns_out.is_empty() {
        store.insert_transcript_turns(file_id, &turns_out)?;
    }
    let new_offset = start_offset + consumed;
    store.upsert_transcript_file_state(project_dir, &src_file, inode, new_offset as i64, mtime)?;

    Ok(TailStats {
        turns_indexed,
        bytes_advanced: consumed,
        full_reparse: force_full,
    })
}

/// Recursively enumerate every INCLUDED `.jsonl` file under `root` — see
/// the module doc's "What gets walked" section. Returns `(project_dir,
/// absolute path)` pairs. Tolerant of a missing/unreadable `root` or
/// subdirectory (returns whatever was reachable, never panics/errors) —
/// mirrors `mirror::watchset::working_tree_watch_set`'s same posture for a
/// directory that may not exist yet.
pub fn walk_transcripts_root(root: &Path, exclude_projects: &[String]) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(top_entries) = std::fs::read_dir(root) else {
        return out;
    };
    for top in top_entries.flatten() {
        let Ok(file_type) = top.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let project_dir = top.file_name().to_string_lossy().into_owned();
        if exclude_projects.contains(&project_dir) {
            continue;
        }
        walk_project_dir(&top.path(), &project_dir, &mut out);
    }
    out
}

fn walk_project_dir(dir: &Path, project_dir: &str, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            if entry.file_name() == "workflows" {
                // Drops BOTH `<session>/workflows/**` and
                // `<session>/subagents/workflows/**` in one rule — see the
                // module doc.
                continue;
            }
            walk_project_dir(&path, project_dir, out);
        } else if file_type.is_file() && path.extension().and_then(|e| e.to_str()) == Some("jsonl")
        {
            out.push((project_dir.to_string(), path));
        }
    }
}

/// Is `path` (already known to end in `.jsonl`) something the transcripts
/// lane should index, given `root`/`exclude_projects`? Shared by the
/// startup walk's implicit filtering ([`walk_transcripts_root`] never
/// descends into a `workflows` dir or an excluded project to begin with)
/// and the live watcher's per-EVENT filter (which sees a raw path that
/// hasn't been pre-filtered by a directory walk). Returns the resolved
/// `project_dir` on acceptance.
fn accept_event_path(root: &Path, path: &Path, exclude_projects: &[String]) -> Option<String> {
    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
        return None;
    }
    let rel = path.strip_prefix(root).ok()?;
    if rel
        .components()
        .any(|c| c.as_os_str() == std::ffi::OsStr::new("workflows"))
    {
        return None;
    }
    let project_dir = rel
        .components()
        .next()?
        .as_os_str()
        .to_string_lossy()
        .into_owned();
    if exclude_projects.contains(&project_dir) {
        return None;
    }
    Some(project_dir)
}

/// Active watcher — drop the value to stop watching (mirrors
/// `mirror::MirrorWatcher`'s RAII shape).
pub struct TranscriptWatcher {
    // Leading underscore (mirrors `mirror::MirrorWatcher`'s own two fields)
    // — held purely for RAII; neither is ever read back after construction.
    _debouncer: Debouncer<notify::RecommendedWatcher, RecommendedCache>,
    _drain_thread: std::thread::JoinHandle<()>,
}

impl TranscriptWatcher {
    /// Start the transcripts lane: an immediate startup walk (every file
    /// under `root`, tailed from its stored — or zero — offset), then a
    /// live recursive watch that tails whatever file each debounced event
    /// names. Runs on its own dedicated `std::thread` (never the tokio
    /// runtime — see the module doc). Non-fatal to fail: unlike
    /// `mirror::MirrorWatcher` (a code-browsing daemon with no watcher is a
    /// broken core feature), the raw-transcripts lane is a pull-only
    /// convenience surface — `bind_and_spawn` logs a warning and continues
    /// booting rather than refusing to start the whole daemon.
    pub fn start(
        store: Arc<Store>,
        root: PathBuf,
        exclude_projects: Vec<String>,
        index_thinking: bool,
    ) -> anyhow::Result<Self> {
        use anyhow::Context;

        let (tx, rx) = std::sync::mpsc::channel::<DebounceEventResult>();
        let mut debouncer = new_debouncer(Duration::from_millis(DEFAULT_DEBOUNCE_MS), None, tx)
            .context("transcripts watcher: notify debouncer init")?;
        if root.exists() {
            if let Err(e) = debouncer.watch(&root, RecursiveMode::Recursive) {
                tracing::warn!(
                    root = %root.display(), error = %e,
                    "transcripts watcher: failed to arm the recursive watch — live tailing will \
                     not observe new/changed transcripts until a restart",
                );
            }
        } else {
            tracing::warn!(
                root = %root.display(),
                "transcripts watcher: root does not exist — nothing to walk yet, and the watch \
                 was not armed (a restart is needed once it appears)",
            );
        }

        let drain_thread = std::thread::Builder::new()
            .name("kb-code-transcripts".to_string())
            .spawn(move || drain_loop(rx, store, root, exclude_projects, index_thinking))
            .context("transcripts watcher thread spawn")?;

        Ok(Self {
            _debouncer: debouncer,
            _drain_thread: drain_thread,
        })
    }
}

fn drain_loop(
    rx: std::sync::mpsc::Receiver<DebounceEventResult>,
    store: Arc<Store>,
    root: PathBuf,
    exclude_projects: Vec<String>,
    index_thinking: bool,
) {
    for (project_dir, path) in walk_transcripts_root(&root, &exclude_projects) {
        if let Err(e) = tail_file(&store, &root, &path, &project_dir, index_thinking) {
            tracing::warn!(
                path = %path.display(), error = %e,
                "transcripts: startup tail failed — skipping this file",
            );
        }
    }

    while let Ok(result) = rx.recv() {
        match result {
            Ok(events) => {
                for event in events {
                    for path in &event.event.paths {
                        if !path.exists() {
                            // A remove/rename-away — tail state is left as
                            // is; a future recreate at this path (a new
                            // inode) forces a full reparse on its own.
                            continue;
                        }
                        let Some(project_dir) = accept_event_path(&root, path, &exclude_projects)
                        else {
                            continue;
                        };
                        if let Err(e) = tail_file(&store, &root, path, &project_dir, index_thinking)
                        {
                            tracing::warn!(
                                path = %path.display(), error = %e,
                                "transcripts: live tail failed — skipping this event",
                            );
                        }
                    }
                }
            }
            Err(errors) => {
                for e in errors {
                    tracing::warn!(error = %e, "transcripts watcher error");
                }
            }
        }
    }
    tracing::info!(
        "transcripts watcher drain thread exiting (debouncer channel disconnected); no more \
         live transcript updates will be observed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcripts::parse::{KIND_ASSISTANT, KIND_USER};

    fn open_store() -> (tempfile::TempDir, Store) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("index.db")).unwrap();
        (tmp, store)
    }

    fn user_line(uuid: &str, parent: Option<&str>, text: &str) -> String {
        let parent_json = match parent {
            Some(p) => format!("\"{p}\""),
            None => "null".to_string(),
        };
        format!(
            r#"{{"type":"user","uuid":"{uuid}","parentUuid":{parent_json},"sessionId":"s1","timestamp":"2026-07-17T10:00:00.000Z","isSidechain":false,"message":{{"role":"user","content":"{text}"}}}}"#
        )
    }

    #[test]
    fn tail_file_indexes_a_fresh_file_from_scratch() {
        let (_tmp, store) = open_store();
        let root = tempfile::tempdir().unwrap();
        let proj = root.path().join("proj-a");
        std::fs::create_dir_all(&proj).unwrap();
        let file = proj.join("session1.jsonl");
        let content = format!(
            "{}\n{}\n",
            user_line("u1", None, "hello"),
            user_line("u2", Some("u1"), "world"),
        );
        std::fs::write(&file, &content).unwrap();

        let stats = tail_file(&store, root.path(), &file, "proj-a", true).unwrap();
        assert_eq!(stats.turns_indexed, 2);
        assert_eq!(stats.bytes_advanced, content.len() as u64);
        assert!(!stats.full_reparse, "a brand new file is not a reparse");

        let state = store
            .get_transcript_file("proj-a/session1.jsonl")
            .unwrap()
            .unwrap();
        assert_eq!(state.byte_offset, content.len() as i64);
    }

    #[test]
    fn tail_file_only_parses_new_bytes_on_append() {
        let (_tmp, store) = open_store();
        let root = tempfile::tempdir().unwrap();
        let proj = root.path().join("proj-a");
        std::fs::create_dir_all(&proj).unwrap();
        let file = proj.join("session1.jsonl");
        let first = format!("{}\n", user_line("u1", None, "hello"));
        std::fs::write(&file, &first).unwrap();

        let stats1 = tail_file(&store, root.path(), &file, "proj-a", true).unwrap();
        assert_eq!(stats1.turns_indexed, 1);
        assert_eq!(stats1.bytes_advanced, first.len() as u64);

        let second = format!("{}\n", user_line("u2", Some("u1"), "world"));
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap();
        use std::io::Write;
        f.write_all(second.as_bytes()).unwrap();
        drop(f);

        let stats2 = tail_file(&store, root.path(), &file, "proj-a", true).unwrap();
        assert_eq!(
            stats2.turns_indexed, 1,
            "only the newly appended line's turn must be parsed, not the whole file again"
        );
        assert_eq!(
            stats2.bytes_advanced,
            second.len() as u64,
            "offset math must advance by exactly the new bytes"
        );
        assert!(!stats2.full_reparse);

        let state = store
            .get_transcript_file("proj-a/session1.jsonl")
            .unwrap()
            .unwrap();
        assert_eq!(state.byte_offset, (first.len() + second.len()) as i64);

        let hits = store
            .search_transcripts("hello OR world", 10, None, None)
            .unwrap();
        assert_eq!(hits.len(), 2, "both turns must be searchable: {hits:?}");
    }

    #[test]
    fn tail_file_does_not_consume_an_incomplete_trailing_line() {
        let (_tmp, store) = open_store();
        let root = tempfile::tempdir().unwrap();
        let proj = root.path().join("proj-a");
        std::fs::create_dir_all(&proj).unwrap();
        let file = proj.join("session1.jsonl");
        // No trailing newline — simulates a writer mid-flush.
        let partial = user_line("u1", None, "still writing");
        std::fs::write(&file, &partial).unwrap();

        let stats = tail_file(&store, root.path(), &file, "proj-a", true).unwrap();
        assert_eq!(
            stats.turns_indexed, 0,
            "an incomplete line must not be indexed"
        );
        assert_eq!(stats.bytes_advanced, 0);

        let state = store
            .get_transcript_file("proj-a/session1.jsonl")
            .unwrap()
            .unwrap();
        assert_eq!(state.byte_offset, 0);

        // The writer finishes the line.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap();
        use std::io::Write;
        f.write_all(b"\n").unwrap();
        drop(f);

        let stats2 = tail_file(&store, root.path(), &file, "proj-a", true).unwrap();
        assert_eq!(
            stats2.turns_indexed, 1,
            "now-complete line must be picked up"
        );
    }

    #[test]
    fn inode_swap_forces_a_full_reparse_and_drops_stale_turns() {
        let (_tmp, store) = open_store();
        let root = tempfile::tempdir().unwrap();
        let proj = root.path().join("proj-a");
        std::fs::create_dir_all(&proj).unwrap();
        let file = proj.join("session1.jsonl");
        std::fs::write(&file, format!("{}\n", user_line("u1", None, "original"))).unwrap();
        tail_file(&store, root.path(), &file, "proj-a", true).unwrap();
        assert_eq!(
            store
                .search_transcripts("original", 10, None, None)
                .unwrap()
                .len(),
            1
        );

        // Force a genuine inode change at the SAME path: write the new
        // content to a SIBLING file (a distinct, already-existing inode)
        // and `rename` it onto `file` — a POSIX rename re-points the
        // directory entry at the new inode unconditionally, unlike
        // remove+recreate, which some filesystems/allocators may satisfy
        // by reusing the just-freed inode number (observed on this
        // environment's `/tmp`-backed tempdir).
        let replacement = proj.join("session1.jsonl.new");
        std::fs::write(
            &replacement,
            format!("{}\n", user_line("u9", None, "rewritten")),
        )
        .unwrap();
        std::fs::rename(&replacement, &file).unwrap();

        let stats = tail_file(&store, root.path(), &file, "proj-a", true).unwrap();
        assert!(
            stats.full_reparse,
            "an inode change must force a full reparse"
        );
        assert_eq!(stats.turns_indexed, 1);

        assert!(
            store
                .search_transcripts("original", 10, None, None)
                .unwrap()
                .is_empty(),
            "the stale turn's FTS entry must be gone after a full reparse"
        );
        assert_eq!(
            store
                .search_transcripts("rewritten", 10, None, None)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn walk_transcripts_root_includes_sidecars_excludes_workflows_and_denylist() {
        let root = tempfile::tempdir().unwrap();
        let r = root.path();

        // Included: top-level session transcript.
        std::fs::create_dir_all(r.join("proj-a")).unwrap();
        std::fs::write(r.join("proj-a/session1.jsonl"), "{}\n").unwrap();

        // Included: subagent sidecar.
        std::fs::create_dir_all(r.join("proj-a/session1/subagents")).unwrap();
        std::fs::write(r.join("proj-a/session1/subagents/agent-x.jsonl"), "{}\n").unwrap();

        // Excluded: a top-level session workflows dir (not a transcript).
        std::fs::create_dir_all(r.join("proj-a/session1/workflows")).unwrap();
        std::fs::write(r.join("proj-a/session1/workflows/wf.json"), "{}").unwrap();

        // Excluded: a subagent workflow journal (real .jsonl, wrong shape).
        std::fs::create_dir_all(r.join("proj-a/session1/subagents/workflows/wf_1")).unwrap();
        std::fs::write(
            r.join("proj-a/session1/subagents/workflows/wf_1/journal.jsonl"),
            "{}\n",
        )
        .unwrap();

        // Excluded: an entire deny-listed project.
        std::fs::create_dir_all(r.join("proj-b")).unwrap();
        std::fs::write(r.join("proj-b/session2.jsonl"), "{}\n").unwrap();

        let found = walk_transcripts_root(r, &["proj-b".to_string()]);
        let mut rels: Vec<String> = found
            .iter()
            .map(|(_, p)| p.strip_prefix(r).unwrap().to_string_lossy().into_owned())
            .collect();
        rels.sort();
        assert_eq!(
            rels,
            vec![
                "proj-a/session1.jsonl".to_string(),
                "proj-a/session1/subagents/agent-x.jsonl".to_string(),
            ],
            "got {rels:?}"
        );
        assert!(found.iter().all(|(pd, _)| pd == "proj-a"));
    }

    #[test]
    fn accept_event_path_mirrors_the_walk_filters() {
        let root = PathBuf::from("/root");
        let deny = vec!["proj-b".to_string()];
        assert_eq!(
            accept_event_path(&root, &root.join("proj-a/session1.jsonl"), &deny),
            Some("proj-a".to_string())
        );
        assert_eq!(
            accept_event_path(
                &root,
                &root.join("proj-a/session1/subagents/agent-x.jsonl"),
                &deny
            ),
            Some("proj-a".to_string())
        );
        assert_eq!(
            accept_event_path(
                &root,
                &root.join("proj-a/session1/subagents/workflows/wf_1/journal.jsonl"),
                &deny
            ),
            None
        );
        assert_eq!(
            accept_event_path(&root, &root.join("proj-b/session2.jsonl"), &deny),
            None
        );
        assert_eq!(
            accept_event_path(&root, &root.join("proj-a/notes.txt"), &deny),
            None,
            "non-.jsonl files must be ignored"
        );
    }

    #[test]
    fn end_to_end_kind_smoke() {
        // Sanity: a mixed user+assistant transcript round-trips through
        // tail_file into both KIND_USER and KIND_ASSISTANT rows.
        let (_tmp, store) = open_store();
        let root = tempfile::tempdir().unwrap();
        let proj = root.path().join("proj-a");
        std::fs::create_dir_all(&proj).unwrap();
        let file = proj.join("session1.jsonl");
        let assistant = r#"{"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":"s1","timestamp":"2026-07-17T10:00:01.000Z","isSidechain":false,"message":{"role":"assistant","content":[{"type":"text","text":"hi there"}]}}"#;
        let content = format!("{}\n{}\n", user_line("u1", None, "hello"), assistant);
        std::fs::write(&file, &content).unwrap();
        tail_file(&store, root.path(), &file, "proj-a", true).unwrap();

        let hits = store
            .search_transcripts("hello", 10, None, Some(KIND_USER))
            .unwrap();
        assert_eq!(hits.len(), 1);
        let hits2 = store
            .search_transcripts("hi", 10, None, Some(KIND_ASSISTANT))
            .unwrap();
        assert_eq!(hits2.len(), 1);
    }
}
