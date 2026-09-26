//! RS-U6 — base-model tests. The first half is PURE (the `--base` grammar,
//! `legacy_spec`, the resolution chain, `decide_kind`); the second half
//! drives a READY review store end to end over hermetic fixtures: a local
//! bare "forge" (synthetic acme/widgets, `refs/pull/<n>/head` pushed by
//! hand), one member clone, a temp DB. No network. Test-only direct `git`
//! spawns build the fixtures (this file is in SEC-17's
//! `GIT_SPAWNING_FILES`).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::capture::{CaptureOpts, Member, NewReview, Recapture, StoreCtx};
use super::*;
use crate::config::{RepoEntry, ReviewSection};
use crate::review_store::registry::{Registration, ReviewStores};
use crate::store::{ReviewRow, Store};
use kb_core::events::EventBus;

// =====================================================================
// pure: grammar
// =====================================================================

#[derive(Default)]
struct TableProbe {
    remotes: Vec<String>,
    local: Vec<&'static str>,
    remote: Vec<&'static str>,
    revs: BTreeMap<&'static str, &'static str>,
}

impl BaseProbe for TableProbe {
    fn mapped_remotes(&self) -> Vec<String> {
        self.remotes.clone()
    }
    fn local_branch_exists(&self, b: &str) -> bool {
        self.local.contains(&b)
    }
    fn remote_branch_exists(&self, b: &str) -> bool {
        self.remote.contains(&b)
    }
    fn resolve_rev(&self, rev: &Revspec) -> Option<String> {
        self.revs.get(rev.as_str()).map(|s| s.to_string())
    }
}

const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn probe() -> TableProbe {
    TableProbe {
        remotes: vec!["origin".into(), "team/upstream".into()],
        local: vec!["main", "feature/x", "origin/odd"],
        remote: vec!["main", "release/2.1", "develop"],
        revs: BTreeMap::from([
            (SHA_A, SHA_A),
            ("abc1234", SHA_B),
            ("v1.0", SHA_B),
            ("HEAD~3", SHA_B),
            ("main", SHA_A),
            ("refs/tags/v1.0", SHA_B),
        ]),
    }
}

fn mode_branch(c: &Classified) -> (Option<BaseMode>, Option<String>, Option<String>) {
    match &c.policy {
        Some(p) => (Some(p.mode), p.branch.clone(), p.pin.clone()),
        None => (None, None, None),
    }
}

fn codes(w: &[BaseWarningOut]) -> Vec<&str> {
    w.iter().map(|w| w.code.as_str()).collect()
}

#[test]
fn grammar_table() {
    use BaseMode::*;
    let p = probe();
    // (input, is_pr, mode, branch, pin, warning codes)
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        Option<&str>,
        bool,
        Option<BaseMode>,
        Option<&str>,
        Option<&str>,
        Vec<&str>,
    )> = vec![
        (None, true, None, None, None, vec![]),
        (Some(""), false, None, None, None, vec![]),
        (Some("auto"), true, None, None, None, vec![]),
        (
            Some("pin:abc1234"),
            false,
            Some(Pin),
            None,
            Some(SHA_B),
            vec![],
        ),
        (
            Some("local:dev"),
            true,
            Some(Local),
            Some("dev"),
            None,
            vec![],
        ),
        (
            Some("track:release/2.1"),
            false,
            Some(Track),
            Some("release/2.1"),
            None,
            vec![],
        ),
        (
            Some(SHA_A),
            true,
            Some(Pin),
            None,
            Some(SHA_A),
            vec![warn::BASE_PINNED],
        ),
        (
            Some("refs/remotes/origin/main"),
            false,
            Some(Track),
            Some("main"),
            None,
            vec![],
        ),
        (
            Some("refs/remotes/team/upstream/release/2.1"),
            true,
            Some(Track),
            Some("release/2.1"),
            None,
            vec![],
        ),
        (
            Some("origin/main"),
            false,
            Some(Track),
            Some("main"),
            None,
            vec![],
        ),
        (
            Some("refs/heads/dev"),
            true,
            Some(Local),
            Some("dev"),
            None,
            vec![],
        ),
        // bare on a PR → track, even when the member has it locally.
        (Some("main"), true, Some(Track), Some("main"), None, vec![]),
        // bare, non-PR: local when the member has it…
        (Some("main"), false, Some(Local), Some("main"), None, vec![]),
        (
            Some("feature/x"),
            false,
            Some(Local),
            Some("feature/x"),
            None,
            vec![],
        ),
        // …else track when the forge has it.
        (
            Some("develop"),
            false,
            Some(Track),
            Some("develop"),
            None,
            vec![],
        ),
        // `<R>/<B>` with a LOCAL branch of that literal name stays local.
        (
            Some("origin/odd"),
            false,
            Some(Local),
            Some("origin/odd"),
            None,
            vec![],
        ),
        // other revs → pin + warning.
        (
            Some("abc1234"),
            false,
            Some(Pin),
            None,
            Some(SHA_B),
            vec![warn::BASE_PINNED],
        ),
        (
            Some("v1.0"),
            false,
            Some(Pin),
            None,
            Some(SHA_B),
            vec![warn::BASE_PINNED],
        ),
        (
            Some("HEAD~3"),
            false,
            Some(Pin),
            None,
            Some(SHA_B),
            vec![warn::BASE_PINNED],
        ),
        (
            Some("refs/tags/v1.0"),
            true,
            Some(Pin),
            None,
            Some(SHA_B),
            vec![warn::BASE_PINNED],
        ),
    ];
    for (input, is_pr, mode, branch, pin, warns) in rows {
        let c = classify_base(input, is_pr, &p)
            .unwrap_or_else(|e| panic!("{input:?} (pr={is_pr}) refused: {e}"));
        assert_eq!(
            mode_branch(&c),
            (mode, branch.map(str::to_string), pin.map(str::to_string)),
            "{input:?} (pr={is_pr})"
        );
        assert_eq!(codes(&c.warnings), warns, "{input:?} (pr={is_pr})");
        if let Some(pol) = &c.policy {
            assert_eq!(pol.set_by, SetBy::User, "{input:?}");
            assert_eq!(pol.source, BaseSource::Explicit, "{input:?}");
        }
    }
}

#[test]
fn grammar_refusals_are_base_unresolved() {
    let p = probe();
    for (input, is_pr) in [
        ("nope-not-a-branch", false),
        ("pin:does-not-exist", false),
        ("local:-rf", true),
        ("track:bad..name", true),
        ("track:with space", false),
        ("local:a:b", false),
        (&"c".repeat(40), false),
        ("refs/heads/..", false),
        ("--upload-pack=x", false),
        ("main@{upstream}", false),
    ] {
        match classify_base(Some(input), is_pr, &p) {
            Err(e) => assert_eq!(e.urn, URN_BASE_UNRESOLVED, "{input:?}"),
            Ok(c) => panic!("{input:?} classified as {c:?}"),
        }
    }
}

#[test]
fn branch_names_follow_check_ref_format() {
    for ok in ["main", "release/2.1", "feature/x-y_z", "v1.0"] {
        assert!(valid_branch_name(ok), "{ok}");
    }
    for bad in [
        "",
        "HEAD",
        "-x",
        "a..b",
        "a b",
        "a:b",
        "a~1",
        "a^",
        "a?",
        "a*",
        "a[",
        "a\\b",
        "a/",
        "a.lock",
        "/a",
        "a//b",
        ".a",
        "refs/heads/x",
        "x@{1}",
    ] {
        assert!(!valid_branch_name(bad), "{bad:?}");
    }
}

// =====================================================================
// pure: legacy_spec
// =====================================================================

