//! Track V — unified artifact version timeline + text/raw resolution.
//!
//! A *version* of an artifact comes from one of three sources, unified behind
//! [`Version`] + [`VersionCtx`]:
//!
//! - **working** — the live on-disk file (uncommitted); always the newest
//!   entry, read straight from disk.
//! - **git** — a commit that touched the file ([`crate::vcs::git_log`] /
//!   [`crate::vcs::git_show`]).
//! - **index** — a kb content snapshot ([`crate::storage`] `snapshot_*`),
//!   captured by the indexer when the corpus isn't under git (or as the
//!   `auto`-mode fallback for untracked files).
//!
//! Any version ref resolves to bytes via [`VersionCtx::raw_at`]; the prose
//! view ([`VersionCtx::prose_at`]) runs those bytes through
//! [`crate::parser::text_blocks`] (markdown is rendered first) so the diff
//! focuses on the words a reader sees, not the markup. Both the git and index
//! paths funnel through the same prose extraction, so a cross-source diff
//! (`both` mode) stays apples-to-apples.

use crate::storage::StorageHandle;
use crate::vcs::{self, VersionsMode};
use serde::Serialize;
use std::path::Path;

/// The ref for the live, on-disk (uncommitted) working-tree file.
pub const WORKING_REF: &str = "WORKING";

/// How many index snapshots to surface in a timeline (well above the
/// per-artifact retention cap, so it's effectively "all of them").
const SNAPSHOT_LIST_LIMIT: u32 = 200;

/// One entry in an artifact's version timeline.
///
/// No `Deserialize`: `source` is `&'static str` (a closed 3-way enum
/// spelled as a string for wire simplicity), and serde's derived
/// `Deserialize` for a `&'static str` field can't satisfy the fully
/// generic `Deserialize<'de>` bound `serde_json::from_value` needs — a
/// caller wanting to round-trip the wire JSON back into `Version` (MI-
/// W2.4b's `kb diff --between`) maps the wire `source` string onto the
/// matching `&'static str` by hand instead (`kb-cli`'s
/// `parse_versions_wire`).
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Version {
    /// Opaque ref used to fetch this version's bytes: `"WORKING"`, a git
    /// commit sha, or `"index:<rowid>"`. (`ref` in JSON.)
    pub r#ref: String,
    /// `"working"` | `"git"` | `"index"`.
    #[cfg_attr(feature = "ts-export", ts(type = "\"working\" | \"git\" | \"index\""))]
    pub source: &'static str,
    /// Display label: the commit subject for git; empty for index/working
    /// (the SPA/CLI render the timestamp instead).
    pub label: String,
    /// Commit author (git only; empty otherwise).
    pub author: String,
    /// Unix seconds — the sort key + display timestamp.
    pub ts_unix: i64,
    /// Short id for display: a short sha, `"wt"`, or `"#<rowid>"`.
    pub short: String,
}

/// Everything the version engine needs to list + resolve an artifact's
/// revisions. The HTTP route assembles this from the resolved `KbContext`
/// (mode + git root) and the artifact's on-disk path.
pub struct VersionCtx<'a> {
    pub mode: VersionsMode,
    /// Repo root that owns the corpus (walked up from the kb source dir).
    /// `None` when the corpus isn't under git.
    pub git_root: Option<&'a Path>,
    /// The artifact file's path relative to `git_root` (for `git log/show`).
    /// `None` when not under git.
    pub rel_to_git: Option<String>,
    /// Absolute on-disk path — the working-tree source + the file-type sniff
    /// (md vs html) for prose extraction.
    pub abs_path: &'a Path,
    pub storage: &'a StorageHandle,
    /// Path-based artifact id (the snapshot lookup key).
    pub artifact_id: &'a str,
}

