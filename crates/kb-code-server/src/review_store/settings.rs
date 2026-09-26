//! RS-U3 — `[review.store]` / `[[review.repos]]` resolved once at boot
//! (README §11). Enum-valued keys are TOLERANT: an unknown value warns
//! (collected in [`StoreSettings::warnings`], logged at boot, shown by
//! `store doctor`) and falls back to the default — a typo never stops the
//! daemon.
//!
//! # What the store-disabled guard does NOT do
//!
//! The overlap check compares a fixed set of CONFIGURED paths, once, at
//! boot (`StoreSettings::resolve`): each `[[repos]]` work tree, its `.git`
//! directory and the shared common dir the store actually fetches from
//! (see `overlap_targets` below). It is a resident-symlink check on those
//! paths, not a TOCTOU-proof one — a symlink CREATED AFTER boot is not
//! seen, and a repo that gains a linked worktree after boot is not
//! covered either. Re-checking per access would mean a stat/canonicalise
//! walk on every store read for a root only the operator configures.
//! Stated here so the gap is a recorded scope decision, not an
//! oversight — same posture as [`crate::security::paths`]'s own "What
//! this does NOT do".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::cred::{CredentialPin, FetchCredentialConfig};
use crate::config::{RepoEntry, ReviewSection};
use crate::security::paths::canonicalize_lenient;

/// Directory name of the default store root under the daemon's state dir.
pub const DEFAULT_ROOT_DIR: &str = "git";
/// `HOME` for scrubbed store git calls — always under the state dir, never
/// under the (overridable) store root (design §3.1).
pub const GIT_HOME_DIR: &str = "git-home";
/// RS-U9 (README §5.4/§8) — where `store-<uuid>-<ts>.bundle` backups land.
/// Always under the daemon's state dir, same "never under the overridable
/// store root" rule as [`GIT_HOME_DIR`] (a bundle must survive `store rm`/a
/// bad `[review.store] root`).
pub const BACKUPS_DIR_NAME: &str = "backups";
/// RS-U9 — the restore-guard sentinel's file name. Lives beside
/// `backup.marker`/`index.db` (the daemon's state dir, NOT under
/// `[review.store] root`), on purpose: restoring the sqlite volume alone
/// can never also roll this file back, which is exactly the asymmetry the
/// guard's epoch-rollback detector depends on (see
/// `review_store::maint::restore_guard`'s module doc).
pub const RESTORE_GUARD_FILE: &str = "review-store-restore-guard.json";

/// `[[review.repos]] forge`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ForgeKind {
    #[default]
    Auto,
    Github,
    Gitlab,
    Gitea,
    Forgejo,
    BitbucketServer,
    None,
}

impl ForgeKind {
    pub fn parse_tolerant(s: &str) -> (Self, Option<String>) {
        let v = match s.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Self::Auto,
            "github" => Self::Github,
            "gitlab" => Self::Gitlab,
            "gitea" => Self::Gitea,
            "forgejo" => Self::Forgejo,
            "bitbucket-server" => Self::BitbucketServer,
            "none" => Self::None,
            _ => {
                return (
                    Self::Auto,
                    Some("unknown `forge` value; using `auto`".to_string()),
                )
            }
        };
        (v, None)
    }

    pub fn slug(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Github => "github",
            Self::Gitlab => "gitlab",
            Self::Gitea => "gitea",
            Self::Forgejo => "forgejo",
            Self::BitbucketServer => "bitbucket-server",
            Self::None => "none",
        }
    }

    /// `auto` resolved against a store key's host. Only public hosts whose
    /// forge is unambiguous are auto-detected; anything else is `None`
    /// (unknown), and an operator names it with `forge = …`.
    pub fn detect(self, host: Option<&str>) -> Option<ForgeKind> {
        match self {
            Self::Auto => match host? {
                "github.com" => Some(Self::Github),
                "gitlab.com" => Some(Self::Gitlab),
                "codeberg.org" => Some(Self::Forgejo),
                _ => None,
            },
            Self::None => None,
            other => Some(other),
        }
    }

    /// D8: GitHub is the only verified forge in Phase 1.
    pub fn verified(self) -> bool {
        self == Self::Github
    }
}

