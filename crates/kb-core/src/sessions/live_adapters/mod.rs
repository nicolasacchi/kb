//! LSC-5 (`docs/research/kb-live-sessions-cockpit-2026-08.html` §4
//! "Collection, harness by harness") — the four non-Claude-Code adapters,
//! each a sibling of [`super::live`]'s Claude Code adapter, plus
//! [`scan_all`], the ONE unified entry point `kb sessions status` fans out
//! to.
//!
//! Module-per-harness (not one `live_adapters.rs` monolith): the four
//! on-disk layouts, turn-boundary vocabularies, and traps are all
//! genuinely different (a rollout file vs. a session directory vs. a
//! sqlite row), and the existing per-harness CAPTURE adapters
//! (`plugins/kb-memory/hooks/kb-capture-{codex,grok,kimi,opencode}.sh`)
//! already established this split for the same reason — mirroring it here
//! keeps each harness's parsing logic independently reviewable and
//! independently testable, the same way the bash scripts are.
//!
//! Every adapter shares [`super::live`]'s three non-negotiable rules:
//! bounded reads only (`super::live::read_tail_lines`'s
//! [`super::live::LIVE_TAIL_BYTES`] tail, reused verbatim — never a full
//! parse), honest `None` on genuine ambiguity (never a guessed
//! [`super::live::Holder`]), and silence never flips the holder axis —
//! only [`super::live::derive_state`] (called, never reimplemented) turns
//! the two axes into a [`super::live::LiveState`].

use std::path::{Path, PathBuf};

use super::live::{self, LivePolicy, LiveSession};

pub mod codex;
pub mod grok;
pub mod kimi;
pub mod opencode;

pub use live::LIVE_SCAN_CAP;

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// Per-harness root overrides for [`scan_all`]. Every field defaults to
/// `None` (or empty, for [`LiveRoots::kimi`]) — call [`LiveRoots::with_defaults`]
/// to fill gaps with each harness's real default location before scanning;
/// an explicitly-set field is never touched.
#[derive(Debug, Clone, Default)]
pub struct LiveRoots {
    /// `~/.claude/projects` by default. Kept `Option<PathBuf>` (not
    /// resolved here) so callers that already have their own
    /// config-aware resolution (`kb sessions status --local`'s
    /// `[sessions] live_transcripts_dir` precedence, unchanged by LSC-5)
    /// can set it explicitly and skip this module's default entirely.
    pub claude: Option<PathBuf>,
    /// `~/.codex/sessions` by default.
    pub codex: Option<PathBuf>,
    /// `~/.grok/sessions` by default.
    pub grok: Option<PathBuf>,
    /// ZERO or more Kimi Code homes — `~/.kimi-code` (the shared,
    /// interactive home) by default, but `kimiclaude` worker jobs run
    /// `kimi -p` inside bubblewrap with a PER-JOB `KIMI_CODE_HOME` (design
    /// §4's Kimi trap), so a caller that also wants worker-job sessions
    /// appends those homes here rather than this module guessing at job
    /// directory layout it has no ground truth for.
    pub kimi: Vec<PathBuf>,
    /// `~/.local/share/opencode/opencode.db` by default. A file path, not
    /// a directory — opencode sessions are database rows (see
    /// `opencode.rs`'s module docs).
    pub opencode: Option<PathBuf>,
}

impl LiveRoots {
    /// Fill every unset field with its harness's real default location —
    /// an explicitly-set field is NEVER overwritten. Uses `$HOME`; see
    /// [`with_defaults_from`] for the deterministic, unit-testable core
    /// (this is a one-line wrapper over it using the real environment).
    pub fn with_defaults(self) -> Self {
        let home = home_dir();
        self.with_defaults_from(home.as_deref())
    }

