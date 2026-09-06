//! XDG-style path resolver. Constructed once at daemon start; threaded
//! through state. Topic 01 §Decisions specifies
//! `~/.local/state/kb/<daemon>/<kb>/` for storage.

use crate::slate::SlateSlug;
use crate::types::KbName;
use crate::{Error, Result};
use directories::ProjectDirs;
use std::path::{Path, PathBuf};

/// Daemon-scoped paths. `<daemon>` is interpolated by `KbPaths::new`.
#[derive(Debug, Clone)]
pub struct KbPaths {
    /// `<XDG_STATE_HOME>/kb/<daemon>/`
    pub state: PathBuf,
    /// `<XDG_CONFIG_HOME>/kb/`
    pub config: PathBuf,
    /// `<XDG_CACHE_HOME>/kb/`
    pub cache: PathBuf,
    /// `<state>/log/`
    pub log: PathBuf,
    /// `<state>/runs/`
    pub runs: PathBuf,
    /// `<state>/quarantine/`
    pub quarantine: PathBuf,
    /// `<state>/exports/`
    pub exports: PathBuf,
    /// Daemon name (also segment in state path).
    pub daemon_name: String,
}

impl KbPaths {
    pub fn new(daemon_name: impl Into<String>) -> Result<Self> {
        let daemon_name = daemon_name.into();
        if daemon_name.is_empty() {
            return Err(Error::Config("daemon name must not be empty".into()));
        }

        // Resolve the three base dirs. Precedence (highest first):
        //   per-dir override  KB_STATE_DIR / KB_CONFIG_DIR / KB_CACHE_DIR
        //   single-root       KB_HOME  (→ <home>/{state,config,cache})
        //   platform-native   directories::ProjectDirs
        // These env overrides are the portable, cross-platform knob:
        // `ProjectDirs` only honours `XDG_*` on Linux; tests and
        // containers still need `KB_HOME` (and the per-dir overrides)
        // to redirect paths deterministically.
        // `<daemon>` is joined onto the state root below; `KB_STATE_DIR` is
        // that root (the per-daemon segment is still appended).
        let env_dir = |k: &str| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };

        let (mut state_root, mut config, mut cache) = if let Some(home) = env_dir("KB_HOME") {
            (home.join("state"), home.join("config"), home.join("cache"))
        } else {
            let dirs = ProjectDirs::from("", "", "kb").ok_or_else(|| {
                Error::Config("could not resolve platform project directories".into())
            })?;
            let state_root = dirs
                .state_dir()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| dirs.data_local_dir().to_path_buf());
            (
                state_root,
                dirs.config_dir().to_path_buf(),
                dirs.cache_dir().to_path_buf(),
            )
        };
        if let Some(s) = env_dir("KB_STATE_DIR") {
            state_root = s;
        }
        if let Some(c) = env_dir("KB_CONFIG_DIR") {
            config = c;
        }
        if let Some(c) = env_dir("KB_CACHE_DIR") {
            cache = c;
        }

        let state = state_root.join(&daemon_name);
        Ok(Self {
            log: state.join("log"),
            runs: state.join("runs"),
            quarantine: state.join("quarantine"),
            exports: state.join("exports"),
            state,
            config,
            cache,
            daemon_name,
        })
    }

    /// Construct a `KbPaths` rooted at an arbitrary directory. For tests.
    pub fn rooted_at(root: &Path, daemon_name: impl Into<String>) -> Self {
        let daemon_name = daemon_name.into();
        let state = root.join("state").join(&daemon_name);
        Self {
            log: state.join("log"),
            runs: state.join("runs"),
            quarantine: state.join("quarantine"),
            exports: state.join("exports"),
            state,
            config: root.join("config"),
            cache: root.join("cache"),
            daemon_name,
        }
    }

    /// `<state>/<kb>/`
    pub fn kb_state(&self, kb: &KbName) -> PathBuf {
        self.state.join(kb.as_str())
    }

    /// `<state>/<kb>/lance/`
    pub fn kb_lance(&self, kb: &KbName) -> PathBuf {
        self.kb_state(kb).join("lance")
    }

    /// `<state>/<kb>/index.db`
    pub fn kb_sqlite(&self, kb: &KbName) -> PathBuf {
        self.kb_state(kb).join("index.db")
    }

    /// `<state>/<kb>/.review/` — directory where per-artifact review JSON
    /// files (kb-comments/1) live. Hidden so it doesn't pollute the
    /// kb's source folder if someone points one at the state dir.
    pub fn kb_review_dir(&self, kb: &KbName) -> PathBuf {
        self.kb_state(kb).join(".review")
    }

    /// `<state>/<kb>/.review/<id>.json` — review file for a single artifact.
    pub fn kb_review_file(&self, kb: &KbName, id: &str) -> PathBuf {
        self.kb_review_dir(kb).join(format!("{id}.json"))
    }

    /// `<state>/<kb>/.attachments/<artifact_id>/` — per-artifact blob dir
    /// for comment attachments (Y-track). Sibling of `.review/`; daemon
    /// state, never the kb source folder. The caller MUST validate
    /// `artifact_id` (no `..`, no path separators) before constructing this
    /// — the `join` would otherwise traverse.
    pub fn kb_attachment_dir(&self, kb: &KbName, artifact_id: &str) -> PathBuf {
        self.kb_state(kb).join(".attachments").join(artifact_id)
    }

    /// `<state>/<kb>/.attachments/<artifact_id>/<aid>` — one attachment blob
    /// (raw bytes, no extension; the content-type lives in the manifest).
    /// Validate `artifact_id` AND `aid` before calling.
    pub fn kb_attachment_blob(&self, kb: &KbName, artifact_id: &str, aid: &str) -> PathBuf {
        self.kb_attachment_dir(kb, artifact_id).join(aid)
    }

    /// `<state>/<kb>/.attachments/<artifact_id>/_manifest.json` — the
    /// per-artifact attachment metadata map. `_manifest.json` can never
    /// collide with an `a_<hex>` blob name. Guarded by the same per-kb
    /// `review_lock` as the review file.
    pub fn kb_attachment_manifest(&self, kb: &KbName, artifact_id: &str) -> PathBuf {
        self.kb_attachment_dir(kb, artifact_id)
            .join("_manifest.json")
    }

    /// `<state>/<kb>/.proposals/` — W2.15b tribal-knowledge proposal inbox.
    /// Sibling of `.review/`: per-kb sidecar dir of `kb-proposal/1` JSON
    /// files, each a memory CANDIDATE awaiting human approve/reject.
    /// Guarded by its own per-kb lock (`KbHandles::proposal_lock_for`) —
    /// distinct from the `.review/` lock, since proposals never touch
    /// comments.
    pub fn kb_proposals_dir(&self, kb: &KbName) -> PathBuf {
        self.kb_state(kb).join(".proposals")
    }

    /// `<state>/<kb>/.proposals/<id>.json` — one queued proposal.
    pub fn kb_proposal_file(&self, kb: &KbName, id: &str) -> PathBuf {
        self.kb_proposals_dir(kb).join(format!("{id}.json"))
    }

    /// `<state>/quarantine/<kb>/` — per-kb quarantine bucket. Mirrors
    /// the `quarantine_dir` computed inside `kb-server::lib::run_indexer`
    /// so the un-quarantine HTTP route lands on the same files.
    pub fn quarantine_kb_dir(&self, kb: &KbName) -> PathBuf {
        self.quarantine.join(kb.as_str())
    }

    /// `<config>/kb.toml`
    pub fn config_file(&self) -> PathBuf {
        self.config.join("kb.toml")
    }

    /// `<config>/daemons.toml`
    pub fn daemons_file(&self) -> PathBuf {
        self.config.join("daemons.toml")
    }

    /// `<config>/token` — v0.4 bearer-token file. Single shared token
    /// in v0.4; per-kb ACLs + multi-token defer to v0.5+. Intended file
    /// mode: 0600 (the `kb token generate` CLI sets this).
    pub fn token_file(&self) -> PathBuf {
        self.config.join("token")
    }

    /// `<config>/tokens` — v0.34 Y1 multi-user token registry.
    /// One entry per line: `<user>:sha256:<hex>` (preferred) or
    /// `<user>:<plaintext>`. Parsed by
    /// [`crate::identity::parse_tokens_file`]. Missing file = empty
    /// registry (not an error).
    pub fn tokens_file(&self) -> PathBuf {
        self.config.join("tokens")
    }

    /// `<state>/memory-policy.json` — daemon-wide decay-policy
    /// persistence (v0.12). The /api/memory/policy PUT writes here;
    /// boot reads it back. Survives restart without baking the policy
    /// into kb.toml (per-kb override stays a v0.13 task).
    pub fn memory_policy_file(&self) -> PathBuf {
        self.state.join("memory-policy.json")
    }

    /// `<state>/query-embed-cache.json` — daemon-wide persisted query-embedding
    /// LRU. Saved on shutdown, reloaded on boot so a restart (incl. the CE
    /// in-process config restart) doesn't re-pay the embed for hot queries.
    pub fn embed_cache_file(&self) -> PathBuf {
        self.state.join("query-embed-cache.json")
    }

    /// `<state>/saved-queries.json` — daemon-wide saved-query store
    /// (v0.13 Q4). The SPA POSTs/DELETEs here; reads come from a
    /// single GET that returns the full list. Cross-device sync via
    /// the daemon; the SPA's localStorage still serves as an
    /// offline-first cache.
    pub fn saved_queries_file(&self) -> PathBuf {
        self.state.join("saved-queries.json")
    }

    /// `<state>/tombstone-era.json` — MI-W2.4c EPOCH HONESTY marker.
    /// Written ONCE, the first time this daemon process observes it's
    /// absent (`kb_server::state::ensure_tombstone_era`, called at
    /// `KbHandles::new` — every boot, idempotent), holding the unix
    /// timestamp this specific daemon started being able to honestly
    /// tombstone a delete (MI-W2.3's soft forget) instead of destroying it
    /// with no trace. `kb memory log` / `kb diff --between` compare a
    /// requested window's start against this marker and print an explicit
    /// caveat when it predates it — pre-marker deletions are genuinely
    /// unrecoverable and undetectable, so silence would UNDER-report a
    /// real gap rather than admit the daemon can't see that far back.
    pub fn tombstone_era_file(&self) -> PathBuf {
        self.state.join("tombstone-era.json")
    }

    /// `<state>/slates/<slug>/` — SL1/D2: the slate store is DAEMON-WIDE
    /// and keyed on the project SLUG, not on a corpus. A project maps to
    /// several kbs and not every project has one, so a per-kb home would
    /// be the wrong join; sqlite would drag in schema-epoch and
    /// refuse-boot coupling (#2's `kb-sibling/1`) for no coordination
    /// gain. One directory per slug, always — `rotate` archives inside it
    /// rather than minting a `<slug>@2` the slug grammar refuses.
    pub fn slate_dir(&self, slug: &SlateSlug) -> PathBuf {
        self.state.join("slates").join(slug.as_str())
    }

    /// `<state>/slates/<slug>/ledger.jsonl` — the append-only truth.
    /// JSONL rather than one JSON per entry because ORDERING is the
    /// semantics here (cursors, deltas, supersede chains).
    pub fn slate_ledger_file(&self, slug: &SlateSlug) -> PathBuf {
        self.slate_dir(slug).join("ledger.jsonl")
    }

    /// `<state>/slates/<slug>/meta.json` — `kb-slate/1` schema, slug,
    /// created/closed, `head_seq` (the revision token, not an
    /// mtime-dependent sha), generation and `rotated_from`.
    pub fn slate_meta_file(&self, slug: &SlateSlug) -> PathBuf {
        self.slate_dir(slug).join("meta.json")
    }

    /// `<state>/slates/<slug>/ledger.<gen>.jsonl` — what `rotate` moves
    /// the current ledger to before starting a fresh one at
    /// `generation + 1`. Archives are for distill; `history` and `--all`
    /// read the CURRENT generation only.
    pub fn slate_archive_file(&self, slug: &SlateSlug, generation: u32) -> PathBuf {
        self.slate_dir(slug)
            .join(format!("ledger.{generation}.jsonl"))
    }

    /// `<state>/kb-daemon.pid` — pid file written by `kb daemon` on
    /// successful bind, removed on clean exit. `kb daemon stop` reads
    /// it to know which process to signal. Deep-review LOW (kb daemon
    /// lifecycle).
    ///
    /// Located in `state` (not `runs`) so it sits alongside the lance
    /// dataset + sqlite — natural for `kb reset` to clean up too.
    pub fn daemon_pid_file(&self) -> PathBuf {
        self.state.join("kb-daemon.pid")
    }

    /// Create every directory listed (state, log, runs, quarantine, exports,
    /// config, cache). Idempotent. Called by the daemon at startup.
    pub fn ensure_dirs(&self) -> Result<()> {
        for dir in [
            &self.state,
            &self.log,
            &self.runs,
            &self.quarantine,
            &self.exports,
            &self.config,
            &self.cache,
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}

/// Full path of a doc relative to the kb source root, forward-slash
/// separated (e.g. `"changelog/daily/2026-05-13.html"`). Returns `""`
/// when the path doesn't sit under `source_root`.
///
/// This is the canonical identity input for `ArtifactId::from_path`:
/// hashing the source-relative path keeps an artifact's id stable
/// across content edits (and across kb relocation), unlike a content
/// hash.
///
/// Both sides are canonicalised before stripping when possible — the
/// indexer's `Doc.path` is the path the watcher emitted, which on
/// some platforms surfaces symlink-resolved forms, while
/// `KbContext.source_path` may still be the user's pre-canonical form.
/// On canonicalise failure (e.g., the source file got deleted after
/// indexing) we fall back to raw-prefix matching.
pub fn doc_rel_path(absolute_path: &str, source_root: &Path) -> String {
    let path = Path::new(absolute_path);
    let canon_root = cached_canonical_root(source_root);
    let canon_path = path.canonicalize();
    let (root, p) = match (canon_root.as_ref(), canon_path.as_ref()) {
        (Some(r), Ok(p)) => (r.as_path(), p.as_path()),
        _ => (source_root, path),
    };
    match p.strip_prefix(root) {
        // Normalise Windows-style separators if they ever appear (kb targets
        // Linux/WSL2, where paths are forward-slash; defensive).
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => String::new(),
    }
}

/// Source-relative form of an ALREADY-CANONICAL absolute path — the zero-
/// syscall fast path for stored `Doc.path` values, which `index_file`
/// writes canonical (invariant #27). Callers pass a once-canonicalised
/// root ([`canonical_abs`]); the strip is then pure string work, so loops
/// over the whole corpus (the wikilink candidate build) don't pay two
/// `canonicalize` syscalls per row like [`doc_rel_path`] does.
///
/// Falls back to stripping against the raw `canonical_root` argument
/// verbatim — identical to [`doc_rel_path`]'s failure path — so a root
/// whose canonicalise failed (deleted mid-run) degrades the same way.
pub fn doc_rel_path_from_canonical(canonical_path: &str, canonical_root: &Path) -> String {
    match Path::new(canonical_path).strip_prefix(canonical_root) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => String::new(),
    }
}