/// One repo's resolved `[[review.repos]]` entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoStoreSettings {
    pub base_url: Option<String>,
    pub credential: CredentialPin,
    pub gh_user: Option<String>,
    pub token_file: Option<PathBuf>,
    pub default_branch: Option<String>,
    pub forge: ForgeKind,
}

/// Everything the review store reads from config.
#[derive(Debug, Clone)]
pub struct StoreSettings {
    pub root: PathBuf,
    pub git_home: PathBuf,
    /// RS-U9 — `<state>/backups/`; see [`BACKUPS_DIR_NAME`].
    pub backups_dir: PathBuf,
    /// RS-U9 — `<state>/review-store-restore-guard.json`; see
    /// [`RESTORE_GUARD_FILE`].
    pub restore_guard_path: PathBuf,
    pub seed_on_boot: bool,
    pub allow_inherited_credentials: bool,
    pub repos: BTreeMap<String, RepoStoreSettings>,
    /// Tolerant-parse and placement warnings (boot log + doctor).
    pub warnings: Vec<String>,
    /// `Some(reason)` = the store is disabled for this boot (e.g. the root
    /// sits inside a browsed repo — SEC-13/15). Reads fall back to the
    /// user repo; for a post-store install that means the store-only
    /// `refs/kbc/*` names MISS, because those refs live only in the store,
    /// not in the user repo.
    /// Enforced on the read side by `Store::review_store_readable`,
    /// published at boot from `ReviewStores::reads_can_use_store`.
    pub disabled: Option<String>,
}