#[test]
fn legacy_spec_table() {
    let mapped = vec!["origin".to_string()];
    // pinned commit → pin, legacy, amber warning; never upgraded.
    let c = legacy_spec(SHA_A, true, Some("main"), &mapped);
    match &c.effective {
        EffectiveBase::Policy(p) => {
            assert_eq!(p.mode, BaseMode::Pin);
            assert_eq!(p.set_by, SetBy::Legacy);
            assert_eq!(p.pin.as_deref(), Some(SHA_A));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(codes(&c.warnings), vec![warn::BASE_PINNED]);
    assert!(!c.upgraded);

    // refs/remotes/<mapped>/B → track(B), auto.
    for (r, b) in [
        ("refs/remotes/origin/main", "main"),
        ("refs/remotes/origin/release/2.1", "release/2.1"),
        ("origin/main", "main"),
    ] {
        let c = legacy_spec(r, false, None, &mapped);
        assert!(c.upgraded, "{r}");
        assert_eq!(
            c.effective,
            EffectiveBase::Policy(BasePolicy::track(b, SetBy::Auto, BaseSource::Legacy)),
            "{r}"
        );
        assert!(c.warnings.is_empty(), "{r}");
    }

    // A remote that does NOT map to the project (a fork) stays verbatim.
    let c = legacy_spec("refs/remotes/fork/main", false, None, &mapped);
    assert_eq!(
        c.effective,
        EffectiveBase::Verbatim("refs/remotes/fork/main".into())
    );

    // The SPA's bare "main" on a PR row → track(main) + base-upgraded.
    let c = legacy_spec("main", true, Some("main"), &mapped);
    assert!(c.upgraded);
    assert_eq!(codes(&c.warnings), vec![warn::BASE_UPGRADED]);
    let c = legacy_spec("main", true, None, &mapped);
    assert!(c.upgraded);
    // …but not when the PR's recorded target disagrees.
    let c = legacy_spec("main", true, Some("develop"), &mapped);
    assert_eq!(c.effective, EffectiveBase::Verbatim("main".into()));
    // A bare name on a NON-PR row is evaluated verbatim, as before.
    let c = legacy_spec("main", false, None, &mapped);
    assert_eq!(c.effective, EffectiveBase::Verbatim("main".into()));
    // Expressions / tags stay verbatim.
    for r in ["HEAD~3", "refs/tags/v1", "refs/heads/main"] {
        assert_eq!(
            legacy_spec(r, true, None, &mapped).effective,
            EffectiveBase::Verbatim(r.into()),
            "{r}"
        );
    }
}

#[test]
fn effective_base_prefers_the_policy_columns() {
    let c = effective_base(
        "refs/remotes/origin/main",
        Some("track"),
        Some("release/2.1"),
        None,
        "user",
        Some("explicit"),
        true,
        None,
        &[],
    );
    assert_eq!(
        c.effective,
        EffectiveBase::Policy(BasePolicy::track(
            "release/2.1",
            SetBy::User,
            BaseSource::Explicit
        ))
    );
    // No columns → legacy classification.
    let c = effective_base(SHA_A, None, None, None, "legacy", None, false, None, &[]);
    assert_eq!(c.effective.policy().unwrap().mode, BaseMode::Pin);
}

// =====================================================================
// pure: resolution chain + default branch + kind
// =====================================================================

fn no_default() -> Result<(String, Vec<BaseWarningOut>), BaseError> {
    Err(BaseError::undetermined("none"))
}

#[test]
fn pr_chain_rungs_in_order() {
    let explicit = classify_base(Some("track:develop"), true, &probe()).unwrap();
    let chain = PrChain {
        explicit: Some(explicit),
        forge_api: Some("main".into()),
        caller: Some("other".into()),
        head_branch: Some("feature".into()),
    };
    let (p, _) = resolve_pr_base(&chain, no_default).unwrap();
    assert_eq!(
        (p.branch.as_deref(), p.set_by),
        (Some("develop"), SetBy::User)
    );

    // Forge API beats the caller; kb may follow it (auto).
    let chain = PrChain {
        explicit: None,
        forge_api: Some("main".into()),
        caller: Some("other".into()),
        head_branch: Some("feature".into()),
    };
    let (p, w) = resolve_pr_base(&chain, no_default).unwrap();
    assert_eq!(
        p,
        BasePolicy::track("main", SetBy::Auto, BaseSource::ForgeApi)
    );
    assert!(w.is_empty());

    // Caller when the API is silent.
    let chain = PrChain {
        forge_api: None,
        ..chain
    };
    let (p, _) = resolve_pr_base(&chain, no_default).unwrap();
    assert_eq!(
        p,
        BasePolicy::track("other", SetBy::Auto, BaseSource::Caller)
    );

    // Default + loud pr-target-assumed when nothing else answers.
    let chain = PrChain {
        caller: None,
        ..chain
    };
    let (p, w) = resolve_pr_base(&chain, || Ok(("main".into(), vec![]))).unwrap();
    assert_eq!(
        p,
        BasePolicy::track("main", SetBy::Auto, BaseSource::DefaultAssumed)
    );
    assert_eq!(codes(&w), vec![warn::PR_TARGET_ASSUMED]);

    // The head's own branch is never a base (API answering garbage).
    let chain = PrChain {
        explicit: None,
        forge_api: Some("feature".into()),
        caller: Some("feature".into()),
        head_branch: Some("feature".into()),
    };
    let (p, w) = resolve_pr_base(&chain, || Ok(("main".into(), vec![]))).unwrap();
    assert_eq!(p.branch.as_deref(), Some("main"));
    assert_eq!(codes(&w), vec![warn::PR_TARGET_ASSUMED]);
    // …and nothing to fall back to → refuse.
    assert!(resolve_pr_base(&chain, no_default).is_err());
}

#[test]
fn an_old_spa_bare_target_equal_to_the_api_is_the_forge_rung() {
    let explicit = classify_base(Some("main"), true, &probe()).unwrap();
    assert!(explicit.from_bare);
    let chain = PrChain {
        explicit: Some(explicit.clone()),
        forge_api: Some("main".into()),
        ..PrChain::default()
    };
    let (p, _) = resolve_pr_base(&chain, no_default).unwrap();
    assert_eq!(
        p,
        BasePolicy::track("main", SetBy::Auto, BaseSource::ForgeApi)
    );
    // Disagreeing with the API: the user's word stands.
    let chain = PrChain {
        explicit: Some(explicit),
        forge_api: Some("develop".into()),
        ..PrChain::default()
    };
    let (p, _) = resolve_pr_base(&chain, no_default).unwrap();
    assert_eq!((p.branch.as_deref(), p.set_by), (Some("main"), SetBy::User));
}

#[test]
fn non_pr_chain_rungs_in_order_and_d14() {
    let base = NonPrChain {
        explicit: None,
        stack_parent: Some("parent".into()),
        upstream: Some(("dev".into(), true)),
        head_branch: Some("feature".into()),
        has_forge: true,
    };
    let (p, _) = resolve_non_pr_base(&base, no_default).unwrap();
    assert_eq!(
        p,
        BasePolicy::local("parent", SetBy::Auto, BaseSource::StackParent)
    );
    let chain = NonPrChain {
        stack_parent: None,
        ..base.clone()
    };
    let (p, _) = resolve_non_pr_base(&chain, no_default).unwrap();
    assert_eq!(
        p,
        BasePolicy::track("dev", SetBy::Auto, BaseSource::Upstream)
    );
    // An upstream outside the project → local.
    let chain = NonPrChain {
        stack_parent: None,
        upstream: Some(("dev".into(), false)),
        ..base.clone()
    };
    let (p, _) = resolve_non_pr_base(&chain, no_default).unwrap();
    assert_eq!(p.mode, BaseMode::Local);
    // The head's own mirror (`feature` ↔ `origin/feature`) is skipped, and
    // D14: the default is TRACKED, never the member's local main.
    let chain = NonPrChain {
        stack_parent: None,
        upstream: Some(("feature".into(), true)),
        ..base.clone()
    };
    let (p, _) = resolve_non_pr_base(&chain, || Ok(("main".into(), vec![]))).unwrap();
    assert_eq!(
        p,
        BasePolicy::track("main", SetBy::Auto, BaseSource::DefaultAssumed)
    );
    // No forge at all → the default is followed locally.
    let chain = NonPrChain {
        stack_parent: None,
        upstream: None,
        has_forge: false,
        ..base
    };
    let (p, _) = resolve_non_pr_base(&chain, || Ok(("main".into(), vec![]))).unwrap();
    assert_eq!(p.mode, BaseMode::Local);
}

#[test]
fn default_branch_ladder() {
    let c = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        pick_default_branch(Some("trunk"), Some("main"), &c(&["main"]), None)
            .unwrap()
            .0,
        "trunk"
    );
    assert_eq!(
        pick_default_branch(None, Some("develop"), &c(&["main"]), None)
            .unwrap()
            .0,
        "develop"
    );
    let (b, w) = pick_default_branch(None, None, &c(&["master", "feature"]), None).unwrap();
    assert_eq!(b, "master");
    assert_eq!(codes(&w), vec![warn::DEFAULT_BRANCH_GUESSED]);
    // Two candidates → refuse; none → refuse.
    assert!(pick_default_branch(None, None, &c(&["main", "master"]), None).is_err());
    assert!(pick_default_branch(None, None, &c(&["feature"]), None).is_err());
    // The head's own branch is never the default.
    assert!(pick_default_branch(None, Some("main"), &c(&["main"]), Some("main")).is_err());
}

#[test]
fn kind_decisions() {
    use PatchsetKind::*;
    let (t1, t2, m1, m2) = ("t1", "t2", "m1", "m2");
    assert_eq!(decide_kind(None, t1, m1, None, false), Some(Initial));
    // Base advancing without a head change: same pair → nothing (D13).
    assert_eq!(decide_kind(Some((t1, m1)), t1, m1, None, false), None);
    assert_eq!(
        decide_kind(Some((t1, m1)), t1, m1, Some(Retarget), false),
        None
    );
    assert_eq!(
        decide_kind(Some((t1, m1)), t1, m1, None, true),
        Some(Forced)
    );
    assert_eq!(decide_kind(Some((t1, m1)), t2, m1, None, false), Some(Push));
    assert_eq!(
        decide_kind(Some((t1, m1)), t2, m2, None, false),
        Some(Rebase)
    );
    assert_eq!(
        decide_kind(Some((t1, m1)), t1, m2, None, false),
        Some(BaseMoved)
    );
    assert_eq!(
        decide_kind(Some((t1, m1)), t2, m2, Some(BaseCorrected), false),
        Some(BaseCorrected)
    );
}

#[test]
fn base_out_labels_legacy_pinned_and_unfetched_bases() {
    let st = BaseStatus::default();
    let v = base_out(
        &EffectiveBase::Verbatim("HEAD~1".into()),
        "legacy",
        &st,
        None,
    );
    assert_eq!((v.mode, v.state.as_deref()), (None, Some("legacy")));
    let pin = legacy_spec(SHA_A, false, None, &[]).effective;
    let v = base_out(&pin, "legacy", &st, Some(SHA_A));
    assert_eq!(v.state.as_deref(), Some("pinned"));
    assert_eq!(v.set_by, "legacy");
    let t = EffectiveBase::Policy(BasePolicy::track("main", SetBy::Auto, BaseSource::ForgeApi));
    let v = base_out(&t, "auto", &st, None);
    assert_eq!(v.state.as_deref(), Some("unverified"));
    assert_eq!(v.source.as_deref(), Some("forge-api"));
}

// =====================================================================
// store fixtures
// =====================================================================

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn commit(dir: &Path, file: &str, body: &str) -> String {
    std::fs::write(dir.join(file), body).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", file]);
    git(dir, &["rev-parse", "HEAD"])
}

/// A bare forge (acme/widgets stand-in) + a member clone whose `origin` is
/// the forge's local path, and a temp DB/state dir.
struct Fx {
    _tmp: tempfile::TempDir,
    forge: PathBuf,
    clone: PathBuf,
    /// A scratch clone used to author forge-side history (main moving, PR
    /// pushes) WITHOUT touching the member clone.
    author: PathBuf,
    store: Store,
    rs: ReviewStores,
    bus: EventBus,
    m1: String,
}

const REPO: &str = "widgets";
const PR: u32 = 7;

fn fixture() -> Fx {
    fixture_with(false)
}

/// `no_remote`: the member clone has NO remote at all (a `local:` store
/// with no forge to fetch a base from).
fn fixture_with(no_remote: bool) -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let forge = root.join("forge/widgets.git");
    std::fs::create_dir_all(&forge).unwrap();
    git(&forge, &["init", "-q", "--bare", "-b", "main"]);
    let author = root.join("author");
    std::fs::create_dir_all(&author).unwrap();
    git(&author, &["init", "-q", "-b", "main"]);
    git(
        &author,
        &["remote", "add", "origin", forge.to_str().unwrap()],
    );
    commit(&author, "m0.txt", "m0");
    let m1 = commit(&author, "m1.txt", "m1");
    git(&author, &["push", "-q", "origin", "main"]);
    // The member clone (its local `main` stays where it was cloned).
    let clone = root.join("work/widgets");
    git(
        &root,
        &[
            "clone",
            "-q",
            forge.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    if no_remote {
        git(&clone, &["remote", "remove", "origin"]);
    }
    let home = root.join("state");
    std::fs::create_dir_all(&home).unwrap();
    let store = Store::open(&home.join("index.db")).unwrap();
    let repos = vec![RepoEntry {
        name: REPO.into(),
        path: clone.clone(),
    }];
    let mut ids = HashMap::new();
    ids.insert(
        REPO.to_string(),
        store.upsert_repo(REPO, &clone.to_string_lossy()).unwrap(),
    );
    let rs = ReviewStores::new(&ReviewSection::default(), &home, &repos, &ids);
    let fx = Fx {
        _tmp: tmp,
        forge,
        clone,
        author,
        store,
        rs,
        bus: EventBus::default(),
        m1,
    };
    let id = match fx.rs.register_repo(&fx.store, REPO, None) {
        Registration::Member { store_id, .. } => store_id,
        other => panic!("registration: {other:?}"),
    };
    fx.rs.seed(&fx.store, id, false).expect("seed");
    fx
}

impl Fx {
    fn ctx(&self) -> (crate::review_store::StoreHandle, Member) {
        let h = self
            .rs
            .handle_for_repo(&self.store, REPO)
            .expect("the store is ready");
        let r = self.rs.repo(REPO).unwrap();
        (
            h,
            Member {
                id: r.id,
                name: r.name.clone(),
                root: r.root.clone(),
            },
        )
    }

    fn with<T>(&self, f: impl FnOnce(&StoreCtx<'_>) -> T) -> T {
        let (h, m) = self.ctx();
        let ctx = StoreCtx {
            rs: &self.rs,
            store: &self.store,
            bus: &self.bus,
            handle: &h,
            member: m,
            max_patchsets: 50,
        };
        f(&ctx)
    }

    fn store_dir(&self) -> PathBuf {
        self.ctx().0.git_dir
    }

    /// Author a PR branch `pr` off `from` with `files`, push it as the
    /// forge's `refs/pull/<PR>/head` (forced) and return its tip.
    fn push_pr(&self, from: &str, files: &[&str], tag: &str) -> String {
        git(&self.author, &["checkout", "-q", "-B", "pr", from]);
        let mut tip = String::new();
        for f in files {
            tip = commit(&self.author, f, &format!("{f} {tag}"));
        }
        git(
            &self.author,
            &[
                "push",
                "-q",
                "-f",
                "origin",
                &format!("HEAD:refs/pull/{PR}/head"),
            ],
        );
        git(&self.author, &["checkout", "-q", "main"]);
        tip
    }

    /// Advance the forge's `main` by `n` commits (never the member's).
    fn advance_main(&self, n: usize, tag: &str) -> String {
        git(&self.author, &["checkout", "-q", "main"]);
        let mut tip = String::new();
        for i in 0..n {
            tip = commit(&self.author, &format!("main-{tag}-{i}.txt"), tag);
        }
        git(&self.author, &["push", "-q", "origin", "main"]);
        tip
    }

    /// A PR review row whose ps1 is not yet captured.
    fn pr_review(&self, base_ref: &str, policy: Option<&BasePolicy>) -> ReviewRow {
        let id = self
            .store
            .create_review(
                REPO,
                Some("t"),
                base_ref,
                &crate::reviews::pr_ref(PR),
                None,
                1,
            )
            .unwrap();
        self.store
            .set_review_pr_binding(id, PR as i64, "acme/widgets", None, None, None)
            .unwrap();
        if let Some(p) = policy {
            self.store
                .set_review_base(
                    id,
                    p.mode.as_str(),
                    p.branch.as_deref(),
                    p.member,
                    p.set_by.as_str(),
                    None,
                )
                .unwrap();
        }
        self.store.get_review(id).unwrap().unwrap()
    }

    fn refetch(&self, id: i64) -> ReviewRow {
        self.store.get_review(id).unwrap().unwrap()
    }

    fn clone_refs(&self) -> String {
        git(
            &self.clone,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        )
    }

    fn diff_names(&self, base: &str, tip: &str) -> Vec<String> {
        let out = git(
            &self.store_dir(),
            &["diff", "--name-only", &format!("{base}..{tip}")],
        );
        let mut v: Vec<String> = out.lines().map(str::to_string).collect();
        v.sort();
        v
    }

    fn commit_count(&self, base: &str, tip: &str) -> String {
        git(
            &self.store_dir(),
            &["rev-list", "--count", &format!("{base}..{tip}")],
        )
    }
}

fn recap(fx: &Fx, id: i64, rc: Recapture) -> capture::Recaptured {
    let review = fx.refetch(id);
    fx.with(|ctx| ctx.recapture(&review, &rc))
        .unwrap_or_else(|e| panic!("recapture: {e}"))
}

fn fetch() -> Recapture {
    Recapture {
        network: true,
        ..Recapture::default()
    }
}

#[test]
fn a_local_path_origin_seeds_a_ready_local_store() {
    let fx = fixture();
    let (h, _) = fx.ctx();
    assert!(crate::review_store::key::is_local_key(&h.store_key));
    assert_eq!(
        fx.with(|c| c.forge()),
        capture::Forge::Local(std::fs::canonicalize(&fx.forge).unwrap())
    );
    assert_eq!(fx.with(|c| c.mapped_remotes()), vec!["origin".to_string()]);
}

/// THE review-65 shape (README §1): a PR of N commits forked from main at
/// M1; main advances by K; the PR is REBASED onto the new main; a review
/// pinned (legacy) at M1 diffs 40-commits-style. Correcting it to
/// `track(main)` gives exactly the PR's own N commits and files
/// (GitHub-equal), `kind=base-corrected`; a re-sync with no upstream change
/// mints nothing.
#[test]
fn review_65_shape_base_correction_is_github_equal() {
    let fx = fixture();
    let pr_files = ["p1.rs", "p2.rs", "p3.rs"];
    let tip0 = fx.push_pr(&fx.m1, &pr_files, "v1");
    // A legacy review pinned at M1 (the morning skill's `--base <sha>`).
    let review = fx.pr_review(&fx.m1, None);
    let before = fx.clone_refs();
    let r1 = recap(&fx, review.id, fetch());
    assert!(r1.outcome.minted);
    // A capture that did not change the policy writes ONLY base_status —
    // the legacy row keeps its NULL policy columns (never rewritten).
    let b = fx.store.get_review_base(review.id).unwrap().unwrap();
    assert_eq!(
        (b.base_mode.as_deref(), b.base_set_by.as_str()),
        (None, "legacy")
    );
    assert!(b.base_status.is_some());
    assert_eq!(r1.outcome.ps.tip_sha, tip0);
    assert_eq!(r1.outcome.ps.base_sha, fx.m1);
    assert_eq!(r1.effective.policy().unwrap().set_by, SetBy::Legacy);
    assert!(codes(&r1.warnings).contains(&warn::BASE_PINNED));

    // main advances by K=4; the PR is rebased onto the new main tip.
    let main_tip = fx.advance_main(4, "k");
    let tip1 = fx.push_pr(&main_tip, &pr_files, "v2");
    // Still pinned: the pin drags main's K commits into the diff.
    let r2 = recap(&fx, review.id, fetch());
    assert_eq!(r2.outcome.ps.tip_sha, tip1);
    assert_eq!(r2.outcome.ps.base_sha, fx.m1);
    assert_eq!(fx.commit_count(&fx.m1, &tip1), "7");

    // Correct the base: track(main), user — the retrack shape.
    let corrected = BasePolicy::track("main", SetBy::User, BaseSource::Explicit);
    let r3 = recap(
        &fx,
        review.id,
        Recapture {
            network: true,
            policy_override: Some(corrected),
            kind_hint: Some(PatchsetKind::BaseCorrected),
            ..Recapture::default()
        },
    );
    assert!(r3.outcome.minted);
    assert_eq!(r3.outcome.kind.as_deref(), Some("base-corrected"));
    assert_eq!(r3.outcome.ps.tip_sha, tip1);
    assert_eq!(
        r3.outcome.ps.base_sha, main_tip,
        "merge-base = the new main"
    );
    assert_eq!(r3.outcome.base_tip_sha.as_deref(), Some(main_tip.as_str()));
    // GitHub-equal: exactly the PR's own 3 commits and 3 files.
    assert_eq!(fx.commit_count(&r3.outcome.ps.base_sha, &tip1), "3");
    assert_eq!(
        fx.diff_names(&r3.outcome.ps.base_sha, &tip1),
        vec!["p1.rs", "p2.rs", "p3.rs"]
    );
    // The policy was persisted with the capture.
    let b = fx.store.get_review_base(review.id).unwrap().unwrap();
    assert_eq!(
        (
            b.base_mode.as_deref(),
            b.base_branch.as_deref(),
            b.base_set_by.as_str()
        ),
        (Some("track"), Some("main"), "user")
    );
    assert_eq!(fx.refetch(review.id).base_ref, "refs/remotes/origin/main");
    let pb = fx
        .store
        .get_patchset_base(review.id, r3.outcome.ps.ps_number)
        .unwrap()
        .unwrap();
    assert_eq!(pb.kind.as_deref(), Some("base-corrected"));
    // ps<n> and ps<n>-base refs live in the STORE.
    let store_refs = git(&fx.store_dir(), &["for-each-ref", "--format=%(refname)"]);
    let n = r3.outcome.ps.ps_number;
    for want in [
        format!("refs/kbc/review/{}/ps{n}", review.id),
        format!("refs/kbc/review/{}/ps{n}-base", review.id),
        format!("refs/kbc/pr/{PR}"),
    ] {
        assert!(
            store_refs.lines().any(|l| l == want),
            "{want} missing: {store_refs}"
        );
    }

    // An idempotent re-sync (nothing moved upstream) mints nothing.
    let r4 = recap(&fx, review.id, fetch());
    assert!(!r4.outcome.minted);
    assert_eq!(r4.outcome.ps.ps_number, n);
    // pr_head_sha is synced on every fetch.
    let binding = fx.store.get_review_pr_binding(review.id).unwrap().unwrap();
    assert_eq!(binding.pr_head_sha.as_deref(), Some(tip1.as_str()));
    // The user clone was never written.
    assert_eq!(fx.clone_refs(), before);
}

#[test]
fn kinds_push_rebase_base_moved_and_retarget_follow() {
    let fx = fixture();
    let tip0 = fx.push_pr(&fx.m1, &["a.rs"], "v1");
    // Created through the chain: the forge API says the PR targets main.
    let nr = NewReview {
        pr: Some(PR),
        forge_base_ref: Some("main".into()),
        ..NewReview::default()
    };
    let prepared = fx.with(|c| c.prepare_new(&nr)).unwrap();
    assert_eq!(
        prepared.policy,
        BasePolicy::track("main", SetBy::Auto, BaseSource::ForgeApi)
    );
    assert_eq!(prepared.head_sha, tip0);
    let review = fx.pr_review(&prepared.base_ref, Some(&prepared.policy));
    let ps1 = fx
        .with(|c| {
            c.capture(
                &review,
                &EffectiveBase::Policy(prepared.policy.clone()),
                &CaptureOpts {
                    force: true,
                    kind_hint: None,
                },
            )
        })
        .unwrap();
    assert_eq!(ps1.kind.as_deref(), Some("initial"));

    // main advances, head unchanged → nothing minted.
    let main2 = fx.advance_main(2, "a");
    let r = recap(&fx, review.id, fetch());
    assert!(
        !r.outcome.minted,
        "a base branch merely advancing never mints"
    );
    assert_eq!(r.status.state.as_deref(), Some("ok"));

    // head push → push.
    git(&fx.author, &["checkout", "-q", "pr"]);
    let tip1 = commit(&fx.author, "b.rs", "b");
    git(
        &fx.author,
        &[
            "push",
            "-q",
            "-f",
            "origin",
            &format!("HEAD:refs/pull/{PR}/head"),
        ],
    );
    git(&fx.author, &["checkout", "-q", "main"]);
    let r = recap(&fx, review.id, fetch());
    assert_eq!(
        (r.outcome.minted, r.outcome.kind.as_deref()),
        (true, Some("push"))
    );
    assert_eq!(r.outcome.ps.tip_sha, tip1);

    // rebase onto the new main → rebase.
    let tip2 = fx.push_pr(&main2, &["a.rs", "b.rs"], "v3");
    let r = recap(&fx, review.id, fetch());
    assert_eq!(
        (r.outcome.minted, r.outcome.kind.as_deref()),
        (true, Some("rebase"))
    );
    assert_eq!(r.outcome.ps.base_sha, main2);
    assert_eq!(r.outcome.ps.tip_sha, tip2);

    // A branch that forked earlier; the API now says the PR targets it
    // (set_by=auto) → followed, kind=retarget, persisted.
    git(
        &fx.author,
        &[
            "push",
            "-q",
            "origin",
            &format!("{}:refs/heads/release", fx.m1),
        ],
    );
    let r = recap(
        &fx,
        review.id,
        Recapture {
            network: true,
            forge_base_ref: Some("release".into()),
            ..Recapture::default()
        },
    );
    assert!(r.retargeted);
    assert_eq!(
        (r.outcome.minted, r.outcome.kind.as_deref()),
        (true, Some("retarget"))
    );
    assert_eq!(r.outcome.ps.base_sha, fx.m1);
    assert!(codes(&r.warnings).contains(&warn::RETARGETED));
    let b = fx.store.get_review_base(review.id).unwrap().unwrap();
    assert_eq!(
        (b.base_branch.as_deref(), b.base_set_by.as_str()),
        (Some("release"), "auto")
    );

    // snapshot --force on an identical pair keeps the old always-mint.
    let r = recap(
        &fx,
        review.id,
        Recapture {
            network: true,
            forge_base_ref: Some("release".into()),
            force: true,
            ..Recapture::default()
        },
    );
    assert!(r.outcome.minted);
    let r = recap(&fx, review.id, fetch());
    assert!(!r.outcome.minted);
}

#[test]
fn a_user_set_base_is_never_retargeted() {
    let fx = fixture();
    fx.push_pr(&fx.m1, &["a.rs"], "v1");
    git(
        &fx.author,
        &[
            "push",
            "-q",
            "origin",
            &format!("{}:refs/heads/release", fx.m1),
        ],
    );
    let user = BasePolicy::track("main", SetBy::User, BaseSource::Explicit);
    let review = fx.pr_review("refs/remotes/origin/main", Some(&user));
    let r = recap(
        &fx,
        review.id,
        Recapture {
            network: true,
            forge_base_ref: Some("release".into()),
            ..Recapture::default()
        },
    );
    assert!(!r.retargeted);
    assert_eq!(r.outcome.kind.as_deref(), Some("initial"));
    assert!(codes(&r.warnings).contains(&warn::PR_TARGET_DIFFERS));
    let b = fx.store.get_review_base(review.id).unwrap().unwrap();
    assert_eq!(
        (b.base_branch.as_deref(), b.base_set_by.as_str()),
        (Some("main"), "user")
    );
}

#[test]
fn pr_target_assumed_when_nothing_names_the_target() {
    let fx = fixture();
    fx.push_pr(&fx.m1, &["a.rs"], "v1");
    let prepared = fx
        .with(|c| {
            c.prepare_new(&NewReview {
                pr: Some(PR),
                ..NewReview::default()
            })
        })
        .unwrap();
    // The default branch came from the forge's HEAD symref.
    assert_eq!(
        prepared.policy,
        BasePolicy::track("main", SetBy::Auto, BaseSource::DefaultAssumed)
    );
    assert!(codes(&prepared.warnings).contains(&warn::PR_TARGET_ASSUMED));
    assert_eq!(prepared.base_ref, "refs/remotes/origin/main");
    // The caller rung beats the assumption.
    let prepared = fx
        .with(|c| {
            c.prepare_new(&NewReview {
                pr: Some(PR),
                caller_base_ref: Some("main".into()),
                ..NewReview::default()
            })
        })
        .unwrap();
    assert_eq!(prepared.policy.source, BaseSource::Caller);
    assert!(prepared.warnings.is_empty(), "{:?}", prepared.warnings);
}

/// D14 — `review start` without `--base` tracks the FORGE's default
/// branch, never the member's stale local `main`.
#[test]
fn review_start_without_base_never_uses_the_stale_local_main() {
    let fx = fixture();
    let stale_local_main = git(&fx.clone, &["rev-parse", "main"]);
    let forge_main = fx.advance_main(3, "d14");
    git(&fx.clone, &["checkout", "-q", "-b", "feature"]);
    commit(&fx.clone, "f.rs", "f");
    let before = fx.clone_refs();
    let prepared = fx
        .with(|c| {
            c.prepare_new(&NewReview {
                head_ref: "feature".into(),
                ..NewReview::default()
            })
        })
        .unwrap();
    assert_eq!(prepared.policy.mode, BaseMode::Track);
    assert_eq!(prepared.policy.branch.as_deref(), Some("main"));
    assert_eq!(prepared.base_tip, forge_main);
    assert_ne!(prepared.base_tip, stale_local_main);
    assert_eq!(fx.clone_refs(), before, "the user clone was never written");
}

#[test]
fn explicit_bases_and_refusals_in_the_store() {
    let fx = fixture();
    fx.push_pr(&fx.m1, &["a.rs"], "v1");
    // A 40-hex base pins with a warning.
    let prepared = fx
        .with(|c| {
            c.prepare_new(&NewReview {
                pr: Some(PR),
                base_input: Some(fx.m1.clone()),
                ..NewReview::default()
            })
        })
        .unwrap();
    assert_eq!(prepared.policy.mode, BaseMode::Pin);
    assert!(codes(&prepared.warnings).contains(&warn::BASE_PINNED));
    // The store-backed grammar (retrack's entry point): `origin` maps to
    // the project, so `origin/main` tracks main; a bare local branch on a
    // non-PR review stays local.
    let c = fx.with(|c| c.classify(Some("origin/main"), false)).unwrap();
    assert_eq!(
        c.policy,
        Some(BasePolicy::track("main", SetBy::User, BaseSource::Explicit))
    );
    let c = fx.with(|c| c.classify(Some("main"), false)).unwrap();
    assert_eq!(c.policy.map(|p| p.mode), Some(BaseMode::Local));
    // A bare non-PR branch that exists only on the forge (never fetched
    // here): fetch-then-track, never a 400.
    git(
        &fx.author,
        &[
            "push",
            "-q",
            "origin",
            &format!("{}:refs/heads/develop", fx.m1),
        ],
    );
    git(&fx.clone, &["checkout", "-q", "-b", "topic"]);
    commit(&fx.clone, "t.rs", "t");
    let prepared = fx
        .with(|c| {
            c.prepare_new(&NewReview {
                head_ref: "topic".into(),
                base_input: Some("develop".into()),
                ..NewReview::default()
            })
        })
        .unwrap();
    assert_eq!(
        prepared.policy,
        BasePolicy::track("develop", SetBy::User, BaseSource::Explicit)
    );
    assert_eq!(prepared.base_tip, fx.m1);
    // A branch the forge does not have is a 400, before any row exists.
    let err = fx
        .with(|c| {
            c.prepare_new(&NewReview {
                pr: Some(PR),
                base_input: Some("track:no-such-branch".into()),
                ..NewReview::default()
            })
        })
        .unwrap_err();
    assert_eq!(err.urn, URN_BASE_UNRESOLVED, "{err}");
    // An unknown PR number is a PR fetch failure.
    let err = fx
        .with(|c| {
            c.prepare_new(&NewReview {
                pr: Some(999),
                forge_base_ref: Some("main".into()),
                ..NewReview::default()
            })
        })
        .unwrap_err();
    assert_eq!(err.urn, URN_PR_FETCH_FAILED, "{err}");
}

/// Auto-capture (`network: false`) never fetches: a forge-side PR push is
/// invisible to it, while a member-branch move is captured.
#[test]
fn auto_capture_never_fetches_the_forge() {
    let fx = fixture();
    git(&fx.clone, &["checkout", "-q", "-b", "feature"]);
    commit(&fx.clone, "f.rs", "f");
    let prepared = fx
        .with(|c| {
            c.prepare_new(&NewReview {
                head_ref: "feature".into(),
                base_input: Some("main".into()),
                ..NewReview::default()
            })
        })
        .unwrap();
    // A bare `main` on a non-PR review is the member's local main.
    assert_eq!(prepared.policy.mode, BaseMode::Local);
    let id = fx
        .store
        .create_review(REPO, None, &prepared.base_ref, "feature", None, 1)
        .unwrap();
    let review = fx.refetch(id);
    fx.with(|c| {
        c.capture(
            &review,
            &EffectiveBase::Policy(prepared.policy.clone()),
            &CaptureOpts {
                force: true,
                kind_hint: None,
            },
        )
    })
    .unwrap();
    fx.store
        .set_review_base(id, "local", Some("main"), None, "user", None)
        .unwrap();
    // Populate the store's `base/main` once, then move the forge.
    let rep = fx.with(|c| {
        let a = c.access();
        c.fetch_forge(
            a.as_ref().map_err(String::as_str),
            &["main".to_string()],
            None,
        )
    });
    assert_eq!(rep.state, "fetched", "{rep:?}");
    let forge_main_before = fx.with(|c| c.store_sha("refs/remotes/base/main"));
    assert!(forge_main_before.is_some());
    fx.advance_main(1, "auto");
    let tip = commit(&fx.clone, "g.rs", "g");
    let r = recap(&fx, id, Recapture::default());
    assert_eq!(
        (r.outcome.minted, r.outcome.kind.as_deref()),
        (true, Some("push"))
    );
    assert_eq!(r.outcome.ps.tip_sha, tip);
    assert_eq!(r.fetch.state, "cached");
    assert_eq!(
        fx.with(|c| c.store_sha("refs/remotes/base/main")),
        forge_main_before,
        "auto-capture never fetched the forge"
    );
}

// =====================================================================
// route glue: start-pr / snapshot / show against a ready store
// =====================================================================

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// The HTTP-facing flows on a READY store: `start-pr` (no forge API → the
/// default branch, loudly assumed), an unchanged `snapshot` (minted:false),
/// a PR push + `snapshot` (kind push), `GET /api/reviews/{id}` carrying
/// `base{}`/`warnings[]`/per-patchset `kind` — and the member clone's refs
/// are byte-identical before and after (user-clone invariance).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_pr_and_snapshot_routes_capture_in_the_store_not_the_clone() {
    use axum::extract::{Path as AxumPath, State};
    use axum::response::IntoResponse;
    let fx = tokio::task::spawn_blocking(fixture).await.unwrap();
    let tip0 = fx.push_pr(&fx.m1, &["a.rs"], "v1");
    let cfg = crate::config::KbCodeConfig {
        repos: vec![RepoEntry {
            name: REPO.into(),
            path: fx.clone.clone(),
        }],
        kb_daemon: crate::config::KbDaemonSection {
            enabled: false,
            url: Some("http://127.0.0.1:0".to_string()),
            token_file: None,
            public_url: None,
        },
        transcripts: crate::config::TranscriptsSection {
            enabled: false,
            ..crate::config::TranscriptsSection::default()
        },
        ..crate::config::KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = kb_core::paths::KbPaths::rooted_at(tmp.path(), "kb-code");
    let state = crate::build_state_for_test(cfg, paths).await.unwrap();
    let st = state.clone();
    tokio::task::spawn_blocking(move || {
        let id = match st.review_stores.register_repo(&st.store, REPO, None) {
            Registration::Member { store_id, .. } => store_id,
            other => panic!("registration: {other:?}"),
        };
        st.review_stores.seed(&st.store, id, false).expect("seed");
    })
    .await
    .unwrap();
    let before = fx.clone_refs();

    let body: crate::reviews::CreateReviewPrBody =
        serde_json::from_value(serde_json::json!({"repo": REPO, "pr_number": PR})).unwrap();
    let (status, created) = crate::reviews::create_review_pr_value(&state, body, None, None)
        .await
        .unwrap_or_else(|e| panic!("start-pr: {e:?}"));
    assert_eq!(status, axum::http::StatusCode::CREATED, "{created}");
    assert_eq!(created["tip_sha"], tip0.as_str(), "{created}");
    assert_eq!(created["base_sha"], fx.m1.as_str(), "{created}");
    assert_eq!(created["pr_head_sha"], tip0.as_str());
    assert_eq!(created["minted"], true);
    assert_eq!(created["kind"], "initial");
    assert_eq!(created["base_ref"], "refs/remotes/origin/main");
    assert_eq!(created["base_source"], "merge-base");
    assert_eq!(created["base"]["mode"], "track", "{created}");
    assert_eq!(created["base"]["branch"], "main");
    assert_eq!(created["base"]["source"], "default-assumed");
    assert_eq!(created["base"]["state"], "ok");
    assert_eq!(created["base"]["fetched_via"], "local");
    assert!(
        created["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["code"] == warn::PR_TARGET_ASSUMED),
        "{created}"
    );
    let id = created["id"].as_i64().unwrap();

    let snap = |force: bool| {
        let state = state.clone();
        async move {
            let raw = if force {
                axum::body::Bytes::from_static(br#"{"force": true}"#)
            } else {
                axum::body::Bytes::new()
            };
            let resp = crate::reviews::snapshot_review(State(state), AxumPath(id), raw)
                .await
                .unwrap_or_else(|e| panic!("snapshot: {e:?}"))
                .into_response();
            body_json(resp).await
        }
    };
    let s = snap(false).await;
    assert_eq!(
        (s["minted"].as_bool(), s["ps_number"].as_i64()),
        (Some(false), Some(1)),
        "{s}"
    );
    fx.advance_main(1, "r");
    let s = snap(false).await;
    assert_eq!(
        s["minted"], false,
        "a base branch merely advancing never mints: {s}"
    );
    git(&fx.author, &["checkout", "-q", "pr"]);
    let tip1 = commit(&fx.author, "b.rs", "b");
    git(
        &fx.author,
        &[
            "push",
            "-q",
            "-f",
            "origin",
            &format!("HEAD:refs/pull/{PR}/head"),
        ],
    );
    git(&fx.author, &["checkout", "-q", "main"]);
    let s = snap(false).await;
    assert_eq!(
        (s["minted"].as_bool(), s["kind"].as_str()),
        (Some(true), Some("push")),
        "{s}"
    );
    assert_eq!(s["tip_sha"], tip1.as_str());
    let s = snap(true).await;
    assert_eq!(s["minted"], true, "--force always mints: {s}");

    let resp = crate::reviews::get_review(State(state.clone()), AxumPath(id))
        .await
        .unwrap_or_else(|e| panic!("show: {e:?}"))
        .into_response();
    let show = body_json(resp).await;
    assert_eq!(show["base"]["mode"], "track", "{show}");
    let kinds: Vec<&str> = show["patchsets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["kind"].as_str().unwrap_or("?"))
        .collect();
    assert_eq!(kinds, vec!["initial", "push", "forced"], "{show}");
    assert_eq!(
        show["pr_head_sha"],
        tip1.as_str(),
        "pr_head_sha synced on fetch"
    );

    assert_eq!(fx.clone_refs(), before, "the user clone was never written");
}

// =====================================================================
// RS-U6 review fixes
// =====================================================================

/// Fix 1 (D14) — `branch review --base auto` on a ready store: the ladder's
/// answer (the member's STALE local `main`) is not frozen as a user
/// choice; the store's chain tracks the forge's `main`, `set_by=auto`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn branch_review_auto_runs_the_store_chain_not_a_frozen_local_main() {
    use axum::extract::State;
    use axum::response::IntoResponse;
    let fx = tokio::task::spawn_blocking(fixture).await.unwrap();
    let forge_main = fx.advance_main(2, "br");
    git(&fx.clone, &["checkout", "-q", "-b", "feature"]);
    commit(&fx.clone, "f.rs", "f");
    let state = route_state(&fx).await;
    let before = fx.clone_refs();
    let body: crate::branches::BranchReviewBody =
        serde_json::from_value(serde_json::json!({"repo": REPO, "ref": "feature"})).unwrap();
    let resp = crate::branches::start_branch_review(State(state.clone()), axum::Json(body))
        .await
        .unwrap_or_else(|e| panic!("branch review: {e:?}"))
        .into_response();
    let out = body_json(resp).await;
    let review = &out["review"];
    assert_eq!(review["base"]["mode"], "track", "{out}");
    assert_eq!(review["base"]["branch"], "main");
    assert_eq!(review["base"]["set_by"], "auto");
    assert_eq!(review["base"]["source"], "default-assumed");
    assert_eq!(review["base_tip_sha"], forge_main.as_str(), "{out}");
    assert_eq!(fx.clone_refs(), before);
}

/// Fix 1 — the stack-parent rung is `local(parent)`, auto, stack-parent.
#[test]
fn a_stack_parent_is_an_auto_local_base() {
    let fx = fixture();
    git(&fx.clone, &["checkout", "-q", "-b", "layer-a"]);
    commit(&fx.clone, "a.rs", "a");
    git(&fx.clone, &["checkout", "-q", "-b", "layer-b"]);
    let tip_b = commit(&fx.clone, "b.rs", "b");
    let a_tip = git(&fx.clone, &["rev-parse", "layer-a"]);
    let prepared = fx
        .with(|c| {
            c.prepare_new(&NewReview {
                head_ref: "layer-b".into(),
                stack_parent: Some("layer-a".into()),
                ..NewReview::default()
            })
        })
        .unwrap();
    assert_eq!(
        prepared.policy,
        BasePolicy::local("layer-a", SetBy::Auto, BaseSource::StackParent)
    );
    assert_eq!(
        (prepared.base_tip.as_str(), prepared.head_sha.as_str()),
        (a_tip.as_str(), tip_b.as_str())
    );
}

/// Fix 2 — capture never imports `refs/kbc/*` from the user clone: a
/// deleted review's leftover clone refs do not come back into the store
/// when ANOTHER review is captured.
#[test]
fn a_deleted_reviews_refs_stay_gone_when_another_review_captures() {
    let fx = fixture();
    git(&fx.clone, &["checkout", "-q", "-b", "feature"]);
    let tip = commit(&fx.clone, "f.rs", "f");
    let mk = |fx: &Fx| {
        let id = fx
            .store
            .create_review(REPO, None, "refs/heads/main", "feature", None, 1)
            .unwrap();
        fx.store
            .set_review_base(id, "local", Some("main"), None, "user", None)
            .unwrap();
        id
    };
    let a = mk(&fx);
    let r = recap(
        &fx,
        a,
        Recapture {
            force: true,
            ..Recapture::default()
        },
    );
    assert!(r.outcome.minted);
    // A legacy copy of A's patchset ref in the user clone.
    git(
        &fx.clone,
        &["update-ref", &crate::reviews::patchset_ref(a, 1), &tip],
    );
    let review_a = fx.refetch(a);
    crate::reviews::delete_review_with_refs(
        &fx.store,
        &fx.bus,
        &fx.with(|c| c.root()),
        &review_a,
        crate::reviews::PrRefScope::StoreWide,
    )
    .unwrap();
    let b = mk(&fx);
    recap(
        &fx,
        b,
        Recapture {
            force: true,
            ..Recapture::default()
        },
    );
    let store_refs = git(&fx.store_dir(), &["for-each-ref", "--format=%(refname)"]);
    assert!(
        !store_refs.contains(&format!("refs/kbc/review/{a}/")),
        "a deleted review's refs were re-imported: {store_refs}"
    );
    assert!(
        store_refs.contains(&format!("refs/kbc/review/{b}/ps1")),
        "{store_refs}"
    );
}

/// Fix 3 — heads that are not local branches are imported: a
/// remote-tracking branch (`origin/<b>`) and a detached sha.
#[test]
fn remote_tracking_and_detached_heads_are_imported() {
    let fx = fixture();
    git(&fx.author, &["checkout", "-q", "-B", "remote-only", &fx.m1]);
    let remote_tip = commit(&fx.author, "r.rs", "r");
    git(&fx.author, &["push", "-q", "origin", "remote-only"]);
    git(&fx.author, &["checkout", "-q", "main"]);
    git(&fx.clone, &["fetch", "-q", "origin"]);
    git(&fx.clone, &["checkout", "-q", "--detach", "main"]);
    let detached = commit(&fx.clone, "d.rs", "d");
    git(&fx.clone, &["checkout", "-q", "main"]);
    for (head, want) in [
        ("origin/remote-only", remote_tip.as_str()),
        (detached.as_str(), detached.as_str()),
    ] {
        let prepared = fx
            .with(|c| {
                c.prepare_new(&NewReview {
                    head_ref: head.into(),
                    base_input: Some("main".into()),
                    ..NewReview::default()
                })
            })
            .unwrap_or_else(|e| panic!("{head}: {e}"));
        assert_eq!(prepared.head_sha, want, "{head}");
        let id = fx
            .store
            .create_review(REPO, None, &prepared.base_ref, head, None, 1)
            .unwrap();
        let review = fx.refetch(id);
        let out = fx
            .with(|c| {
                c.capture(
                    &review,
                    &EffectiveBase::Policy(prepared.policy.clone()),
                    &CaptureOpts {
                        force: true,
                        kind_hint: None,
                    },
                )
            })
            .unwrap_or_else(|e| panic!("{head}: {e}"));
        assert_eq!(out.ps.tip_sha, want, "{head}");
    }
}

/// Fix 5 — a vanished tracked base: `set_by=auto` re-resolves (retarget),
/// a user base on an explicit fetch is a typed 409.
#[test]
fn a_vanished_base_is_reresolved_when_auto_and_refused_when_user_set() {
    let fx = fixture();
    fx.push_pr(&fx.m1, &["a.rs"], "v1");
    git(
        &fx.author,
        &[
            "push",
            "-q",
            "origin",
            &format!("{}:refs/heads/release", fx.m1),
        ],
    );
    let auto = BasePolicy::track("release", SetBy::Auto, BaseSource::ForgeApi);
    let ra = fx.pr_review("refs/remotes/origin/release", Some(&auto));
    // The user-set one is a non-PR review of a member branch.
    git(&fx.clone, &["checkout", "-q", "-b", "feature"]);
    commit(&fx.clone, "f.rs", "f");
    let id_u = fx
        .store
        .create_review(
            REPO,
            None,
            "refs/remotes/origin/release",
            "feature",
            None,
            1,
        )
        .unwrap();
    fx.store
        .set_review_base(id_u, "track", Some("release"), None, "user", None)
        .unwrap();
    let ru = fx.refetch(id_u);
    recap(&fx, ra.id, fetch());
    recap(&fx, ru.id, fetch());
    git(&fx.author, &["push", "-q", "origin", ":refs/heads/release"]);

    let r = recap(&fx, ra.id, fetch());
    assert!(r.retargeted);
    assert!(
        codes(&r.warnings).contains(&warn::BASE_VANISHED),
        "{:?}",
        r.warnings
    );
    assert_eq!(
        r.effective.policy().unwrap().branch.as_deref(),
        Some("main")
    );
    let b = fx.store.get_review_base(ra.id).unwrap().unwrap();
    assert_eq!(b.base_branch.as_deref(), Some("main"));

    let review_u = fx.refetch(ru.id);
    let err = fx.with(|c| c.recapture(&review_u, &fetch())).unwrap_err();
    assert_eq!(err.urn, URN_BASE_VANISHED, "{err}");
    let b = fx.store.get_review_base(ru.id).unwrap().unwrap();
    assert_eq!(
        (b.base_branch.as_deref(), b.base_set_by.as_str()),
        (Some("release"), "user"),
        "a user-set base is never changed"
    );
}

/// Fix 8 — with no forge, an ambiguous default branch REFUSES instead of
/// taking whatever is checked out.
#[test]
fn no_forge_default_branch_refuses_when_ambiguous() {
    let fx = fixture_with(true);
    assert_eq!(fx.with(|c| c.forge()), capture::Forge::None);
    git(&fx.clone, &["branch", "master", "main"]);
    git(&fx.clone, &["checkout", "-q", "-b", "feature"]);
    let err = fx
        .with(|c| c.default_branch(None, Some("feature")))
        .unwrap_err();
    assert_eq!(err.urn, URN_BASE_UNDETERMINED, "{err}");
    git(&fx.clone, &["branch", "-D", "master"]);
    let (b, w) = fx
        .with(|c| c.default_branch(None, Some("feature")))
        .unwrap();
    assert_eq!(b, "main");
    assert_eq!(codes(&w), vec![warn::DEFAULT_BRANCH_GUESSED]);
}

/// A daemon state over the fixture's clone, with its store seeded.
async fn route_state(fx: &Fx) -> crate::state::SharedState {
    route_state_with(fx, crate::config::GithubSection::default()).await
}

async fn route_state_with(
    fx: &Fx,
    github: crate::config::GithubSection,
) -> crate::state::SharedState {
    let cfg = crate::config::KbCodeConfig {
        repos: vec![RepoEntry {
            name: REPO.into(),
            path: fx.clone.clone(),
        }],
        kb_daemon: crate::config::KbDaemonSection {
            enabled: false,
            url: Some("http://127.0.0.1:0".to_string()),
            token_file: None,
            public_url: None,
        },
        transcripts: crate::config::TranscriptsSection {
            enabled: false,
            ..crate::config::TranscriptsSection::default()
        },
        github,
        ..crate::config::KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = kb_core::paths::KbPaths::rooted_at(tmp.path(), "kb-code");
    // Leak the tempdir for the test's lifetime (the state outlives this fn).
    std::mem::forget(tmp);
    let state = crate::build_state_for_test(cfg, paths).await.unwrap();
    let st = state.clone();
    tokio::task::spawn_blocking(move || {
        if let Registration::Member { store_id, .. } =
            st.review_stores.register_repo(&st.store, REPO, None)
        {
            let _ = st.review_stores.seed(&st.store, store_id, false);
        }
    })
    .await
    .unwrap();
    state
}

const TOKEN_ALICE: &str = "ghp_FAKEaliceAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

/// A fake `gh` answering `auth status`/`auth token` for one account.
fn fake_gh(dir: &Path, login: &str) -> crate::review_store::GhCli {
    let script = format!(
        r#"#!/bin/sh
if [ "$1 $2" = "auth status" ]; then
  echo '{{"hosts":{{"github.com":[{{"state":"success","active":true,"host":"github.com","login":"{login}","tokenSource":"keyring","scopes":"repo","gitProtocol":"https"}}]}}}}'
  exit 0
fi
if [ "$1 $2" = "auth token" ]; then echo "{TOKEN_ALICE}"; exit 0; fi
exit 2
"#
    );
    let p = dir.join("gh");
    std::fs::write(&p, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    // ETXTBSY guard: a sibling test's fork may briefly hold the write fd.
    for _ in 0..200 {
        match Command::new(&p).arg("--warmup").output() {
            Err(e) if e.raw_os_error() == Some(26) => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            _ => break,
        }
    }
    crate::review_store::GhCli::from_env_fn(p, |k| match k {
        "PATH" => Some("/usr/bin:/bin".into()),
        _ => None,
    })
}

/// Fixes 6 + 7 + deviation 3's follow-up — on the store path the forge API
/// is read for the STORE's project (never a fork `origin`), with the
/// daemon's gh-cli token (never visible in the client's Debug form), and a
/// gh account that is not the recorded one is a
/// `credential-account-mismatch` warning, never a silent fall-through.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forge_api_reads_the_store_project_with_the_gh_cli_token() {
    use axum::extract::Path as AxumPath;
    use axum::http::HeaderMap;
    use std::sync::{Arc, Mutex};
    let fx = tokio::task::spawn_blocking(fixture).await.unwrap();
    // origin = a personal FORK; the store's project is acme/widgets.
    git(
        &fx.clone,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/someone/widgets.git",
        ],
    );
    type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;
    let seen: Seen = Arc::default();
    let seen2 = seen.clone();
    let router = axum::Router::new().route(
        "/repos/{owner}/{name}/pulls/{n}",
        axum::routing::get(
            move |AxumPath((owner, name, _n)): AxumPath<(String, String, u64)>,
                  headers: HeaderMap| {
                let seen = seen2.clone();
                async move {
                    let auth = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    seen.lock().unwrap().push((format!("{owner}/{name}"), auth));
                    let base = if owner == "acme" {
                        "release"
                    } else {
                        "fork-main"
                    };
                    axum::Json(serde_json::json!({
                        "number": 7, "title": "t", "user": {"login": "someone"},
                        "head": {"ref": "feature", "sha": "deadbeef"},
                        "base": {"ref": base},
                        "updated_at": "2024-01-01T00:00:00Z", "draft": false,
                        "state": "open", "merged": false, "labels": []
                    }))
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let github = crate::config::GithubSection {
        token_file: None,
        api_base: format!("http://{addr}"),
    };
    let cfg = crate::config::KbCodeConfig {
        repos: vec![RepoEntry {
            name: REPO.into(),
            path: fx.clone.clone(),
        }],
        kb_daemon: crate::config::KbDaemonSection {
            enabled: false,
            url: Some("http://127.0.0.1:0".to_string()),
            token_file: None,
            public_url: None,
        },
        transcripts: crate::config::TranscriptsSection {
            enabled: false,
            ..crate::config::TranscriptsSection::default()
        },
        github,
        ..crate::config::KbCodeConfig::default()
    };
    let tmp = tempfile::tempdir().unwrap();
    let paths = kb_core::paths::KbPaths::rooted_at(tmp.path(), "kb-code");
    let state = crate::build_state_for_test(cfg, paths).await.unwrap();
    if state.github.has_ambient_token() {
        eprintln!("skipped: a GitHub token is configured in this environment");
        return;
    }
    let st = state.clone();
    let row = tokio::task::spawn_blocking(move || {
        let reg = st.review_stores.register_repo(
            &st.store,
            REPO,
            Some("https://github.com/acme/widgets.git"),
        );
        assert!(matches!(reg, Registration::Member { .. }), "{reg:?}");
        st.store.store_for_repo_name(REPO).unwrap().unwrap()
    })
    .await
    .unwrap();
    assert_eq!(row.forge_slug.as_deref(), Some("acme/widgets"));
    let handle = crate::review_store::StoreHandle {
        id: row.id,
        uuid: row.uuid.clone(),
        git_dir: PathBuf::from(&row.git_dir),
        store_key: row.store_key.clone(),
        base_url: row.base_url.clone(),
        forge_kind: row.forge_kind.clone(),
    };

    let gh_dir = tempfile::tempdir().unwrap();
    let gh = fake_gh(gh_dir.path(), "alice");
    let (client, warnings) = crate::reviews::github_with_gh_cli_warned(
        &state,
        state.github.with_cli_token(None),
        &handle,
        REPO,
        gh.clone(),
    )
    .await;
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(
        !format!("{client:?}").contains("ghp_"),
        "the token must never appear in Debug output"
    );
    let (base, warnings) = crate::reviews::forge_pr_base_ref(
        &state,
        &handle,
        REPO,
        7,
        state.github.with_cli_token(None),
        gh,
    )
    .await;
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(
        base.as_deref(),
        Some("release"),
        "the store's project, not the fork"
    );
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "{seen:?}");
        assert_eq!(seen[0].0, "acme/widgets");
        assert_eq!(
            seen[0].1.as_deref(),
            Some(format!("Bearer {TOKEN_ALICE}").as_str())
        );
    }

    // D12: the store recorded `alice`; gh now answers as `mallory`.
    let st = state.clone();
    let id = row.id;
    tokio::task::spawn_blocking(move || {
        st.store
            .set_review_store_credential(id, "gh-cli", None, Some("alice"))
            .unwrap();
    })
    .await
    .unwrap();
    let gh_dir2 = tempfile::tempdir().unwrap();
    let (_, warnings) = crate::reviews::forge_pr_base_ref(
        &state,
        &handle,
        REPO,
        7,
        state.github.with_cli_token(None),
        fake_gh(gh_dir2.path(), "mallory"),
    )
    .await;
    assert_eq!(
        codes(&warnings),
        vec![warn::CREDENTIAL_ACCOUNT_MISMATCH],
        "{warnings:?}"
    );
    let seen = seen.lock().unwrap();
    assert!(
        seen.iter().skip(1).all(|(_, auth)| auth.is_none()),
        "no token is sent when the account does not match: {seen:?}"
    );
}
