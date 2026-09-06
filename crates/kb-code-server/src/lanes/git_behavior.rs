//! `git.behavior` — the one DERIVED lane (V72-H4a).
//!
//! Three facts about a path, all of them read out of the repository's own
//! history and none of them stored:
//!
//! * **`churn`** — how many non-merge commits touched the file in the last
//!   [`WINDOW_DAYS`] days, and how many distinct authors made them.
//! * **`co_change`** — the files that changed in at least
//!   [`MIN_SHARED`] of those same commits, capped at [`PARTNER_CAP`] and
//!   reporting the true total beside the capped list (the usages/2 rule:
//!   never a silent cap).
//! * **`last_touch`** — the newest commit touching the file, and whether
//!   an agent or a human made it.
//!
//! ## Why this lane spawns only `git`
//!
//! Invariant 10 — the daemon never spawns a non-git process — is what
//! makes `derived` and `ingested` the registry's two kinds rather than the
//! design sketch's four source kinds. Every call here goes through
//! [`crate::history::run_git_raw`] (so this file adds nothing to the
//! git-argv lint's `GIT_SPAWNING_FILES` allowlist), every caller-supplied
//! pathspec is pushed after an explicit `--`, and the only other argv
//! values are shas this daemon read out of its own `git log` output and
//! re-validated as 40 hex characters before passing them back.
//!
//! ## The agent-vs-human rule, and why it is capped
//!
//! Agent authorship is read from the commit's OFFICIAL trailer block
//! (`git log --format=%(trailers:key=Kb-Session,valueonly)` — git's own
//! trailer parse, the same `Kb-Session:` key `join::local` documents and
//! kb's capture hook stamps). A trailer PRESENT is proof, so that fact
//! carries no extra cap and reaches `exact` when the blob is current. A
//! trailer ABSENT is not proof of a human — it is the absence of evidence,
//! since a commit can be agent-authored and untagged — so the fact caps
//! ITSELF at `likely` through `FactAnchor::cap`. That per-fact cap is a
//! property of this one claim, not of the lane, which is exactly why the
//! classing function takes both.
//!
//! ## Freshness
//!
//! Facts are computed on demand and cached in process, keyed
//! `(repo_root, HEAD, path)`. A commit moves HEAD and every cached entry
//! for the old HEAD becomes unreachable; nothing is invalidated, nothing
//! is persisted, and a restart simply recomputes. The cache is bounded and
//! cleared wholesale on overflow — a code-reading daemon browsing a
//! handful of repos never reaches the bound, and an LRU here would be
//! machinery in service of a case that does not occur.

use super::classing::SHA_SOURCE_TOOL;
use super::TrustClass;
use crate::history::run_git_raw;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

/// The churn/co-change window, in days.
pub const WINDOW_DAYS: i64 = 90;
/// How many commits the window walk reads at most. Bounds both the log
/// parse and the argv of the co-change `git show`.
pub const COMMIT_CAP: usize = 100;
/// A file must share at least this many commits with the subject to count
/// as a co-change partner.
pub const MIN_SHARED: usize = 2;
/// How many partners are RETURNED; the true total rides beside them.
pub const PARTNER_CAP: usize = 10;
/// How many `(repo, HEAD, path)` entries the process-local cache holds
/// before it is cleared wholesale.
pub const CACHE_CAP: usize = 4_096;

/// One derived fact, in the shape the facts route classes and renders.
/// It carries no `sha_source` field because a derived fact always has
/// one: this daemon computed it against the blob it names, so the
/// producer named the blob ([`SHA_SOURCE_TOOL`]).
#[derive(Debug, Clone, PartialEq)]
pub struct DerivedFact {
    pub kind: &'static str,
    pub value: Value,
    /// A ceiling this fact declares for itself — see the module doc.
    pub cap: Option<TrustClass>,
    pub produced_at: i64,
}

impl DerivedFact {
    pub fn sha_source(&self) -> &'static str {
        SHA_SOURCE_TOOL
    }
}