impl VersionCtx<'_> {
    /// Build the version timeline per the configured mode. The working tree
    /// (uncommitted current file) is always first; git commits and/or index
    /// snapshots follow, newest-first.
    ///
    /// Mode resolution: `git`/`index` force one source; `both` unions them;
    /// `auto` uses git when the file has commit history and falls back to
    /// index snapshots otherwise (per-file hybrid); `off` returns empty.
    pub async fn list(&self) -> Vec<Version> {
        if self.mode == VersionsMode::Off {
            return Vec::new();
        }
        let mut out = Vec::new();

        // Working tree — the live file, always the newest entry.
        if let Ok(meta) = tokio::fs::metadata(self.abs_path).await {
            let ts = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            out.push(Version {
                r#ref: WORKING_REF.to_string(),
                source: "working",
                label: String::new(),
                author: String::new(),
                ts_unix: ts,
                short: "wt".to_string(),
            });
        }

        // Git commits — fetched whenever the mode could use them, so `auto`
        // can decide based on whether any exist.
        let git_commits = if self.mode.uses_git() {
            match (self.git_root, self.rel_to_git.as_deref()) {
                (Some(root), Some(rel)) => vcs::git_log(root, rel).await,
                _ => Vec::new(),
            }
        } else {
            Vec::new()
        };

        let use_git = match self.mode {
            VersionsMode::Git | VersionsMode::Both => true,
            VersionsMode::Auto => !git_commits.is_empty(),
            VersionsMode::Index | VersionsMode::Off => false,
        };
        let use_index = match self.mode {
            VersionsMode::Index | VersionsMode::Both => true,
            VersionsMode::Auto => git_commits.is_empty(),
            VersionsMode::Git | VersionsMode::Off => false,
        };

        let mut rest: Vec<Version> = Vec::new();
        if use_git {
            for c in git_commits {
                rest.push(Version {
                    r#ref: c.sha,
                    source: "git",
                    label: c.subject,
                    author: c.author,
                    ts_unix: c.ts_unix,
                    short: c.short_sha,
                });
            }
        }
        if use_index {
            if let Ok(rows) = self
                .storage
                .snapshot_list(self.artifact_id.to_string(), SNAPSHOT_LIST_LIMIT)
                .await
            {
                for r in rows {
                    rest.push(Version {
                        r#ref: format!("index:{}", r.id),
                        source: "index",
                        label: String::new(),
                        author: String::new(),
                        ts_unix: r.captured_at,
                        short: format!("#{}", r.id),
                    });
                }
            }
        }
        // Newest-first; stable sort keeps same-second entries in source order.
        rest.sort_by_key(|b| std::cmp::Reverse(b.ts_unix));
        out.extend(rest);
        out
    }

    /// Resolve a version ref to its verbatim source bytes — the raw-diff input
    /// and the text the prose extractor reads.
    pub async fn raw_at(&self, ref_: &str) -> Result<String, String> {
        if ref_ == WORKING_REF {
            return tokio::fs::read(self.abs_path)
                .await
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .map_err(|e| format!("read working tree: {e}"));
        }
        if let Some(id_str) = ref_.strip_prefix("index:") {
            let id: i64 = id_str
                .parse()
                .map_err(|_| format!("malformed snapshot ref: {ref_}"))?;
            return self
                .storage
                .snapshot_raw(id)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("snapshot {id} not found"));
        }
        // Otherwise a git commit sha.
        let (root, rel) = match (self.git_root, self.rel_to_git.as_deref()) {
            (Some(r), Some(p)) => (r, p),
            _ => return Err("git history is not available for this artifact".to_string()),
        };
        let bytes = vcs::git_show(root, ref_, rel).await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Resolve a version ref to its block-structured prose — the
    /// "focus on text, not markup" diff input. Markdown is rendered then
    /// flattened; HTML is flattened directly; other types fall back to raw.
    pub async fn prose_at(&self, ref_: &str) -> Result<String, String> {
        let raw = self.raw_at(ref_).await?;
        Ok(prose_from_source(self.abs_path, &raw))
    }
}

/// MI-W2.4b — `kb diff --between <D1> <D2>`'s resolver (the 2026-07
/// temporal-query design, "one pure nearest-version-at-or-before resolver
/// over the EXISTING versions façade, zero server changes, zero new
/// storage"). Returns the FIRST entry — in `versions`' own order, which
/// `VersionCtx::list()` always produces newest-first (working tree, then
/// git/index sorted by `ts_unix` descending) — whose `ts_unix <= at_unix`.
///
/// Pure and total over the ORDER it's handed: it does not re-sort or
/// re-derive "newest", so a caller passing an already-`list()`-ordered
/// slice gets the documented "nearest version at or before `at_unix`"
/// answer; a caller passing a differently-ordered slice gets "first
/// element in ITS order satisfying the predicate" instead (garbage in,
/// garbage out — the function has no way to tell the two apart, and
/// re-deriving newest-first here would just be a second, potentially
/// drifting copy of `list()`'s own sort). `None` when every version is
/// NEWER than `at_unix` (the date predates the oldest known version) —
/// the caller turns that into a hard, named error rather than silently
/// returning an empty diff.
pub fn resolve_as_of(versions: &[Version], at_unix: i64) -> Option<&Version> {
    versions.iter().find(|v| v.ts_unix <= at_unix)
}