    /// The pure core of [`with_defaults`] — `home` is passed in explicitly
    /// rather than read from the environment, so tests can exercise the
    /// defaulting logic deterministically without mutating process-global
    /// env state (`std::env::set_var` races across parallel `cargo test`
    /// threads). `home: None` (no `$HOME`) leaves every unset field as
    /// `None`/empty — [`scan_all`] then honestly skips that harness rather
    /// than guessing a path.
    pub fn with_defaults_from(mut self, home: Option<&Path>) -> Self {
        if let Some(home) = home {
            self.claude
                .get_or_insert_with(|| home.join(".claude").join("projects"));
            self.codex
                .get_or_insert_with(|| home.join(".codex").join("sessions"));
            self.grok
                .get_or_insert_with(|| home.join(".grok").join("sessions"));
            if self.kimi.is_empty() {
                self.kimi.push(home.join(".kimi-code"));
            }
            self.opencode.get_or_insert_with(|| {
                home.join(".local")
                    .join("share")
                    .join("opencode")
                    .join("opencode.db")
            });
        }
        self
    }
}

/// THE unified entry point `kb sessions status` fans out to across every
/// harness (LSC-5's work order). Claude Code's existing
/// [`live::scan_claude_projects`] is folded in UNCHANGED — same call, same
/// arguments, same behavior; this function adds nothing to its path, only
/// runs it alongside four siblings. A harness whose root is `None` (never
/// defaulted, or explicitly left unset) is silently skipped — not an
/// error, since "this harness isn't installed on this box" is the common
/// case for at least one of these four on most boxes.
pub fn scan_all(
    roots: &LiveRoots,
    now_unix: i64,
    policy: &LivePolicy,
    max_age_secs: i64,
) -> Vec<LiveSession> {
    let mut out = Vec::new();
    if let Some(root) = &roots.claude {
        out.extend(live::scan_claude_projects(
            root,
            now_unix,
            policy,
            max_age_secs,
        ));
    }
    if let Some(root) = &roots.codex {
        out.extend(codex::scan_codex(root, now_unix, policy, max_age_secs));
    }
    if let Some(root) = &roots.grok {
        out.extend(grok::scan_grok(root, now_unix, policy, max_age_secs));
    }
    for home in &roots.kimi {
        out.extend(kimi::scan_kimi(home, now_unix, policy, max_age_secs));
    }
    if let Some(db) = &roots.opencode {
        out.extend(opencode::scan_opencode(db, now_unix, policy, max_age_secs));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn with_defaults_from_fills_every_field_from_home() {
        let home = Path::new("/home/testuser");
        let roots = LiveRoots::default().with_defaults_from(Some(home));
        assert_eq!(
            roots.claude.as_deref(),
            Some(Path::new("/home/testuser/.claude/projects"))
        );
        assert_eq!(
            roots.codex.as_deref(),
            Some(Path::new("/home/testuser/.codex/sessions"))
        );
        assert_eq!(
            roots.grok.as_deref(),
            Some(Path::new("/home/testuser/.grok/sessions"))
        );
        assert_eq!(roots.kimi, vec![PathBuf::from("/home/testuser/.kimi-code")]);
        assert_eq!(
            roots.opencode.as_deref(),
            Some(Path::new(
                "/home/testuser/.local/share/opencode/opencode.db"
            ))
        );
    }

    #[test]
    fn with_defaults_from_never_overwrites_an_explicit_override() {
        let home = Path::new("/home/testuser");
        let roots = LiveRoots {
            codex: Some(PathBuf::from("/custom/codex/root")),
            kimi: vec![PathBuf::from("/custom/kimi/job-home")],
            ..Default::default()
        }
        .with_defaults_from(Some(home));
        assert_eq!(
            roots.codex.as_deref(),
            Some(Path::new("/custom/codex/root"))
        );
        // kimi is non-empty already — with_defaults_from must not APPEND
        // the shared-home default on top of an explicit override.
        assert_eq!(roots.kimi, vec![PathBuf::from("/custom/kimi/job-home")]);
        // Untouched fields still get filled.
        assert_eq!(
            roots.grok.as_deref(),
            Some(Path::new("/home/testuser/.grok/sessions"))
        );
    }

    #[test]
    fn with_defaults_from_none_home_leaves_every_field_unset() {
        let roots = LiveRoots::default().with_defaults_from(None);
        assert!(roots.claude.is_none());
        assert!(roots.codex.is_none());
        assert!(roots.grok.is_none());
        assert!(roots.kimi.is_empty());
        assert!(roots.opencode.is_none());
    }

    #[test]
    fn scan_all_skips_every_harness_whose_root_is_none() {
        let roots = LiveRoots::default();
        let got = scan_all(&roots, 100_000_000, &LivePolicy::default(), 86_400);
        assert!(got.is_empty());
    }

    /// Claude Code's existing scan is folded in UNCHANGED: build one fixture
    /// tree and confirm `scan_all` with only `claude` set returns exactly
    /// what a direct `live::scan_claude_projects` call returns.
    #[test]
    fn scan_all_folds_in_claude_code_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("-home-user-project-kb");
        fs::create_dir_all(&proj).unwrap();
        fs::write(
            proj.join("sess.jsonl"),
            b"{\"type\":\"assistant\",\"message\":{\"stop_reason\":\"tool_use\"}}\n",
        )
        .unwrap();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let policy = LivePolicy::default();

        let direct = live::scan_claude_projects(tmp.path(), now, &policy, 86_400);
        let roots = LiveRoots {
            claude: Some(tmp.path().to_path_buf()),
            ..Default::default()
        };
        let via_scan_all = scan_all(&roots, now, &policy, 86_400);
        assert_eq!(direct, via_scan_all);
        assert_eq!(via_scan_all.len(), 1);
    }

    /// Every harness contributes when every root is set — a small
    /// end-to-end fixture per adapter, one row each.
    #[test]
    fn scan_all_aggregates_every_harness() {
        let tmp = tempfile::tempdir().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let policy = LivePolicy::default();

        // claude
        let claude_root = tmp.path().join("claude-projects");
        let cproj = claude_root.join("-proj");
        fs::create_dir_all(&cproj).unwrap();
        fs::write(
            cproj.join("csid.jsonl"),
            b"{\"type\":\"assistant\",\"message\":{\"stop_reason\":\"tool_use\"}}\n",
        )
        .unwrap();

        // codex
        let codex_root = tmp.path().join("codex-sessions");
        let cdir = codex_root.join("2026").join("08").join("21");
        fs::create_dir_all(&cdir).unwrap();
        fs::write(
            cdir.join("rollout-2026-08-21T00-00-00-01a025fd-51eb-7cd3-86f2-95261769cb14.jsonl"),
            b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n",
        )
        .unwrap();

        // grok
        let grok_root = tmp.path().join("grok-sessions");
        let gdir = grok_root.join("%2Ftmp").join("gsid");
        fs::create_dir_all(&gdir).unwrap();
        fs::write(gdir.join("events.jsonl"), b"{\"type\":\"turn_started\"}\n").unwrap();

        // kimi
        let kimi_home = tmp.path().join("kimi-home");
        let kdir = kimi_home.join("sessions").join("wd_x").join("session_ksid");
        fs::create_dir_all(kdir.join("agents").join("main")).unwrap();
        fs::write(
            kdir.join("agents").join("main").join("wire.jsonl"),
            b"{\"type\":\"turn.prompt\"}\n",
        )
        .unwrap();

        // opencode
        let oc_path = tmp.path().join("opencode.db");
        {
            let conn = rusqlite::Connection::open(&oc_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT, title TEXT, \
                    time_created INTEGER, time_updated INTEGER, time_archived INTEGER);
                 CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT, \
                    time_created INTEGER, time_updated INTEGER, data TEXT);",
            )
            .unwrap();
            let now_ms = now * 1000;
            conn.execute(
                "INSERT INTO session VALUES ('ocsid', '/tmp', 'oc', ?1, ?1, NULL)",
                [now_ms],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO message VALUES ('m1', 'ocsid', ?1, ?1, '{\"role\":\"user\"}')",
                [now_ms],
            )
            .unwrap();
        }

        let roots = LiveRoots {
            claude: Some(claude_root),
            codex: Some(codex_root),
            grok: Some(grok_root),
            kimi: vec![kimi_home],
            opencode: Some(oc_path),
        };
        let got = scan_all(&roots, now, &policy, 86_400);
        let harnesses: std::collections::BTreeSet<&str> =
            got.iter().map(|s| s.harness.as_str()).collect();
        assert_eq!(
            harnesses,
            ["claude", "codex", "grok", "kimi", "opencode"]
                .into_iter()
                .collect()
        );
        assert_eq!(got.len(), 5);
    }
}