type Cache = Mutex<BTreeMap<(String, String, String), Vec<DerivedFact>>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// `true` for a full 40-character lowercase-or-uppercase hex sha. Used to
/// re-validate a sha this daemon read out of its OWN `git log` output
/// before passing it back to `git show` — cheap, and deliberately NOT
/// shaped like `git/revspec.rs`'s leading-dash predicate, which the
/// git-argv lint requires to live in exactly one file.
fn is_full_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The repo's current HEAD sha, or `None` when there is no commit yet (a
/// freshly `git init`ed repo) or git failed.
pub fn head_sha(repo_root: &Path) -> Option<String> {
    let out = run_git_raw(repo_root, &["rev-parse", "HEAD"]).ok()?;
    let s = String::from_utf8_lossy(&out).trim().to_string();
    is_full_sha(&s).then_some(s)
}

/// The lane's facts for one path, memoised on `(repo_root, HEAD, path)`.
///
/// `None` when the repo has no HEAD (nothing to derive from) — an honest
/// absence, never an empty fact set that reads as "no churn".
pub fn facts_for_path(repo_root: &Path, path: &str, now_unix: i64) -> Option<Vec<DerivedFact>> {
    let head = head_sha(repo_root)?;
    let key = (
        repo_root.to_string_lossy().to_string(),
        head,
        path.to_string(),
    );
    if let Ok(c) = cache().lock() {
        if let Some(hit) = c.get(&key) {
            return Some(hit.clone());
        }
    }
    let facts = derive(repo_root, path, now_unix);
    if let Ok(mut c) = cache().lock() {
        if c.len() >= CACHE_CAP {
            c.clear();
        }
        c.insert(key, facts.clone());
    }
    Some(facts)
}

/// Clear the process-local cache. Test-only: the cache is keyed on a
/// tempdir path plus a HEAD sha, so tests never alias each other, but a
/// test that rewrites history under the SAME HEAD needs a way back.
#[cfg(test)]
pub fn clear_cache() {
    if let Ok(mut c) = cache().lock() {
        c.clear();
    }
}

fn derive(repo_root: &Path, path: &str, now_unix: i64) -> Vec<DerivedFact> {
    let mut out = Vec::new();
    let since = format!("--since=@{}", now_unix - WINDOW_DAYS * 86_400);
    let n = COMMIT_CAP.to_string();

    // (1) the commits in the window that touched this path.
    let log = run_git_raw(
        repo_root,
        &[
            "log",
            "--no-merges",
            "--format=%H%x1f%an%x1f%at",
            "-n",
            &n,
            &since,
            "--",
            path,
        ],
    );
    let Ok(log) = log else { return out };
    let text = String::from_utf8_lossy(&log);
    let mut shas: Vec<String> = Vec::new();
    let mut authors: std::collections::BTreeSet<String> = Default::default();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let mut f = line.splitn(3, '\u{1f}');
        let (Some(sha), Some(author)) = (f.next(), f.next()) else {
            continue;
        };
        if !is_full_sha(sha) {
            continue;
        }
        shas.push(sha.to_string());
        authors.insert(author.to_string());
    }

    out.push(DerivedFact {
        kind: "churn",
        value: json!({
            "commits": shas.len(),
            "authors": authors.len(),
            "window_days": WINDOW_DAYS,
            "capped": shas.len() >= COMMIT_CAP,
        }),
        cap: None,
        produced_at: now_unix,
    });

    // (2) co-change partners over exactly those commits.
    if !shas.is_empty() {
        if let Some(fact) = co_change(repo_root, path, &shas, now_unix) {
            out.push(fact);
        }
    }

    // (3) the newest commit touching the path, and who made it.
    if let Some(fact) = last_touch(repo_root, path, now_unix) {
        out.push(fact);
    }
    out
}