/// CT-F6 — the honest answer [`resolve_as_of`] can't give on its own: WHICH
/// version a `?at=<unix>` coordinate landed on, WHEN that version's own
/// timestamp is, whether the hit was exact or merely the nearest prior, and
/// — on a miss — how far back the timeline actually reaches.
///
/// RFC 7089 (Memento) calls this a "memento" of the resource at a datetime;
/// this is the per-ARTIFACT slice of that idea and nothing more. It is
/// deliberately NOT a corpus timeline and NOT `recall --as-of` (still
/// rejected, invariant #10: pin state / salience / decay policy are read as
/// CURRENT values only, so an as-of RANKING would chimera history against
/// present-day metadata). Nothing here touches ranking — it resolves one
/// artifact's own revision list and stops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Memento<'a> {
    /// Echo of the requested instant (unix seconds).
    pub at_unix: i64,
    /// The newest version at or before `at_unix`; `None` when every known
    /// version is NEWER than that (the coordinate predates the timeline).
    /// **Never** silently the oldest version — a miss stays a miss.
    pub version: Option<&'a Version>,
    /// True only when the resolved version's OWN `ts_unix` equals `at_unix`.
    /// False on every nearest-prior hit, so a caller can never render an
    /// approximate answer as an exact one.
    pub exact: bool,
    /// The oldest known version's timestamp — the floor a miss ran off the
    /// bottom of. `None` only for an empty timeline.
    pub oldest_ts_unix: Option<i64>,
}

impl<'a> Memento<'a> {
    /// Whether the coordinate resolved to a version at all.
    pub fn found(&self) -> bool {
        self.version.is_some()
    }

    /// The honest "nothing is that old" sentence, shared by every presenter
    /// (HTTP body, CLI, SPA copy is its own mirror) so the wording can't
    /// drift between them. `None` when the coordinate DID resolve.
    pub fn miss_note(&self) -> Option<String> {
        if self.found() {
            return None;
        }
        Some(match self.oldest_ts_unix {
            Some(oldest) => format!(
                "no version of this artifact is that old — the oldest known version is {} (unix {oldest})",
                fmt_ts_utc(oldest)
            ),
            None => "this artifact has no recorded versions to resolve against".to_string(),
        })
    }
}

/// Resolve a Memento coordinate over an artifact's version timeline.
///
/// Pure, total, allocation-free. Reuses [`resolve_as_of`] VERBATIM for the
/// pick itself (MI-W2.4b's resolver, already golden-pinned by `kb diff
/// --between`) and only adds the metadata around it — so the two temporal
/// surfaces can never disagree about which version a date names.
///
/// Same order precondition as `resolve_as_of`: `versions` must be in
/// [`VersionCtx::list`]'s own newest-first order. `oldest_ts_unix` uses
/// `min()` rather than "the last element" so it stays correct even for a
/// caller that hands over a differently-ordered slice.
pub fn resolve_memento(versions: &[Version], at_unix: i64) -> Memento<'_> {
    let version = resolve_as_of(versions, at_unix);
    Memento {
        at_unix,
        version,
        exact: version.is_some_and(|v| v.ts_unix == at_unix),
        oldest_ts_unix: versions.iter().map(|v| v.ts_unix).min(),
    }
}