impl StoreSettings {
    /// Resolve `[review]`'s store keys against the daemon's state dir.
    pub fn resolve(review: &ReviewSection, state_dir: &Path, repos: &[RepoEntry]) -> Self {
        let mut warnings = Vec::new();
        let root = review
            .store
            .root
            .as_deref()
            .map(expand_tilde)
            .unwrap_or_else(|| state_dir.join(DEFAULT_ROOT_DIR));
        let mut disabled = None;
        if !root.is_absolute() {
            disabled = Some("[review.store] root must be an absolute path".to_string());
        } else if let Some((r, what)) = repos.iter().find_map(|r| {
            overlap_targets(&r.path)
                .into_iter()
                .find(|target| contains(target, &root) || contains(&root, target))
                .map(|target| (r.name.clone(), what_label(&r.path, &target)))
        }) {
            disabled = Some(format!(
                "[review.store] root overlaps the browsed repo `{r}` at {what}; the store must live outside every repo and its git dirs"
            ));
        }
        let mut map = BTreeMap::new();
        for e in &review.repos {
            let name = e.name.trim();
            if name.is_empty() {
                warnings.push("[[review.repos]] entry without a `name`; ignored".into());
                continue;
            }
            if !repos.iter().any(|r| r.name == name) {
                warnings.push(format!(
                    "[[review.repos]] `{name}` is not a configured [[repos]] name; ignored"
                ));
                continue;
            }
            if map.contains_key(name) {
                warnings.push(format!(
                    "[[review.repos]] `{name}` appears more than once; the first entry wins"
                ));
                continue;
            }
            let (credential, w) =
                CredentialPin::parse_tolerant(e.credential.as_deref().unwrap_or(""));
            if let Some(w) = w {
                warnings.push(format!("[[review.repos]] `{name}`: {w}"));
            }
            let (forge, w) = ForgeKind::parse_tolerant(e.forge.as_deref().unwrap_or(""));
            if let Some(w) = w {
                warnings.push(format!("[[review.repos]] `{name}`: {w}"));
            }
            let nonempty = |v: &Option<String>| {
                v.as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            map.insert(
                name.to_string(),
                RepoStoreSettings {
                    base_url: nonempty(&e.base_url),
                    credential,
                    gh_user: nonempty(&e.gh_user),
                    token_file: e
                        .token_file
                        .as_deref()
                        .and_then(Path::to_str)
                        .map(expand_tilde),
                    default_branch: nonempty(&e.default_branch),
                    forge,
                },
            );
        }
        Self {
            git_home: state_dir.join(GIT_HOME_DIR),
            backups_dir: state_dir.join(BACKUPS_DIR_NAME),
            restore_guard_path: state_dir.join(RESTORE_GUARD_FILE),
            root,
            seed_on_boot: review.store.seed_on_boot,
            allow_inherited_credentials: review.store.allow_inherited_credentials,
            repos: map,
            warnings,
            disabled,
        }
    }

    pub fn repo(&self, name: &str) -> RepoStoreSettings {
        self.repos.get(name).cloned().unwrap_or_default()
    }

    /// The fetch-ladder inputs for `repo` in a store that has recorded
    /// `recorded_account` (D12).
    pub fn fetch_credential_config(
        &self,
        repo: &str,
        recorded_account: Option<&str>,
    ) -> FetchCredentialConfig {
        let r = self.repo(repo);
        FetchCredentialConfig {
            pin: r.credential,
            gh_user: r.gh_user,
            token_file: r.token_file,
            token_username: None,
            allow_inherited_credentials: self.allow_inherited_credentials,
            recorded_account: recorded_account.map(str::to_string),
        }
    }
}

/// Containment for the store-vs-repo overlap guard, at the same
/// discipline as [`crate::security::paths::contained_abs_path`]:
/// canonicalise BOTH sides, then compare.
///
/// `inner.starts_with(outer)` alone is LEXICAL and not a containment
/// proof: `..` survives `Path::starts_with` as a `Component::ParentDir`
/// (`/kb-git/../repos/widgets` does not start with `/kb-git` but DOES
/// live under `/repos/widgets`), and a symlinked parent is never
/// followed. `canonicalize_lenient` — the crate's existing helper from
/// `security::paths` (the same one `contained_abs_path` uses, whose
/// module doc carries the deepest-existing-ancestor rationale) — keeps
/// the check working for a store root that does not exist yet, which a
/// plain `canonicalize` would reject.
fn contains(outer: &Path, inner: &Path) -> bool {
    // Both directions of the caller's `find` need the canonical form: a
    // repo reached through a symlink, and a root reached through one.
    canonicalize_lenient(inner).starts_with(canonicalize_lenient(outer))
}

/// Every directory a store root must neither sit inside nor contain for
/// one configured `[[repos]] path`: the WORK TREE, the repo's own `.git`
/// directory, and the SHARED COMMON DIR the store fetches from.
///
/// The work tree alone was not enough. `[[repos]] path` is what the daemon
/// browses, but the store reads and writes the repo's git DIRECTORY, so a
/// root inside `<repo>/.git` puts a bare repo inside the very directory
/// the daemon treats as that repo's object store. The common dir is
/// resolved with [`super::seed::common_dir_of`] — the same spawn-free
/// resolver the seeding path uses, not a second one — which walks a
/// linked worktree's `gitdir:` file and then its `commondir`, so a
/// configured path that IS a linked worktree also brings the main
/// repository's `.git` into the set (and with it every
/// `<common>/worktrees/*` gitdir, which lives under it).
fn overlap_targets(repo_path: &Path) -> Vec<PathBuf> {
    // Most specific first, so the doctor reason names the narrowest thing
    // that actually matched.
    let mut out = Vec::new();
    // A non-git `[[repos]] path` (or one that is not readable) simply
    // contributes no git dir: the work tree alone is still compared, and
    // the guard must never be the thing that stops a boot.
    if let Ok(common) = super::seed::common_dir_of(repo_path) {
        out.push(common);
    }
    let dotgit = repo_path.join(".git");
    if dotgit.exists() {
        out.push(dotgit);
    }
    out.push(repo_path.to_path_buf());
    out
}

/// Which of [`overlap_targets`]' entries matched, for the doctor reason.
fn what_label(repo_path: &Path, matched: &Path) -> String {
    if matched == repo_path {
        return "its work tree".to_string();
    }
    if matched.ends_with(".git") && matched.parent() == Some(repo_path) {
        return "its .git directory".to_string();
    }
    "its git common dir".to_string()
}

fn expand_tilde<P: AsRef<Path>>(p: P) -> PathBuf {
    let p = p.as_ref();
    let Some(s) = p.to_str() else {
        return p.to_path_buf();
    };
    if s == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    } else if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    p.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{KbCodeConfig, ReviewRepoEntry};