fn co_change(repo_root: &Path, path: &str, shas: &[String], now_unix: i64) -> Option<DerivedFact> {
    let mut args: Vec<&str> = vec!["show", "--no-renames", "--format=%x1e%H", "--name-only"];
    for s in shas {
        args.push(s.as_str());
    }
    let out = run_git_raw(repo_root, &args).ok()?;
    let text = String::from_utf8_lossy(&out);
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for chunk in text.split('\u{1e}').skip(1) {
        let mut lines = chunk.lines();
        // The first line of a chunk is the sha the format printed.
        lines.next();
        for name in lines.map(str::trim).filter(|l| !l.is_empty()) {
            if name == path {
                continue;
            }
            *counts.entry(name).or_default() += 1;
        }
    }
    let mut partners: Vec<(&str, usize)> = counts
        .into_iter()
        .filter(|(_, c)| *c >= MIN_SHARED)
        .collect();
    // Strongest first, then by path so the list is deterministic.
    partners.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let total = partners.len();
    let shown: Vec<Value> = partners
        .iter()
        .take(PARTNER_CAP)
        .map(|(p, c)| json!({ "path": p, "commits": c }))
        .collect();
    Some(DerivedFact {
        kind: "co_change",
        value: json!({
            "partners": shown,
            "total": total,
            "truncated": total > PARTNER_CAP,
            "min_shared": MIN_SHARED,
            "window_days": WINDOW_DAYS,
            "over_commits": shas.len(),
        }),
        cap: None,
        produced_at: now_unix,
    })
}