/// `YYYY-MM-DD HH:MM` in UTC — the timestamp rendering shared by the
/// memento miss note and the CLI's timeline output. UTC always: the same
/// coordinate must read identically regardless of the caller's timezone.
fn fmt_ts_utc(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod resolve_as_of_tests {
    use super::*;

    fn v(r#ref: &str, source: &'static str, ts_unix: i64) -> Version {
        Version {
            r#ref: r#ref.into(),
            source,
            label: String::new(),
            author: String::new(),
            ts_unix,
            short: r#ref.into(),
        }
    }

    /// Golden: WORKING wins when `D >= mtime` — WORKING is first in
    /// `list()`'s order, so a date at-or-after its own timestamp resolves
    /// to it immediately, regardless of what (possibly newer-looking, per
    /// clock skew) commits follow.
    #[test]
    fn golden_working_wins_when_at_or_after_its_own_ts() {
        let versions = vec![
            v(WORKING_REF, "working", 300),
            v("deadbeef", "git", 200),
            v("index:1", "index", 100),
        ];
        assert_eq!(resolve_as_of(&versions, 300).unwrap().r#ref, WORKING_REF);
        assert_eq!(resolve_as_of(&versions, 500).unwrap().r#ref, WORKING_REF);
    }

    /// Golden: ties — two versions share `ts_unix == D`; the FIRST one in
    /// the given order wins (stable, not "pick the newer ref" or similar).
    #[test]
    fn golden_tie_at_exact_boundary_picks_the_first_in_order() {
        let versions = vec![v("a", "git", 200), v("b", "git", 200)];
        assert_eq!(resolve_as_of(&versions, 200).unwrap().r#ref, "a");
    }

    /// Golden: below-floor — `D` predates every version → `None`, which
    /// the CLI turns into a hard error naming the oldest known date,
    /// never a silent empty diff.
    #[test]
    fn golden_below_the_oldest_version_is_none() {
        let versions = vec![v(WORKING_REF, "working", 300), v("deadbeef", "git", 200)];
        assert!(resolve_as_of(&versions, 50).is_none());
    }

    /// Golden: non-monotonic git author dates — a rebase can put an OLDER-
    /// authored commit ahead of a NEWER-authored one in `list()`'s own
    /// sort-by-`ts_unix`-descending order is actually impossible (the sort
    /// itself enforces `ts_unix` order) — what CAN happen is two commits
    /// with the same short sha prefix or an author date that doesn't match
    /// commit order; this pins that the resolver only ever trusts the
    /// GIVEN order + `ts_unix`, never re-derives "commit order" from shas
    /// or any other signal.
    #[test]
    fn golden_non_monotonic_author_dates_trusts_the_given_order_only() {
        // "c1" has a LATER sha/short-id but an EARLIER author date than
        // "c2" — exactly what a rebase can produce. `list()` would have
        // sorted these by ts_unix descending (c2 first); resolve_as_of
        // must follow THAT order, not sha/label order.
        let versions = vec![v("c2", "git", 500), v("c1", "git", 100)];
        assert_eq!(resolve_as_of(&versions, 150).unwrap().r#ref, "c1");
        assert_eq!(resolve_as_of(&versions, 500).unwrap().r#ref, "c2");
    }

    #[test]
    fn empty_versions_is_none() {
        assert!(resolve_as_of(&[], 1_000).is_none());
    }

    // ---- CT-F6 — Memento (?at=) resolution -----------------------------

    /// The ordinary case: a coordinate BETWEEN two versions resolves to the
    /// newer of the two older ones — the nearest PRIOR — and says so
    /// (`exact == false`), never rounding forward to the version that
    /// hadn't happened yet.
    #[test]
    fn memento_nearest_prior_never_rounds_forward() {
        let versions = vec![
            v(WORKING_REF, "working", 300),
            v("deadbeef", "git", 200),
            v("index:1", "index", 100),
        ];
        let m = resolve_memento(&versions, 250);
        assert!(m.found());
        assert_eq!(m.version.unwrap().r#ref, "deadbeef");
        assert_eq!(m.version.unwrap().ts_unix, 200);
        assert!(!m.exact, "250 != 200 — an approximate hit is never exact");
        assert_eq!(m.at_unix, 250);
        assert_eq!(m.oldest_ts_unix, Some(100));
        assert_eq!(m.miss_note(), None);
    }

    /// Exact hit: the coordinate IS a version's own timestamp. Same chosen
    /// version as the nearest-prior path (the boundary is inclusive), but
    /// `exact` flips true so a caller can render "as it stood" without the
    /// "nearest prior" hedge.
    #[test]
    fn memento_exact_hit_is_flagged_exact() {
        let versions = vec![
            v(WORKING_REF, "working", 300),
            v("deadbeef", "git", 200),
            v("index:1", "index", 100),
        ];
        let m = resolve_memento(&versions, 200);
        assert_eq!(m.version.unwrap().r#ref, "deadbeef");
        assert!(m.exact);
    }

    /// Too-old: the coordinate predates every known version. The answer is
    /// an explicit MISS naming the floor — NOT the oldest version. This is
    /// the whole point of the type: silently returning the oldest would
    /// claim the artifact existed in that state before it existed at all.
    #[test]
    fn memento_too_old_is_an_explicit_miss_naming_the_floor() {
        let versions = vec![v(WORKING_REF, "working", 300), v("deadbeef", "git", 200)];
        let m = resolve_memento(&versions, 50);
        assert!(!m.found());
        assert!(m.version.is_none());
        assert!(!m.exact);
        assert_eq!(m.oldest_ts_unix, Some(200));
        let note = m.miss_note().expect("a miss must carry a note");
        assert!(note.contains("no version of this artifact is that old"));
        assert!(note.contains("unix 200"), "the floor is named: {note}");
    }

    /// A coordinate NEWER than everything resolves to the newest entry
    /// (the working tree in `list()`'s order) — "as it stands now".
    #[test]
    fn memento_future_coordinate_resolves_to_the_newest() {
        let versions = vec![v(WORKING_REF, "working", 300), v("deadbeef", "git", 200)];
        let m = resolve_memento(&versions, 9_999);
        assert_eq!(m.version.unwrap().r#ref, WORKING_REF);
        assert!(!m.exact);
    }

    /// An empty timeline is a miss with NO floor to name — and still not a
    /// panic or a fabricated answer.
    #[test]
    fn memento_empty_timeline_is_a_miss_with_no_floor() {
        let m = resolve_memento(&[], 1_000);
        assert!(!m.found());
        assert_eq!(m.oldest_ts_unix, None);
        assert!(m.miss_note().unwrap().contains("no recorded versions"));
    }

    /// `resolve_memento` must agree with `resolve_as_of` on the PICK for
    /// every coordinate — it only adds metadata, it never re-decides. Pins
    /// the "one resolver, two surfaces" claim (`kb diff --between` and
    /// `?at=` can't drift apart).
    #[test]
    fn memento_pick_is_resolve_as_of_verbatim() {
        let versions = vec![
            v(WORKING_REF, "working", 300),
            v("deadbeef", "git", 200),
            v("index:1", "index", 100),
        ];
        for at in [0, 99, 100, 101, 199, 200, 201, 299, 300, 301, 10_000] {
            assert_eq!(
                resolve_memento(&versions, at).version.map(|v| &v.r#ref),
                resolve_as_of(&versions, at).map(|v| &v.r#ref),
                "disagreement at {at}"
            );
        }
    }
}

/// Reduce a source file's text to block-structured prose for diffing, by file
/// type. Markdown → rendered HTML → [`crate::parser::text_blocks`]; HTML →
/// `text_blocks`; anything else → the raw text (already prose-like). Keeping
/// this identical for the git, index, and working sources is what makes a
/// cross-source diff meaningful.
pub fn prose_from_source(path: &Path, raw: &str) -> String {
    if crate::indexer::is_markdown(path) {
        let (_fields, wrapped) = crate::parser::extract_markdown(raw);
        crate::parser::text_blocks(&wrapped)
    } else if is_html(path) {
        crate::parser::text_blocks(raw)
    } else {
        raw.to_string()
    }
}

fn is_html(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("html") | Some("htm")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn prose_from_html_strips_markup() {
        let html = r#"<body><h1>Title</h1><p>Hello <b>world</b></p><script>x()</script></body>"#;
        let prose = prose_from_source(Path::new("doc.html"), html);
        assert_eq!(prose, "Title\nHello world");
    }

    #[test]
    fn prose_from_markdown_renders_then_flattens() {
        let md = "# Title\n\nHello **world**\n";
        let prose = prose_from_source(Path::new("doc.md"), md);
        // Rendered then flattened: heading + paragraph on separate lines, no
        // markdown markers.
        assert!(prose.contains("Title"));
        assert!(prose.contains("Hello world"));
        assert!(!prose.contains('#') && !prose.contains('*'));
    }

    #[test]
    fn prose_from_unknown_type_is_raw() {
        let txt = "line one\nline two";
        assert_eq!(prose_from_source(Path::new("notes.txt"), txt), txt);
    }

    #[test]
    fn is_html_matches_extensions() {
        assert!(is_html(Path::new("a.html")));
        assert!(is_html(Path::new("a.HTM")));
        assert!(!is_html(Path::new("a.md")));
        assert!(!is_html(Path::new("a")));
    }
}