    fn repos() -> Vec<RepoEntry> {
        vec![
            RepoEntry {
                name: "widgets-01".into(),
                path: "/work/acme/widgets.01".into(),
            },
            RepoEntry {
                name: "widgets-02".into(),
                path: "/work/acme/widgets.02".into(),
            },
        ]
    }

    #[test]
    fn toml_parses_and_defaults_hold() {
        let cfg: KbCodeConfig = toml::from_str(
            r#"
            [review.store]
            seed_on_boot = false
            [[review.repos]]
            name = "widgets-01"
            base_url = "https://github.com/acme/widgets.git"
            credential = "gh-cli"
            gh_user = "someone"
            forge = "github"
            "#,
        )
        .unwrap();
        assert!(!cfg.review.store.seed_on_boot);
        // D7, landed: the ambient `inherit` rung is OFF unless an
        // operator asks for it.
        assert!(!cfg.review.store.allow_inherited_credentials);
        let s = StoreSettings::resolve(&cfg.review, Path::new("/state/kb-code"), &repos());
        assert_eq!(s.root, Path::new("/state/kb-code/git"));
        assert_eq!(s.git_home, Path::new("/state/kb-code/git-home"));
        assert!(s.warnings.is_empty(), "{:?}", s.warnings);
        assert!(s.disabled.is_none());
        let r = s.repo("widgets-01");
        assert_eq!(r.credential, CredentialPin::GhCli);
        assert_eq!(r.forge, ForgeKind::Github);
        assert_eq!(r.gh_user.as_deref(), Some("someone"));
        assert_eq!(s.repo("widgets-02"), RepoStoreSettings::default());
        // …and an explicit opt-in still parses.
        let opted: KbCodeConfig =
            toml::from_str("[review.store]\nallow_inherited_credentials = true\n").unwrap();
        assert!(opted.review.store.allow_inherited_credentials);
        assert!(
            StoreSettings::resolve(&opted.review, Path::new("/state"), &repos())
                .allow_inherited_credentials
        );
        // An absent [review] table keeps every default.
        let d = KbCodeConfig::default();
        assert!(d.review.store.seed_on_boot && d.review.repos.is_empty());
    }

    #[test]
    fn unknown_enums_warn_and_fall_back() {
        let mut review = ReviewSection::default();
        review.repos.push(ReviewRepoEntry {
            name: "widgets-01".into(),
            credential: Some("telepathy".into()),
            forge: Some("sourceforge".into()),
            ..Default::default()
        });
        review.repos.push(ReviewRepoEntry {
            name: "not-a-repo".into(),
            ..Default::default()
        });
        review.repos.push(ReviewRepoEntry {
            name: "widgets-01".into(),
            ..Default::default()
        });
        let s = StoreSettings::resolve(&review, Path::new("/state"), &repos());
        let r = s.repo("widgets-01");
        assert_eq!(r.credential, CredentialPin::Auto);
        assert_eq!(r.forge, ForgeKind::Auto);
        assert_eq!(s.warnings.len(), 4, "{:?}", s.warnings);
    }

    #[test]
    fn a_root_inside_a_repo_disables_the_store() {
        let mut review = ReviewSection::default();
        review.store.root = Some("/work/acme/widgets.01/.kb".into());
        let s = StoreSettings::resolve(&review, Path::new("/state"), &repos());
        assert!(s.disabled.unwrap().contains("widgets-01"));
        review.store.root = Some("relative/git".into());
        let s = StoreSettings::resolve(&review, Path::new("/state"), &repos());
        assert!(s.disabled.is_some());
    }

    /// The guard is a CONTAINMENT proof, not a lexical prefix test, and it
    /// compares against the repo's git directories as well as its work
    /// tree. Every case below is a root an operator can actually write in
    /// the config.
    #[test]
    fn the_overlap_guard_canonicalises_both_sides_and_the_git_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let repo = base.join("widgets-01");
        std::fs::create_dir_all(repo.join(".git/refs")).unwrap();
        let link = base.join("widgets-01-link");
        std::os::unix::fs::symlink(&repo, &link).unwrap();
        let repos = |p: &Path| {
            vec![RepoEntry {
                name: "widgets-01".into(),
                path: p.to_path_buf(),
            }]
        };
        let disabled_for = |root: PathBuf| {
            let mut review = ReviewSection::default();
            review.store.root = Some(root);
            StoreSettings::resolve(&review, Path::new("/state"), &repos(&repo)).disabled
        };