/// Per-process memo of `source_root.canonicalize()`. A kb's source root is
/// constant for the daemon's life (`run_with_ingest` resolves it once at
/// startup; reconcile re-canonicalises once per pass), yet `doc_rel_path`
/// used to re-canonicalise it on EVERY call — twice per indexed file via
/// `identity_for`, plus once per corpus row in `doc_folder` scans. Roots
/// are few (one per kb), so this is a tiny map consulted instead of a
/// syscall. Failures are NOT cached (a root created after the first probe
/// — watcher racing mkdir — must still resolve on retry). The one
/// behaviour delta: a root whose canonical TARGET is swapped mid-process
/// keeps its first resolution — consistent with the indexer's own
/// resolve-once-at-startup contract.
fn cached_canonical_root(source_root: &Path) -> Option<PathBuf> {
    static ROOTS: std::sync::LazyLock<
        std::sync::Mutex<std::collections::HashMap<PathBuf, PathBuf>>,
    > = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut map = ROOTS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(hit) = map.get(source_root) {
        return Some(hit.clone());
    }
    let canon = source_root.canonicalize().ok()?;
    map.insert(source_root.to_path_buf(), canon.clone());
    Some(canon)
}

/// Canonicalise an absolute path, falling back to the input unchanged when
/// the filesystem can't resolve it (file just deleted, permission error,
/// non-existent). Stored `Doc.path` values and reconcile's walk root go
/// through this so a symlinked source root — WSL/bind mounts —
/// doesn't produce raw-vs-canonical mismatches in
/// the reconcile delete pass or `get_by_source_path`. Artifact IDs are
/// unaffected: they derive from the source-relative path via
/// [`doc_rel_path`], which canonicalises both sides.
pub fn canonical_abs(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Derive a doc's folder (its parent directory relative to the kb's
/// source root) from the absolute path stored on the row.
///
/// Returns:
/// - `""` for docs sitting directly under the kb source root
/// - `"changelog/daily"` for `<root>/changelog/daily/2026-05-13.html`
/// - `""` when the path doesn't start with `source_root` (defensive
///   fallback; callers may render this as "(root)" or hide it)
pub fn doc_folder(absolute_path: &str, source_root: &Path) -> String {
    // `rsplit_once('/')` of "foo.html" is None; of "a/b/foo.html" is
    // `("a/b", "foo.html")`.
    match doc_rel_path(absolute_path, source_root).rsplit_once('/') {
        Some((parent, _)) => parent.to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rooted_at_composes_paths_correctly() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = KbPaths::rooted_at(tmp.path(), "smoke");
        assert_eq!(paths.daemon_name, "smoke");
        assert_eq!(paths.state, tmp.path().join("state").join("smoke"));
        assert_eq!(paths.log, paths.state.join("log"));
        assert_eq!(paths.runs, paths.state.join("runs"));
        assert_eq!(paths.quarantine, paths.state.join("quarantine"));
        assert_eq!(paths.exports, paths.state.join("exports"));
    }

    #[test]
    fn kb_paths_join_correctly() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = KbPaths::rooted_at(tmp.path(), "smoke");
        let kb = KbName::new("canon").unwrap();
        assert_eq!(paths.kb_state(&kb), tmp.path().join("state/smoke/canon"));
        assert_eq!(
            paths.kb_lance(&kb),
            tmp.path().join("state/smoke/canon/lance")
        );
        assert_eq!(
            paths.kb_sqlite(&kb),
            tmp.path().join("state/smoke/canon/index.db")
        );
        assert_eq!(
            paths.kb_review_dir(&kb),
            tmp.path().join("state/smoke/canon/.review")
        );
        assert_eq!(
            paths.kb_review_file(&kb, "abc123def456"),
            tmp.path()
                .join("state/smoke/canon/.review/abc123def456.json")
        );
        assert_eq!(
            paths.kb_attachment_dir(&kb, "abc123def456"),
            tmp.path()
                .join("state/smoke/canon/.attachments/abc123def456")
        );
        assert_eq!(
            paths.kb_attachment_blob(&kb, "abc123def456", "a_deadbeef0001"),
            tmp.path()
                .join("state/smoke/canon/.attachments/abc123def456/a_deadbeef0001")
        );
        assert_eq!(
            paths.kb_proposals_dir(&kb),
            tmp.path().join("state/smoke/canon/.proposals")
        );
        assert_eq!(
            paths.kb_proposal_file(&kb, "p_abc123def456"),
            tmp.path()
                .join("state/smoke/canon/.proposals/p_abc123def456.json")
        );
        let slug = SlateSlug::new("kb").unwrap();
        assert_eq!(
            paths.slate_dir(&slug),
            tmp.path().join("state/smoke/slates/kb")
        );
        assert_eq!(
            paths.slate_ledger_file(&slug),
            tmp.path().join("state/smoke/slates/kb/ledger.jsonl")
        );
        assert_eq!(
            paths.slate_meta_file(&slug),
            tmp.path().join("state/smoke/slates/kb/meta.json")
        );
        assert_eq!(
            paths.slate_archive_file(&slug, 2),
            tmp.path().join("state/smoke/slates/kb/ledger.2.jsonl")
        );
        assert_eq!(
            paths.kb_attachment_manifest(&kb, "abc123def456"),
            tmp.path()
                .join("state/smoke/canon/.attachments/abc123def456/_manifest.json")
        );
    }

    #[test]
    fn ensure_dirs_creates_everything() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = KbPaths::rooted_at(tmp.path(), "smoke");
        paths.ensure_dirs().unwrap();
        assert!(paths.state.is_dir());
        assert!(paths.log.is_dir());
        assert!(paths.runs.is_dir());
        assert!(paths.quarantine.is_dir());
        assert!(paths.config.is_dir());
        assert!(paths.cache.is_dir());
        // Idempotent.
        paths.ensure_dirs().unwrap();
    }

    #[test]
    fn empty_daemon_name_is_rejected() {
        assert!(KbPaths::new("").is_err());
    }

    #[test]
    fn token_file_is_under_config_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = KbPaths::rooted_at(tmp.path(), "smoke");
        assert_eq!(paths.token_file(), tmp.path().join("config").join("token"));
    }

    // --- doc_folder cases --------------------------------------------------

    fn make_tree(root: &Path, rel: &str) -> std::path::PathBuf {
        let full = root.join(rel);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, b"hi").unwrap();
        full
    }

    #[test]
    fn doc_folder_returns_empty_for_root_level_doc() {
        let tmp = tempfile::tempdir().unwrap();
        let p = make_tree(tmp.path(), "INDEX.html");
        assert_eq!(doc_folder(&p.to_string_lossy(), tmp.path()), "");
    }

    #[test]
    fn doc_folder_returns_single_segment_for_one_level_nested() {
        let tmp = tempfile::tempdir().unwrap();
        let p = make_tree(tmp.path(), "ideas/INDEX.html");
        assert_eq!(doc_folder(&p.to_string_lossy(), tmp.path()), "ideas");
    }

    #[test]
    fn doc_folder_returns_full_relative_dir_for_deep_nesting() {
        let tmp = tempfile::tempdir().unwrap();
        let p = make_tree(tmp.path(), "ideas/passwordless-login/02-mechanisms.html");
        assert_eq!(
            doc_folder(&p.to_string_lossy(), tmp.path()),
            "ideas/passwordless-login"
        );
    }

    #[test]
    fn doc_folder_returns_empty_when_path_not_under_source_root() {
        let tmp = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let p = make_tree(other.path(), "foo.html");
        assert_eq!(doc_folder(&p.to_string_lossy(), tmp.path()), "");
    }

    #[test]
    fn doc_folder_handles_source_root_with_trailing_slash() {
        let tmp = tempfile::tempdir().unwrap();
        let p = make_tree(tmp.path(), "a/b/c.html");
        // PathBuf::canonicalize normalises the trailing slash, so the
        // helper should still find the right prefix.
        let trailing = format!("{}/", tmp.path().to_string_lossy());
        assert_eq!(
            doc_folder(&p.to_string_lossy(), Path::new(&trailing)),
            "a/b"
        );
    }

    #[test]
    fn doc_rel_path_from_canonical_matches_doc_rel_path_for_stored_paths() {
        // Stored `Doc.path` is canonical (invariant #27); the zero-syscall
        // strip must agree with the canonicalising helper on those inputs.
        let tmp = tempfile::tempdir().unwrap();
        let p = make_tree(tmp.path(), "changelog/daily/2026-05-13.html");
        let canon_root = canonical_abs(tmp.path());
        let stored = canonical_abs(&p).to_string_lossy().to_string();
        assert_eq!(
            doc_rel_path_from_canonical(&stored, &canon_root),
            doc_rel_path(&stored, tmp.path()),
        );
        assert_eq!(
            doc_rel_path_from_canonical(&stored, &canon_root),
            "changelog/daily/2026-05-13.html"
        );
        // Not under the root → "" (same defensive fallback).
        let other = tempfile::tempdir().unwrap();
        let q = make_tree(other.path(), "elsewhere.html");
        let stored_q = canonical_abs(&q).to_string_lossy().to_string();
        assert_eq!(doc_rel_path_from_canonical(&stored_q, &canon_root), "");
    }

    // invariant:27 source-relative-id
    #[cfg(unix)]
    #[test]
    fn doc_rel_path_from_canonical_handles_symlinked_root() {
        // A symlinked source root (WSL/bind-mount shape): the raw
        // root differs from the canonical form the stored paths carry.
        let real = tempfile::tempdir().unwrap();
        let link_holder = tempfile::tempdir().unwrap();
        let link = link_holder.path().join("root-link");
        std::os::unix::fs::symlink(real.path(), &link).unwrap();
        let p = make_tree(real.path(), "notes/deploy.md");
        let stored = canonical_abs(&p).to_string_lossy().to_string();
        let canon_root = canonical_abs(&link);
        assert_eq!(
            doc_rel_path_from_canonical(&stored, &canon_root),
            "notes/deploy.md"
        );
        // And agrees with the full canonicalising path given the raw link.
        assert_eq!(doc_rel_path(&stored, &link), "notes/deploy.md");
    }

    #[test]
    fn doc_folder_does_not_canonicalize_when_path_missing() {
        // Falls back to raw-prefix when canonicalize fails (file gone).
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("changelog").join("daily").join("gone.html");
        assert_eq!(
            doc_folder(&p.to_string_lossy(), tmp.path()),
            "changelog/daily"
        );
    }
}