fn last_touch(repo_root: &Path, path: &str, now_unix: i64) -> Option<DerivedFact> {
    let out = run_git_raw(
        repo_root,
        &[
            "log",
            "-n",
            "1",
            "--format=%H%x1f%an%x1f%at%x1e%(trailers:key=Kb-Session,valueonly)",
            "--",
            path,
        ],
    )
    .ok()?;
    let text = String::from_utf8_lossy(&out);
    let (head, trailers) = text.split_once('\u{1e}')?;
    let mut f = head.splitn(3, '\u{1f}');
    let sha = f.next()?.trim().to_string();
    let author = f.next()?.to_string();
    let at: i64 = f.next()?.trim().parse().ok()?;
    if !is_full_sha(&sha) {
        return None;
    }
    let session_id = trailers
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|s| s.to_string());
    let (author_kind, basis, cap) = match &session_id {
        // A trailer present IS the evidence: no extra cap.
        Some(_) => ("agent", "kb-session-trailer", None),
        // Absence of a trailer is not evidence of a human.
        None => ("human", "no-kb-session-trailer", Some(TrustClass::Likely)),
    };
    Some(DerivedFact {
        kind: "last_touch",
        value: json!({
            "sha": sha,
            "author": author,
            "at": at,
            "author_kind": author_kind,
            "basis": basis,
            "session_id": session_id,
        }),
        cap,
        produced_at: now_unix,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn commit(dir: &Path, files: &[(&str, &str)], message: &str, at: i64) {
        for (name, body) in files {
            let p = dir.join(name);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(p, body).unwrap();
        }
        git(dir, &["add", "-A"]);
        let date = format!("{at} +0000");
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["commit", "-q", "-m", message])
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .status()
            .unwrap();
        assert!(status.success());
    }

    /// now, and a repo whose commits all sit inside the window.
    const NOW: i64 = 1_800_000_000;

    fn fixture() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test"]);
        // Three commits touch a.rb; two of them also touch b.rb, one also
        // touches c.rb — so b.rb is a partner (2 shared) and c.rb is not.
        commit(
            dir,
            &[("a.rb", "one\n"), ("b.rb", "b1\n")],
            "c1",
            NOW - 5 * 86_400,
        );
        commit(
            dir,
            &[("a.rb", "one\ntwo\n"), ("b.rb", "b2\n")],
            "c2",
            NOW - 4 * 86_400,
        );
        commit(
            dir,
            &[("a.rb", "one\ntwo\nthree\n"), ("c.rb", "c1\n")],
            "c3\n\nKb-Session: sess-abc\n",
            NOW - 3 * 86_400,
        );
        tmp
    }

    #[test]
    fn churn_counts_commits_and_authors_in_the_window() {
        let tmp = fixture();
        clear_cache();
        let facts = facts_for_path(tmp.path(), "a.rb", NOW).expect("a repo with HEAD");
        let churn = facts.iter().find(|f| f.kind == "churn").unwrap();
        assert_eq!(churn.value["commits"], 3);
        assert_eq!(churn.value["authors"], 1);
        assert_eq!(churn.value["window_days"], WINDOW_DAYS);
        assert_eq!(churn.value["capped"], false);
    }

    #[test]
    fn a_commit_outside_the_window_is_not_counted() {
        let tmp = fixture();
        clear_cache();
        // Move `now` forward a year: every commit falls out of the window.
        let facts = facts_for_path(tmp.path(), "a.rb", NOW + 400 * 86_400).expect("HEAD");
        let churn = facts.iter().find(|f| f.kind == "churn").unwrap();
        assert_eq!(churn.value["commits"], 0);
        // `last_touch` is deliberately NOT window-scoped: "who touched this
        // last" has an answer however old it is.
        assert!(facts.iter().any(|f| f.kind == "last_touch"));
    }

    #[test]
    fn co_change_reports_partners_above_the_threshold_only() {
        let tmp = fixture();
        clear_cache();
        let facts = facts_for_path(tmp.path(), "a.rb", NOW).expect("HEAD");
        let cc = facts.iter().find(|f| f.kind == "co_change").unwrap();
        let partners = cc.value["partners"].as_array().unwrap();
        assert_eq!(partners.len(), 1, "{:?}", cc.value);
        assert_eq!(partners[0]["path"], "b.rb");
        assert_eq!(partners[0]["commits"], 2);
        assert_eq!(cc.value["total"], 1);
        assert_eq!(cc.value["truncated"], false);
        assert_eq!(cc.value["min_shared"], MIN_SHARED);
    }

    #[test]
    fn an_agent_commit_is_named_by_its_trailer_and_carries_no_extra_cap() {
        let tmp = fixture();
        clear_cache();
        let facts = facts_for_path(tmp.path(), "a.rb", NOW).expect("HEAD");
        let lt = facts.iter().find(|f| f.kind == "last_touch").unwrap();
        assert_eq!(lt.value["author_kind"], "agent");
        assert_eq!(lt.value["session_id"], "sess-abc");
        assert_eq!(lt.value["basis"], "kb-session-trailer");
        assert_eq!(lt.cap, None, "a trailer present IS the evidence");
    }

    #[test]
    fn a_human_commit_caps_itself_at_likely_because_absence_is_not_evidence() {
        let tmp = fixture();
        clear_cache();
        let facts = facts_for_path(tmp.path(), "b.rb", NOW).expect("HEAD");
        let lt = facts.iter().find(|f| f.kind == "last_touch").unwrap();
        assert_eq!(lt.value["author_kind"], "human");
        assert_eq!(lt.value["basis"], "no-kb-session-trailer");
        assert_eq!(lt.cap, Some(TrustClass::Likely));
    }

    #[test]
    fn a_repo_with_no_head_derives_nothing_rather_than_an_empty_fact_set() {
        let tmp = tempfile::tempdir().unwrap();
        git(tmp.path(), &["init", "-q", "-b", "main"]);
        clear_cache();
        assert!(facts_for_path(tmp.path(), "a.rb", NOW).is_none());
    }

    #[test]
    fn every_derived_kind_is_declared_in_the_registry() {
        let tmp = fixture();
        clear_cache();
        let spec = super::super::resolve(super::super::GIT_BEHAVIOR).unwrap();
        for f in facts_for_path(tmp.path(), "a.rb", NOW).unwrap() {
            assert!(
                spec.accepts_kind(f.kind),
                "{} is not in the registry's fact_kinds",
                f.kind
            );
        }
    }

    #[test]
    fn is_full_sha_accepts_only_a_forty_character_hex_string() {
        assert!(is_full_sha(&"a".repeat(40)));
        assert!(!is_full_sha(&"a".repeat(39)));
        assert!(!is_full_sha(&"g".repeat(40)));
        assert!(!is_full_sha("--upload-pack=evil"));
    }
}