        // Inside the work tree, the obvious case.
        let d = disabled_for(repo.join(".kb")).unwrap();
        assert!(d.contains("work tree"), "{d}");

        // `..` is a lexical no-op for `Path::starts_with` but not a
        // containment fact: this root is inside the repo.
        assert!(disabled_for(base.join("widgets-01/../widgets-01/.kb")).is_some());

        // A symlinked parent must not smuggle the root past the guard.
        assert!(disabled_for(link.join(".kb")).is_some());

        // INSIDE the repo's `.git` — the store would be a bare repo nested
        // in the very directory the store itself fetches from.
        let d = disabled_for(repo.join(".git/store.git")).unwrap();
        assert!(d.contains("git dir"), "{d}");

        // The CONTAINS direction: a root that swallows the repo.
        assert!(disabled_for(base.to_path_buf()).is_some());
        assert!(disabled_for(repo.join(".git")).is_some());

        // A string prefix that is not a path prefix is NOT an overlap,
        // in either direction.
        let sibling = base.join("widgets-011/store");
        assert!(
            disabled_for(sibling.clone()).is_none(),
            "{sibling:?} is a sibling, not a child"
        );
        let outer = base.join("widgets-0");
        assert!(
            disabled_for(outer.clone()).is_none(),
            "{outer:?} does not contain the repo"
        );
        // A genuinely separate root is untouched.
        assert!(disabled_for(PathBuf::from("/var/lib/kb-code/git")).is_none());
    }

    /// A `[[repos]] path` that IS a linked worktree brings the main
    /// repository's common dir into the compared set — that common dir is
    /// what the store fetches from, and every `<common>/worktrees/*`
    /// gitdir lives under it.
    #[test]
    fn a_linked_worktree_repo_pulls_in_the_shared_common_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let main = base.join("widgets-01");
        let wt = base.join("widgets-01-wt");
        // A hand-built linked worktree: `<wt>/.git` is a `gitdir:` file
        // pointing into the main repo, whose own `commondir` climbs back.
        std::fs::create_dir_all(main.join(".git/worktrees/wt")).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", main.join(".git/worktrees/wt").display()),
        )
        .unwrap();
        std::fs::write(main.join(".git/worktrees/wt/commondir"), "../..\n").unwrap();

        let repos = vec![RepoEntry {
            name: "widgets-01".into(),
            path: wt.clone(),
        }];
        let resolve = |root: &Path| {
            let mut review = ReviewSection::default();
            review.store.root = Some(root.to_path_buf());
            StoreSettings::resolve(&review, Path::new("/state"), &repos).disabled
        };
        // Inside the main repo's common dir, reached from the worktree
        // entry.
        let d = resolve(&main.join(".git/objects/pack")).unwrap();
        assert!(d.contains("common dir"), "{d}");
        // …and inside the worktree's own gitdir, which lives under it.
        assert!(resolve(&main.join(".git/worktrees/wt")).is_some());
        // The worktree itself is still compared, of course.
        assert!(resolve(&wt.join(".kb")).is_some());
        // An unrelated sibling is still fine.
        assert!(resolve(&base.join("other/store.git")).is_none());
    }

    #[test]
    fn forge_detection_and_verification() {
        assert_eq!(
            ForgeKind::Auto.detect(Some("github.com")),
            Some(ForgeKind::Github)
        );
        assert_eq!(ForgeKind::Auto.detect(Some("git.example.com")), None);
        assert_eq!(
            ForgeKind::Gitlab.detect(Some("git.example.com")),
            Some(ForgeKind::Gitlab)
        );
        assert_eq!(ForgeKind::None.detect(Some("github.com")), None);
        assert!(ForgeKind::Github.verified());
        assert!(!ForgeKind::Gitlab.verified());
    }
}
