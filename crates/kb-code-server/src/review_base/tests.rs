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
            let resp = crate::reviews::snapshot_review(
                State(state),
                AxumPath(id),
                axum::extract::Query(crate::review_retrack::AsyncParams::default()),
                raw,
            )
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
    let state = unregistered_state_with(fx, github).await;
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

/// [`route_state_with`] without registering the repo with a store (the
/// caller registers it, e.g. with an explicit forge URL).
async fn unregistered_state_with(
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
    crate::build_state_for_test(cfg, paths).await.unwrap()
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
    // Hermetic (A6.f7): whatever `KB_CODE_GITHUB_TOKEN` holds on the runner,
    // THIS client reads it as unset — the test used to `return` (pass
    // vacuously) when an ambient token was present.
    let ambient_free = state.github.for_test().with_env_token_for_test(None);
    assert!(!ambient_free.has_ambient_token());
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
        ambient_free.with_cli_token(None),
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
        ambient_free.with_cli_token(None),
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
        ambient_free.with_cli_token(None),
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

// =====================================================================
// A6-2 / A6-8 — the ONE forge context (`reviews::forge_ctx`)
// =====================================================================

fn store_handle_of(row: &crate::store::ReviewStoreRow) -> crate::review_store::StoreHandle {
    crate::review_store::StoreHandle {
        id: row.id,
        uuid: row.uuid.clone(),
        git_dir: PathBuf::from(&row.git_dir),
        store_key: row.store_key.clone(),
        base_url: row.base_url.clone(),
        forge_kind: row.forge_kind.clone(),
    }
}

/// A mock GitHub whose PR #7 is OPEN on `acme/widgets` and CLOSED on any
/// other project (the fork). Every request's `owner/name` is recorded.
async fn fork_and_canonical_github() -> (
    crate::config::GithubSection,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) {
    use axum::extract::Path as AxumPath;
    use std::sync::{Arc, Mutex};
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen2 = seen.clone();
    let router = axum::Router::new().route(
        "/repos/{owner}/{name}/pulls/{n}",
        axum::routing::get(
            move |AxumPath((owner, name, _n)): AxumPath<(String, String, u64)>| {
                let seen = seen2.clone();
                async move {
                    let canonical = owner == "acme";
                    seen.lock().unwrap().push(format!("{owner}/{name}"));
                    // A test mock of the forge, not a daemon wire body: built as a Value so the
                    // wire ratchet counts production response bodies only.
                    let body = serde_json::json!({
                        "number": 7,
                        "title": if canonical { "canonical" } else { "the fork's own PR 7" },
                        "user": {"login": "someone"},
                        "head": {"ref": "feature",
                                 "sha": if canonical { "cccccccccccccccccccccccccccccccccccccccc" }
                                        else { "ffffffffffffffffffffffffffffffffffffffff" }},
                        "base": {"ref": "main"},
                        "updated_at": "2024-01-01T00:00:00Z", "draft": false,
                        "state": if canonical { "open" } else { "closed" },
                        "merged": false, "labels": []
                    });
                    axum::Json(body)
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (
        crate::config::GithubSection {
            token_file: None,
            api_base: format!("http://{addr}"),
        },
        seen,
    )
}

/// A6-2: the sweep on a ready store reads the STORE's project, so a fork
/// `origin` whose own PR #7 is closed can never overwrite the review's PR
/// snapshot or close it. A failed store lookup (`ForgeCtx::failed_closed`)
/// reads NOTHING, persists nothing and closes nothing — even with an
/// ambient token configured. Before `forge_ctx` the sweep read
/// `origin`'s `someone/widgets` with the unbound ambient client.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sweep_reads_the_store_project_and_a_fork_pr_can_never_close_the_review() {
    use crate::store::StoreBlocking;
    const CANON: &str = "cccccccccccccccccccccccccccccccccccccccc";
    let fx = tokio::task::spawn_blocking(fixture).await.unwrap();
    git(
        &fx.clone,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/someone/widgets.git",
        ],
    );
    let (github, seen) = fork_and_canonical_github().await;
    let state = unregistered_state_with(&fx, github).await;
    let ambient = state
        .github
        .for_test()
        .with_env_token_for_test(Some("ambient-token"));
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
    let handle = store_handle_of(&row);

    let st = state.clone();
    let id = tokio::task::spawn_blocking(move || {
        let id = st
            .store
            .create_review(REPO, Some("t"), "main", "refs/kbc/pr/7", None, 1)
            .unwrap();
        st.store
            .set_review_pr_binding(id, 7, "acme/widgets", Some("old-head"), None, None)
            .unwrap();
        id
    })
    .await
    .unwrap();
    let load = |state: crate::state::SharedState| async move {
        state
            .store
            .run_blocking(move |s| {
                (
                    s.get_review(id).unwrap().unwrap(),
                    s.get_review_pr_binding(id).unwrap().unwrap(),
                )
            })
            .await
    };

    // The store path: acme/widgets #7 is open — nothing to close, and the
    // snapshot is the canonical PR's.
    let gh_dir = tempfile::tempdir().unwrap();
    let ctx = crate::reviews::forge_ctx_for_store(
        &state,
        ambient.with_cli_token(None),
        &handle,
        Some(row.clone()),
        REPO,
        fake_gh(gh_dir.path(), "alice"),
    )
    .await;
    assert_eq!(
        ctx.repo.as_ref().map(|r| format!("{}/{}", r.owner, r.name)),
        Some("acme/widgets".to_string()),
        "the store's forge_slug, never the fork origin"
    );
    let (review, binding) = load(state.clone()).await;
    let out = crate::review_sweep::sweep_one_with(state.clone(), review, binding, true, ctx)
        .await
        .unwrap();
    assert_eq!(out.row["pr_state"], "open", "{}", out.row);
    assert_eq!(out.row["closed"], false, "{}", out.row);
    assert_eq!(out.row["suggest_close"], false, "{}", out.row);
    let (review, binding) = load(state.clone()).await;
    assert_eq!(review.state, "open");
    assert_eq!(binding.pr_head_sha.as_deref(), Some(CANON));
    assert!(binding
        .pr_meta_json
        .as_deref()
        .is_some_and(|m| m.contains("canonical") && !m.contains("fork")));
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        ["acme/widgets"],
        "only the store's project was ever asked"
    );

    // A lookup that failed closed: no read at all, nothing persisted,
    // nothing closed — whatever the ambient token could have answered.
    let before = seen.lock().unwrap().len();
    let closed_ctx = crate::reviews::ForgeCtx::failed_closed(&ambient, "db unreadable".into());
    assert!(
        !closed_ctx.client.has_credentials(),
        "a fail-closed client is bound to nothing: the ambient token must not answer"
    );
    let (review, binding) = load(state.clone()).await;
    let out = crate::review_sweep::sweep_one_with(state.clone(), review, binding, true, closed_ctx)
        .await
        .unwrap();
    assert!(
        out.row["unavailable_reason"]
            .as_str()
            .is_some_and(|r| r.contains("store-unavailable")),
        "{}",
        out.row
    );
    assert_eq!(out.row["closed"], false, "{}", out.row);
    assert_eq!(seen.lock().unwrap().len(), before, "no request was made");
    let (review, binding) = load(state.clone()).await;
    assert_eq!(review.state, "open");
    assert_eq!(binding.pr_head_sha.as_deref(), Some(CANON));

    // The store-less contrast (the documented pre-store posture): `origin`
    // IS the project, so the fork's closed PR #7 would be what is read.
    let origin_ctx = crate::reviews::forge_ctx_with_gh(
        &state,
        &RepoEntry {
            name: REPO.into(),
            path: fx.clone.clone(),
        },
        None,
        fake_gh(gh_dir.path(), "alice"),
    )
    .await;
    // The registered store is not `ready` in this fixture, so there is no
    // store to answer: origin it is, and that is exactly why the store
    // path above must not be bypassed once one is ready.
    assert_eq!(
        origin_ctx
            .repo
            .as_ref()
            .map(|r| format!("{}/{}", r.owner, r.name)),
        Some("someone/widgets".to_string())
    );
}

/// A6-8: what counts as "no store" vs "the lookup FAILED". A DB error or a
/// refused store credential must not read as `NotRegistered`.
#[test]
fn a_failed_store_lookup_is_never_read_as_no_store() {
    use crate::review_store::StoreUnavailable as U;
    let handle = crate::review_store::StoreHandle {
        id: 1,
        uuid: "u".into(),
        git_dir: PathBuf::from("/nonexistent"),
        store_key: "k".into(),
        base_url: None,
        forge_kind: None,
    };
    assert!(crate::reviews::classify_store_lookup(Ok(handle))
        .unwrap()
        .is_some());
    for benign in [
        U::NotRegistered,
        U::Absent,
        U::Seeding,
        U::LockedElsewhere,
        U::MemberPending,
        U::Disabled { detail: "x".into() },
        U::Broken { code: "x".into() },
    ] {
        assert!(
            matches!(crate::reviews::classify_store_lookup(Err(benign)), Ok(None)),
            "no store answers for this repo"
        );
    }
    for failed in [
        U::Error {
            detail: "row decode".into(),
        },
        U::CredentialRefused {
            class: "credential-account-mismatch".into(),
            detail: "d".into(),
        },
    ] {
        let r = crate::reviews::classify_store_lookup(Err(failed));
        assert!(r.is_err(), "a failed lookup must fail closed, got {r:?}");
    }
}

/// D12 fail-closed: the context built for a failed lookup answers nothing
/// and names why — and its client is bound, so the ambient token is not a
/// rung even for a caller who ignores `closed`.
#[test]
fn a_fail_closed_forge_ctx_has_no_repo_no_credential_and_a_named_warning() {
    let cfg = crate::config::GithubSection {
        token_file: None,
        api_base: "http://127.0.0.1:1".into(),
    };
    let ambient = crate::github::GithubClient::new(&cfg).with_env_token_for_test(Some("ambient"));
    assert!(ambient.has_credentials());
    let ctx = crate::reviews::ForgeCtx::failed_closed(&ambient, "boom".into());
    assert!(ctx.repo.is_none());
    assert!(!ctx.client.has_credentials());
    assert!(ctx.refused_reason().unwrap().contains("boom"));
    assert_eq!(
        codes(&ctx.warnings),
        vec![crate::reviews::FORGE_STORE_UNAVAILABLE]
    );
    assert!(ctx.repo_or_reason().is_err());
}

// =====================================================================
// A6-4 — retrack with an unreadable forge API
// =====================================================================

/// A6-4: the forge API could not answer (`forge-base-unread` rides
/// `api_warnings`). Retrack used to resolve the chain with NO caller rung,
/// land on the default branch, record that guess as `set_by=user` (which
/// disables D15 retarget-follow for good) and class it `stale-pin` so a
/// bulk `--yes` applied it. Now the review's own stored PR target is the
/// fallback rung; with neither, the row is `unknown`, a guess is never
/// persisted, and an apply is refused with the review untouched.
#[test]
fn retrack_with_an_unreadable_forge_follows_the_stored_target_and_refuses_a_guess() {
    use crate::review_retrack::retrack_sync;
    let fx = fixture();
    // gitflow: `develop` is one commit ahead of `main`, and the PR targets it.
    git(&fx.author, &["checkout", "-q", "-B", "develop", "main"]);
    commit(&fx.author, "d.txt", "d");
    git(&fx.author, &["push", "-q", "origin", "develop"]);
    git(&fx.author, &["checkout", "-q", "main"]);
    fx.push_pr("develop", &["p1.rs"], "v1");
    // A legacy review pinned at an old sha (an ancestor of main AND develop).
    let review = fx.pr_review(&fx.m1.clone(), None);
    let id = review.id;
    fx.store
        .set_review_base(id, "pin", None, None, "legacy", None)
        .unwrap();
    recap(&fx, id, fetch());
    let unread = vec![warning(
        crate::reviews::FORGE_BASE_UNREAD,
        "the forge API could not read PR #7's target branch (rate limited)",
    )];
    let patchsets = |fx: &Fx| fx.store.list_patchsets(id).unwrap().len();
    let before_ps = patchsets(&fx);
    let before_base = fx.store.get_review_base(id).unwrap().unwrap();

    // 1. No API answer and NO stored target: the default branch is a
    // GUESS. Dry run says `unknown` (never `stale-pin`) and names it ...
    let review = fx.refetch(id);
    let dry = fx
        .with(|c| retrack_sync(c, &review, None, true, None, unread.clone(), true))
        .unwrap();
    assert_eq!(dry.class, RetrackClass::Unknown, "{:?}", dry.warnings);
    assert!(
        codes(&dry.warnings).contains(&warn::PR_TARGET_ASSUMED),
        "{:?}",
        dry.warnings
    );
    assert!(
        codes(&dry.warnings).contains(&crate::reviews::FORGE_BASE_UNREAD),
        "the forge read failure is surfaced, not .ok()-ed away: {:?}",
        dry.warnings
    );
    // ... and an apply is refused, leaving the review exactly as it was.
    let err = fx
        .with(|c| retrack_sync(c, &review, None, true, None, unread.clone(), false))
        .err()
        .expect("a guessed target must not be applied");
    assert_eq!(err.urn, URN_BASE_UNDETERMINED, "{err}");
    let after = fx.store.get_review_base(id).unwrap().unwrap();
    assert_eq!(after.base_set_by, before_base.base_set_by);
    assert_eq!(after.base_mode, before_base.base_mode);
    assert_eq!(patchsets(&fx), before_ps);

    // 2. The review's own PR snapshot names the target: it is the caller
    // rung, so the chain follows `develop` (no guess, no assumed warning).
    fx.store
        .set_review_pr_meta(id, None, Some(r#"{"base_ref":"develop"}"#), 1)
        .unwrap();
    let dry = fx
        .with(|c| retrack_sync(c, &review, None, true, None, unread.clone(), true))
        .unwrap();
    assert!(
        !codes(&dry.warnings).contains(&warn::PR_TARGET_ASSUMED),
        "{:?}",
        dry.warnings
    );
    assert_eq!(
        dry.base.branch.as_deref(),
        Some("develop"),
        "{:?}",
        dry.base
    );
    let applied = fx
        .with(|c| retrack_sync(c, &review, None, true, None, unread, false))
        .unwrap();
    assert_eq!(applied.base.branch.as_deref(), Some("develop"));
    let b = fx.store.get_review_base(id).unwrap().unwrap();
    assert_eq!(b.base_branch.as_deref(), Some("develop"));
}

// =====================================================================
// v0.44 K3 — merged PRs, retrack races, atomic creation
// =====================================================================

/// Author a merge-commit merge of the PR branch into the forge's `main`
/// (GitHub's default "Create a merge commit").
fn merge_pr_into_main(fx: &Fx) {
    git(&fx.author, &["checkout", "-q", "main"]);
    git(
        &fx.author,
        &["merge", "--no-ff", "-q", "-m", "Merge PR 7", "pr"],
    );
    git(&fx.author, &["push", "-q", "origin", "main"]);
}

/// A6-1 — after a merge-commit merge the target tip CONTAINS the PR head,
/// so `merge-base(target, head) == head` and a naive capture is
/// (tip=head, base=head): zero files. The merge-time base (`M^1` of the
/// merge commit that brought the head in) is pinned instead, so the merged
/// PR stays reviewable with its real two-file diff: creation pins it,
/// snapshot/sync reuse keeps the existing patchset, and retrack (dry run
/// AND apply) agree.
#[test]
fn a_pr_merged_into_its_target_is_pinned_to_its_merge_time_base() {
    use crate::review_retrack::retrack_sync;
    let fx = fixture();
    fx.push_pr(&fx.m1, &["a.rs", "b.rs"], "v1");
    let merge_time_target = git(&fx.author, &["rev-parse", "main"]);
    let nr = || NewReview {
        pr: Some(PR),
        forge_base_ref: Some("main".into()),
        ..NewReview::default()
    };
    // Before the merge the PR reviews normally: ps1 carries both files.
    let prepared = fx.with(|c| c.prepare_new(&nr())).unwrap();
    let review = fx.pr_review(&prepared.base_ref, Some(&prepared.policy));
    let id = review.id;
    let ps1 = recap(&fx, id, fetch()).outcome.ps;
    assert_eq!(fx.diff_names(&ps1.base_sha, &ps1.tip_sha).len(), 2);

    merge_pr_into_main(&fx);

    // Creation (sync --merged-since, start-pr for a merged PR): pinned to
    // the target tip the PR was merged INTO, not refused.
    let merged = fx
        .with(|c| c.prepare_new(&nr()))
        .expect("a merged PR is reviewable");
    assert_eq!(merged.policy.mode, BaseMode::Pin, "{:?}", merged.policy);
    assert_eq!(
        merged.policy.pin.as_deref(),
        Some(merge_time_target.as_str())
    );
    assert_eq!(merged.base_tip, merge_time_target);

    // Reuse / snapshot of the existing review: still the real diff.
    let review = fx.refetch(id);
    let again = recap(&fx, id, fetch()).outcome.ps;
    assert_eq!(again.tip_sha, ps1.tip_sha);
    assert_eq!(
        fx.diff_names(&again.base_sha, &again.tip_sha).len(),
        2,
        "the merged PR's patchset is not empty"
    );

    // Retrack: the dry run predicts what the apply does, and neither
    // refuses nor mints an empty patchset.
    let dry = fx
        .with(|c| retrack_sync(c, &review, None, true, Some("main"), vec![], true))
        .expect("dry run");
    assert!(!dry.minted);
    let applied = fx
        .with(|c| retrack_sync(c, &review, None, true, Some("main"), vec![], false))
        .expect("apply");
    let latest = fx.store.latest_patchset(id).unwrap().unwrap();
    assert_eq!(
        fx.diff_names(&latest.base_sha, &latest.tip_sha).len(),
        2,
        "retrack of a merged PR keeps the real diff: {:?}",
        applied.kind
    );
}

/// A6-1 — with NO merge commit to name the base (a fast-forward merge puts
/// the PR commits straight on the target) the refusal stays, typed and
/// honest, on creation, snapshot and retrack — and nothing empty is minted.
#[test]
fn a_fast_forward_merged_pr_is_still_refused_with_the_typed_conflict() {
    use crate::review_retrack::retrack_sync;
    let fx = fixture();
    fx.push_pr(&fx.m1, &["a.rs", "b.rs"], "v1");
    let nr = || NewReview {
        pr: Some(PR),
        forge_base_ref: Some("main".into()),
        ..NewReview::default()
    };
    let prepared = fx.with(|c| c.prepare_new(&nr())).unwrap();
    let review = fx.pr_review(&prepared.base_ref, Some(&prepared.policy));
    let id = review.id;
    recap(&fx, id, fetch());
    let before_ps = fx.store.list_patchsets(id).unwrap().len();

    git(&fx.author, &["checkout", "-q", "main"]);
    git(&fx.author, &["merge", "--ff-only", "-q", "pr"]);
    git(&fx.author, &["push", "-q", "origin", "main"]);

    let err = fx
        .with(|c| c.prepare_new(&nr()))
        .expect_err("no merge commit names the base");
    assert_eq!(err.urn, URN_PR_ALREADY_MERGED, "{err}");
    assert_eq!(err.status, 409);
    let review = fx.refetch(id);
    let err = fx
        .with(|c| c.recapture(&review, &fetch()))
        .expect_err("a snapshot must not mint an empty patchset");
    assert_eq!(err.urn, URN_PR_ALREADY_MERGED, "{err}");
    let dry = fx
        .with(|c| retrack_sync(c, &review, None, true, Some("main"), vec![], true))
        .err()
        .expect("dry run");
    assert_eq!(dry.urn, URN_PR_ALREADY_MERGED, "{dry}");
    let applied = fx
        .with(|c| retrack_sync(c, &review, None, true, Some("main"), vec![], false))
        .err()
        .expect("apply");
    assert_eq!(applied.urn, URN_PR_ALREADY_MERGED, "{applied}");
    assert_eq!(fx.store.list_patchsets(id).unwrap().len(), before_ps);
}

/// A6-3 — the policy check runs UNDER the ops lock, before minting: a
/// capture computed from a policy that a concurrent retrack replaced
/// fails with 409 `base-changed` and mints nothing.
#[test]
fn a_capture_whose_policy_went_stale_mints_nothing() {
    let fx = fixture();
    fx.push_pr(&fx.m1, &["a.rs"], "v1");
    let prepared = fx
        .with(|c| {
            c.prepare_new(&NewReview {
                pr: Some(PR),
                forge_base_ref: Some("main".into()),
                ..NewReview::default()
            })
        })
        .unwrap();
    let review = fx.pr_review(&prepared.base_ref, Some(&prepared.policy));
    let eff = EffectiveBase::Policy(prepared.policy.clone());
    let opts = CaptureOpts {
        force: true,
        kind_hint: None,
    };
    let stale = fx
        .with(|c| c.capture_with(&review, &eff, &opts, || false, |_| panic!("must not run")))
        .expect_err("a stale policy is refused");
    assert_eq!(stale.urn, URN_BASE_CHANGED, "{stale}");
    assert!(fx.store.list_patchsets(review.id).unwrap().is_empty());
    // The same call with a still-valid policy mints.
    let ok = fx
        .with(|c| c.capture_with(&review, &eff, &opts, || true, |_| {}))
        .unwrap();
    assert!(ok.minted);
}

/// A6-3 (real wiring) — through `recapture`: a retrack persists a new
/// policy AFTER recapture read its own (while it waited on the network
/// fetch). recapture's own staleness closure must see it under the ops lock
/// and refuse 409 `base-changed`, minting nothing and leaving the retracked
/// policy alone.
#[test]
fn recapture_refuses_when_a_retrack_lands_during_its_fetch() {
    let fx = fixture();
    fx.push_pr(&fx.m1, &["a.rs"], "v1");
    let prepared = fx
        .with(|c| {
            c.prepare_new(&NewReview {
                pr: Some(PR),
                forge_base_ref: Some("main".into()),
                ..NewReview::default()
            })
        })
        .unwrap();
    let review = fx.pr_review(&prepared.base_ref, Some(&prepared.policy));
    let id = review.id;
    let rc = Recapture {
        network: true,
        after_fetch: Some(super::capture::TestHook(std::sync::Arc::new(
            move |store| {
                store
                    .set_review_base(id, "track", Some("develop"), None, "user", None)
                    .unwrap();
            },
        ))),
        ..Recapture::default()
    };
    let err = fx
        .with(|c| c.recapture(&review, &rc))
        .expect_err("a recapture computed from a replaced policy must not mint");
    assert_eq!(err.urn, URN_BASE_CHANGED, "{err}");
    assert_eq!(err.status, 409);
    assert!(fx.store.list_patchsets(id).unwrap().is_empty());
    let row = fx.store.get_review_base(id).unwrap().unwrap();
    assert_eq!(
        row.base_branch.as_deref(),
        Some("develop"),
        "the retracked policy survives"
    );
}

/// A6-7 — a tracked base branch the forge deleted: the dry run used to
/// classify against the stale `refs/remotes/base/<branch>` the failed fetch
/// left behind (no warning) while the apply answered 409 `base-vanished`.
#[test]
fn retrack_dry_run_reports_a_vanished_base_like_the_apply_does() {
    use crate::review_retrack::retrack_sync;
    let fx = fixture();
    git(&fx.author, &["checkout", "-q", "-B", "release-3", "main"]);
    commit(&fx.author, "r3.txt", "r3");
    git(&fx.author, &["push", "-q", "origin", "release-3"]);
    git(&fx.author, &["checkout", "-q", "main"]);
    fx.push_pr(&fx.m1, &["p1.rs"], "v1");
    let policy = BasePolicy::track("release-3", SetBy::User, BaseSource::Explicit);
    let review = fx.pr_review(&fx.m1.clone(), Some(&policy));
    let id = review.id;
    // The store fetches release-3 once (the stale ref stays afterwards).
    recap(&fx, id, fetch());
    git(&fx.author, &["push", "-q", "origin", ":release-3"]);
    let review = fx.refetch(id);
    let dry = fx
        .with(|c| retrack_sync(c, &review, None, true, Some("release-3"), vec![], true))
        .err()
        .expect("the dry run must not classify against a deleted branch");
    assert_eq!(dry.urn, URN_BASE_VANISHED, "{dry}");
    let apply = fx
        .with(|c| retrack_sync(c, &review, None, true, Some("release-3"), vec![], false))
        .err()
        .expect("apply");
    assert_eq!(apply.urn, URN_BASE_VANISHED, "{apply}");
}

/// A6-1 (retrack-bulk) — a CLOSED review is final: bulk retrack no longer
/// scans it (re-basing one minted an empty `base-corrected` patchset that
/// flipped its verdict stale and orphaned its findings).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retrack_bulk_skips_closed_reviews() {
    use axum::extract::State;
    use axum::response::IntoResponse;
    let fx = tokio::task::spawn_blocking(fixture).await.unwrap();
    git(&fx.clone, &["checkout", "-q", "-b", "feature"]);
    commit(&fx.clone, "f.rs", "f");
    let state = route_state(&fx).await;
    let mk = |head: &str| {
        let id = state
            .store
            .create_review(REPO, Some("t"), &fx.m1, head, None, 1)
            .unwrap();
        state
            .store
            .set_review_base(id, "pin", None, None, "legacy", None)
            .unwrap();
        id
    };
    let open_id = mk("feature");
    let closed_id = mk("feature");
    state
        .store
        .update_review(closed_id, None, Some("closed"), 2)
        .unwrap();
    let body = crate::review_retrack::RetrackAllBody {
        repo: Some(REPO.into()),
        pinned: true,
        legacy: false,
        dry_run: true,
    };
    let resp = crate::review_retrack::retrack_all_route(
        State(state.clone()),
        axum::extract::Query(crate::review_retrack::AsyncParams::default()),
        axum::Json(body),
    )
    .await
    .unwrap_or_else(|e| panic!("retrack-bulk: {e:?}"))
    .into_response();
    let out = body_json(resp).await;
    let ids: Vec<i64> = out["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["id"].as_i64())
        .collect();
    assert!(ids.contains(&open_id), "{out}");
    assert!(!ids.contains(&closed_id), "closed review scanned: {out}");
}

/// A6.f8 — `review status` shares `verdict_block`'s staleness rule: a
/// legacy verdict with NO recorded patchset is not "stale" (the old
/// `status` expression `verdict_ps != latest_ps` said it was), and only a
/// strictly later patchset makes a verdict stale.
#[test]
fn verdict_staleness_has_one_rule() {
    let mut row = ReviewRow {
        id: 1,
        repo: REPO.into(),
        title: None,
        base_ref: "main".into(),
        head_ref: "feature".into(),
        session_id: None,
        state: "open".into(),
        created_at: 1,
        updated_at: 1,
        verdict: Some("approve".into()),
        verdict_note: None,
        verdict_at: Some(1),
        verdict_ps: None,
    };
    assert!(!crate::reviews::verdict_block(&row, Some(2)).1);
    row.verdict_ps = Some(1);
    assert!(crate::reviews::verdict_block(&row, Some(2)).1);
    assert!(!crate::reviews::verdict_block(&row, Some(1)).1);
}

/// A6.f4 — a failed step after the row insert removes the row (no unbound,
/// patchset-less orphan for the next sync to duplicate); success keeps it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_creation_step_discards_the_half_created_review() {
    let fx = tokio::task::spawn_blocking(fixture).await.unwrap();
    let state = route_state(&fx).await;
    let mk = || {
        state
            .store
            .create_review(REPO, Some("t"), "main", "feature", None, 1)
            .unwrap()
    };
    let kept = mk();
    let ok: Result<(), crate::routes::ApiError> = Ok(());
    crate::reviews::discard_on_err(&state, kept, ok)
        .await
        .unwrap();
    assert!(state.store.get_review(kept).unwrap().is_some());
    let gone = mk();
    let err: Result<(), crate::routes::ApiError> =
        Err(crate::routes::ApiError::bad_request("boom"));
    assert!(crate::reviews::discard_on_err(&state, gone, err)
        .await
        .is_err());
    assert!(state.store.get_review(gone).unwrap().is_none());
}

// =====================================================================
// v0.44 K6 — the no-store reuse path (A6-5): base refresh + D15 follow
// =====================================================================

/// Author a `develop` branch on the forge (one commit off `m1`).
fn push_develop(fx: &Fx) -> String {
    git(&fx.author, &["checkout", "-q", "-B", "develop", &fx.m1]);
    let tip = commit(&fx.author, "dev.txt", "dev");
    git(&fx.author, &["push", "-q", "-f", "origin", "develop"]);
    git(&fx.author, &["checkout", "-q", "main"]);
    tip
}

/// A no-store PR review row (the pre-store fallback shape): the stored
/// `base_ref` is `refs/remotes/origin/<b>` and the recorded ladder answer
/// rides `pr_meta_json.base_source`.
fn no_store_pr_review(state: &crate::state::SharedState, base_ref: &str, source: &str) -> i64 {
    let id = state
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
    let meta = serde_json::json!({ "base_source": source }).to_string();
    state
        .store
        .set_review_pr_binding(id, PR as i64, "acme/widgets", None, Some(&meta), Some(1))
        .unwrap();
    id
}

fn warning_codes(body: &serde_json::Value) -> Vec<String> {
    body["warnings"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|w| w["code"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// A6-5 WIRING — `reuse_pr_review` on a repo with no ready store really
/// refreshes the remote-tracking base before measuring: the forge's `main`
/// moved after the clone's last fetch, and the clone's
/// `refs/remotes/origin/main` must follow it (the helper alone was tested;
/// nothing proved the route called it).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reuse_pr_review_refreshes_the_origin_base_before_capturing() {
    let fx = tokio::task::spawn_blocking(fixture).await.unwrap();
    fx.push_pr(&fx.m1, &["a.rs"], "v1");
    let state = unregistered_state_with(&fx, crate::config::GithubSection::default()).await;
    let id = no_store_pr_review(&state, "refs/remotes/origin/main", "merge-base");
    let new_main = fx.advance_main(2, "adv");
    assert_ne!(
        git(&fx.clone, &["rev-parse", "refs/remotes/origin/main"]),
        new_main,
        "precondition: the clone's origin/main is stale"
    );
    let repo = RepoEntry {
        name: REPO.into(),
        path: fx.clone.clone(),
    };
    let review = state.store.get_review(id).unwrap().unwrap();
    let (status, body) = crate::reviews::reuse_pr_review(
        &state,
        &repo,
        review,
        PR,
        false,
        state.github.for_test(),
        Some(Some("main".into())),
    )
    .await
    .unwrap_or_else(|e| panic!("reuse: {e:?}"));
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    assert_eq!(
        git(&fx.clone, &["rev-parse", "refs/remotes/origin/main"]),
        new_main,
        "reuse refreshed the remote-tracking base"
    );
}

/// D15 (no-store path) — a PR retargeted on the forge is followed when kb
/// chose the base (`base_source` merge-base), never when a person did
/// (`explicit`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reuse_pr_review_follows_a_forge_retarget_unless_the_base_was_explicit() {
    let fx = tokio::task::spawn_blocking(fixture).await.unwrap();
    fx.push_pr(&fx.m1, &["a.rs"], "v1");
    push_develop(&fx);
    let state = unregistered_state_with(&fx, crate::config::GithubSection::default()).await;
    let repo = RepoEntry {
        name: REPO.into(),
        path: fx.clone.clone(),
    };
    let id = no_store_pr_review(&state, "refs/remotes/origin/main", "explicit");
    let reuse = |state: crate::state::SharedState, repo: RepoEntry, id: i64| async move {
        let review = state.store.get_review(id).unwrap().unwrap();
        crate::reviews::reuse_pr_review(
            &state,
            &repo,
            review,
            PR,
            false,
            state.github.for_test(),
            Some(Some("develop".into())),
        )
        .await
        .unwrap_or_else(|e| panic!("reuse: {e:?}"))
    };

    // A person's base never moves; the divergence is a warning.
    let (_, kept) = reuse(state.clone(), repo.clone(), id).await;
    assert_eq!(kept["base_ref"], "refs/remotes/origin/main", "{kept}");
    assert!(!warning_codes(&kept).contains(&"retargeted".to_string()));

    // kb's own base follows the retarget and says so.
    let meta = serde_json::json!({ "base_source": "merge-base" }).to_string();
    state
        .store
        .set_review_pr_meta(id, None, Some(&meta), 2)
        .unwrap();
    let (_, followed) = reuse(state.clone(), repo.clone(), id).await;
    assert_eq!(
        followed["base_ref"], "refs/remotes/origin/develop",
        "{followed}"
    );
    assert!(
        warning_codes(&followed).contains(&"retargeted".to_string()),
        "{followed}"
    );
    assert_eq!(
        state.store.get_review(id).unwrap().unwrap().base_ref,
        "refs/remotes/origin/develop"
    );
}

/// The pure follow rule: only a daemon-chosen `refs/remotes/origin/<b>`
/// base moves, and only to a valid, different branch.
#[test]
fn retarget_follow_ref_only_moves_a_daemon_chosen_origin_base() {
    use crate::reviews::retarget_follow_ref;
    assert_eq!(
        retarget_follow_ref("refs/remotes/origin/main", Some("merge-base"), "develop").as_deref(),
        Some("refs/remotes/origin/develop")
    );
    assert_eq!(
        retarget_follow_ref("refs/remotes/origin/main", None, "develop").as_deref(),
        Some("refs/remotes/origin/develop")
    );
    for (base, src, forge) in [
        ("refs/remotes/origin/main", Some("explicit"), "develop"),
        ("refs/remotes/origin/main", Some("merge-base"), "main"),
        ("main", Some("merge-base"), "develop"),
        ("0123456789012345678901234567890123456789", None, "develop"),
        ("refs/remotes/origin/main", None, "bad:name"),
    ] {
        assert_eq!(
            retarget_follow_ref(base, src, forge),
            None,
            "{base} {forge}"
        );
    }
}

// =====================================================================
// v0.44 K6 — A6.f4: atomic creation + the call-site discard
// =====================================================================

async fn prepared_for_head(
    state: &crate::state::SharedState,
    head: &str,
) -> (crate::review_store::StoreHandle, Member, capture::Prepared) {
    let handle = state
        .review_stores
        .handle_for_repo(&state.store, REPO)
        .expect("the store is ready");
    let member = crate::reviews::store_member(state, REPO).unwrap();
    let nr = NewReview {
        head_ref: head.to_string(),
        ..NewReview::default()
    };
    let prepared =
        crate::reviews::with_store_ctx(state, handle.clone(), member.clone(), move |c| {
            c.prepare_new(&nr)
        })
        .await
        .unwrap()
        .unwrap();
    (handle, member, prepared)
}

/// A6.f4 — the row and its base policy land in ONE transaction, and the
/// real call site (`capture_new_in_store`) removes the row again when the
/// first capture fails: a head that vanished between resolution and
/// capture leaves no patchset-less orphan for the next sync to duplicate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_first_capture_removes_the_atomically_created_row_at_the_call_site() {
    let fx = tokio::task::spawn_blocking(fixture).await.unwrap();
    git(&fx.clone, &["checkout", "-q", "-b", "feature"]);
    commit(&fx.clone, "f.rs", "f");
    git(&fx.clone, &["checkout", "-q", "main"]);
    let state = route_state(&fx).await;

    // Success keeps the row, with its base policy already written.
    let (handle, member, prepared) = prepared_for_head(&state, "feature").await;
    let kept = crate::reviews::insert_store_review(
        &state,
        REPO,
        Some("t".into()),
        &prepared,
        "feature".into(),
        None,
        None,
    )
    .await
    .unwrap();
    let base = state.store.get_review_base(kept.id).unwrap().unwrap();
    assert!(base.base_mode.is_some(), "the policy rode the insert tx");
    crate::reviews::capture_new_in_store(&state, handle, member, &kept, &prepared)
        .await
        .unwrap();
    assert!(state.store.get_review(kept.id).unwrap().is_some());
    assert_eq!(state.store.list_patchsets(kept.id).unwrap().len(), 1);

    // The head disappears after the row was inserted: the capture fails and
    // the row goes with it.
    let (handle, member, prepared) = prepared_for_head(&state, "feature").await;
    let doomed = crate::reviews::insert_store_review(
        &state,
        REPO,
        Some("t".into()),
        &prepared,
        "feature".into(),
        None,
        None,
    )
    .await
    .unwrap();
    git(&fx.clone, &["branch", "-q", "-D", "feature"]);
    crate::reviews::capture_new_in_store(&state, handle, member, &doomed, &prepared)
        .await
        .expect_err("the head no longer exists");
    assert!(
        state.store.get_review(doomed.id).unwrap().is_none(),
        "the half-created review was discarded"
    );
}

/// A6.f4 — a PR binding that cannot be written (an OPEN review already
/// binds the PR) fails the WHOLE creation: no row survives, so a policy-less,
/// unbound orphan never reaches the next sync.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_binding_collision_rolls_the_whole_creation_back() {
    let fx = tokio::task::spawn_blocking(fixture).await.unwrap();
    git(&fx.clone, &["checkout", "-q", "-b", "feature"]);
    commit(&fx.clone, "f.rs", "f");
    git(&fx.clone, &["checkout", "-q", "main"]);
    let state = route_state(&fx).await;
    let (_h, _m, prepared) = prepared_for_head(&state, "feature").await;
    let bind = || crate::reviews::PrBindingInput {
        number: PR as i64,
        repo_slug: "acme/widgets".into(),
        head_sha: "a".repeat(40),
        meta_json: None,
    };
    let first = crate::reviews::insert_store_review(
        &state,
        REPO,
        None,
        &prepared,
        "feature".into(),
        None,
        Some(bind()),
    )
    .await
    .unwrap();
    crate::reviews::insert_store_review(
        &state,
        REPO,
        None,
        &prepared,
        "feature".into(),
        None,
        Some(bind()),
    )
    .await
    .expect_err("an open review already binds PR 7");
    assert!(
        state.store.get_review(first.id + 1).unwrap().is_none(),
        "the failed creation left no row behind"
    );
}

/// K3 regression — a transient forge error while enriching must degrade
/// `pr_meta` (a reason on the envelope), never fail the creation: it used
/// to be a fallible step AFTER the capture, and `discard_on_err` deleted the
/// freshly minted good patchset with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_forge_error_during_enrichment_degrades_pr_meta_and_cannot_fail_creation() {
    let fx = tokio::task::spawn_blocking(fixture).await.unwrap();
    let state = route_state(&fx).await;
    let gh = crate::github::GithubRepo {
        owner: "acme".into(),
        name: "widgets".into(),
    };
    let (meta, reason) = crate::reviews::pr_enrichment(
        state.github.for_test(),
        Some(&gh),
        Some(Err(crate::github::GithubApiError::Unreachable(
            "https://api.invalid/x".into(),
            "connection reset".into(),
        ))),
        PR,
        false,
        "merge-base",
    )
    .await;
    assert!(meta.is_none());
    assert!(reason.is_some(), "the degradation is named, not swallowed");
}

// =====================================================================
// v0.44 K6 — retrack on a forge with NO API
// =====================================================================

/// K2 carry — a store whose forge has no API (here a local-path forge)
/// cannot name a PR's target; retrack falls to the assumed default branch.
/// That is a guess: it is reported `default-assumed` and recorded
/// `set_by=auto`, never as a person's `user` decision (which would stop
/// the review following the PR when the target becomes knowable).
#[test]
fn retrack_on_a_forge_with_no_api_records_a_default_assumed_auto_base() {
    use crate::review_retrack::retrack_sync;
    let fx = fixture();
    fx.push_pr(&fx.m1, &["p1.rs"], "v1");
    let policy = BasePolicy::pin(&fx.m1, SetBy::Legacy, BaseSource::Legacy);
    let review = fx.pr_review(&fx.m1.clone(), Some(&policy));
    let id = review.id;
    recap(&fx, id, fetch());
    let review = fx.refetch(id);
    // No forge answer and no warnings: there was no API to ask.
    let out = fx
        .with(|c| retrack_sync(c, &review, None, true, None, vec![], false))
        .expect("retrack applies");
    assert_eq!(
        out.base.source.as_deref(),
        Some("default-assumed"),
        "{:?}",
        out.base
    );
    assert_eq!(out.base.set_by, "auto", "{:?}", out.base);
    let stored = fx.store.get_review_base(id).unwrap().unwrap();
    assert_eq!(stored.base_set_by, "auto", "never persisted as the user's");
}

/// K7 — the capture-side Vanished retry shares the pass budget: N vanished
/// specs (branches plus the PR head) against a 100 ms budget run a handful,
/// the rest are reported `offline`/`timeout` (and the PR head's error says
/// so), never ten full budgets.
#[test]
fn n_vanished_capture_specs_stay_within_one_base_fetch_timeout() {
    use crate::review_store::url::{FetchRefspec, RefSource};
    use crate::review_store::{FailureClass, StoreGitError};
    let started = std::time::Instant::now();
    let budget = std::time::Duration::from_millis(100);
    let mut specs: Vec<(FetchRefspec, Option<String>)> = (0..9)
        .map(|i| {
            let b = format!("b{i}");
            (
                FetchRefspec::new(
                    true,
                    RefSource::Ref(RefName::branch(&b).unwrap()),
                    RefName::parse(&format!("refs/remotes/base/{b}")).unwrap(),
                ),
                Some(b),
            )
        })
        .collect();
    specs.push((
        FetchRefspec::new(
            true,
            RefSource::Ref(RefName::parse("refs/pull/9/head").unwrap()),
            RefName::parse("refs/kbc/pr/9").unwrap(),
        ),
        None,
    ));
    let mut report = capture::FetchReport::default();
    let mut calls = 0;
    let mut consumed = std::time::Duration::ZERO;
    let pr_fetched =
        capture::retry_vanished_specs(&mut report, &specs, started + budget, |_, left| {
            calls += 1;
            let burn = left.min(std::time::Duration::from_millis(40));
            consumed += burn;
            std::thread::sleep(burn);
            Err(StoreGitError {
                op: "fetch",
                class: FailureClass::Vanished,
                exit_code: Some(128),
                detail: "gone".into(),
            })
        });
    // Budget accounting, not wall time: the calls' consumed budgets cannot
    // sum past the pass budget however slow the runner is.
    assert!(calls < 10, "ran {calls} of 10");
    assert!(
        consumed <= budget,
        "the fetches consumed {consumed:?} of a {budget:?} pass budget"
    );
    assert!(!pr_fetched);
    assert_eq!(report.state, "offline", "{report:?}");
    assert_eq!(report.code.as_deref(), Some("timeout"), "{report:?}");
    assert_eq!(report.pr_error.as_deref(), Some("timeout"), "{report:?}");
    assert_eq!(
        report.vanished.len(),
        calls,
        "ran specs are reported vanished"
    );
}

// =====================================================================
// v0.44 K6 — X1/K3: bulk retrack fetches each base once, holds no lock
// across the scan; snapshot / retrack / retrack-bulk / start run as jobs
// =====================================================================

/// X1/K3 — a bulk run's memo fetches each distinct base branch ONCE: the
/// forge's `main` moves between two memoised fetches, and the second one does
/// not touch the store (while an un-memoised fetch does).
#[test]
fn a_base_fetch_memo_fetches_each_branch_once_per_run() {
    let fx = fixture();
    let memo = capture::BaseFetchMemo::default();
    let branches = vec!["main".to_string()];
    let base_ref = |fx: &Fx| git(&fx.store_dir(), &["rev-parse", "refs/remotes/base/main"]);
    let first = fx.with(|c| {
        let access = c.access();
        c.fetch_forge_memo(
            Some(&memo),
            access.as_ref().map_err(String::as_str),
            &branches,
            None,
        )
    });
    assert!(first.fetched(), "{first:?}");
    let tip1 = base_ref(&fx);
    let tip2 = fx.advance_main(1, "moved");
    assert_ne!(tip1, tip2);

    let second = fx.with(|c| {
        let access = c.access();
        c.fetch_forge_memo(
            Some(&memo),
            access.as_ref().map_err(String::as_str),
            &branches,
            None,
        )
    });
    assert!(
        second.fetched(),
        "the memoised outcome is reported: {second:?}"
    );
    assert_eq!(
        base_ref(&fx),
        tip1,
        "the memoised branch was NOT fetched again"
    );
    assert_eq!(memo.len(), 1);

    // Without a memo (every other caller) the fetch is unchanged.
    fx.with(|c| {
        let access = c.access();
        c.fetch_forge_memo(
            None,
            access.as_ref().map_err(String::as_str),
            &branches,
            None,
        )
    });
    assert_eq!(base_ref(&fx), tip2);
}

/// X1/K3 — `retrack-bulk` applies must not hold the repo's sync lock across
/// the scan's network fetches: with `repo_guard` held by someone else (a
/// start-pr / sync in flight), a bulk run that has nothing to APPLY still
/// completes. (It used to take the guard up front whenever `--yes` was
/// passed, so `start-pr` and `sync` for that repo blocked for the whole run
/// and the run blocked on them.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retrack_bulk_apply_does_not_take_the_repo_guard_for_the_scan() {
    use axum::extract::State;
    let fx = tokio::task::spawn_blocking(fixture).await.unwrap();
    git(&fx.clone, &["checkout", "-q", "-b", "feature"]);
    commit(&fx.clone, "f.rs", "f");
    let state = route_state(&fx).await;
    let id = state
        .store
        .create_review(REPO, Some("t"), &fx.m1, "feature", None, 1)
        .unwrap();
    state
        .store
        .set_review_base(id, "pin", None, None, "legacy", None)
        .unwrap();
    // ps1 first, so the bulk classification is `equivalent` (nothing to
    // apply) — an un-captured review would classify `stale-pin` and the
    // apply would legitimately need the guard.
    crate::reviews::snapshot_review(
        State(state.clone()),
        axum::extract::Path(id),
        axum::extract::Query(crate::review_retrack::AsyncParams::default()),
        axum::body::Bytes::from_static(b"{}"),
    )
    .await
    .unwrap_or_else(|e| panic!("snapshot: {e:?}"));
    let held = crate::review_sync::repo_guard(&state, REPO)
        .await
        .expect("a known repo");
    let body = crate::review_retrack::RetrackAllBody {
        repo: Some(REPO.into()),
        pinned: true,
        legacy: false,
        dry_run: false,
    };
    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        crate::review_retrack::retrack_all_route(
            State(state.clone()),
            axum::extract::Query(crate::review_retrack::AsyncParams::default()),
            axum::Json(body),
        ),
    )
    .await
    .expect("the scan must not wait on the repo guard another request holds")
    .unwrap_or_else(|e| panic!("retrack-bulk: {e:?}"));
    use axum::response::IntoResponse;
    let out = body_json(resp.into_response()).await;
    let ids: Vec<i64> = out["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["id"].as_i64())
        .collect();
    assert!(ids.contains(&id), "{out}");
    drop(held);
}

/// Poll `GET /api/reviews/jobs/{id}` until the job settles.
async fn settle_job(state: &crate::state::SharedState, job_id: &str) -> serde_json::Value {
    use axum::response::IntoResponse;
    for _ in 0..600 {
        let resp = crate::review_jobs::review_job_route(
            axum::extract::State(state.clone()),
            axum::extract::Path(job_id.to_string()),
        )
        .await
        .unwrap_or_else(|e| panic!("job read: {e:?}"))
        .into_response();
        let body = body_json(resp).await;
        if body["status"] != "running" {
            return body;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("job {job_id} never settled");
}

/// X1/K3 — snapshot, retrack and retrack-bulk answer `?async=1` with a 202 +
/// `job_id` at once and settle with the same body the synchronous route
/// returns, so a caller never holds one HTTP request open for the network
/// work.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_retrack_and_bulk_run_as_daemon_jobs() {
    use axum::extract::{Path, Query, State};
    use axum::response::IntoResponse;
    let fx = tokio::task::spawn_blocking(fixture).await.unwrap();
    git(&fx.clone, &["checkout", "-q", "-b", "feature"]);
    commit(&fx.clone, "f.rs", "f");
    let state = route_state(&fx).await;
    let id = state
        .store
        .create_review(REPO, Some("t"), &fx.m1, "feature", None, 1)
        .unwrap();
    state
        .store
        .set_review_base(id, "pin", None, None, "legacy", None)
        .unwrap();
    let want_async = || {
        Query(crate::review_retrack::AsyncParams {
            async_: Some("1".into()),
        })
    };
    let job_id = |resp: axum::response::Response| async move {
        assert_eq!(resp.status(), axum::http::StatusCode::ACCEPTED);
        let body = body_json(resp).await;
        body["job_id"].as_str().expect("job_id").to_string()
    };

    // snapshot
    let resp = crate::reviews::snapshot_review(
        State(state.clone()),
        Path(id),
        want_async(),
        axum::body::Bytes::from_static(b"{}"),
    )
    .await
    .unwrap_or_else(|e| panic!("snapshot: {e:?}"));
    let jid = job_id(resp).await;
    let job = settle_job(&state, &jid).await;
    assert_eq!(job["kind"], "snapshot", "{job}");
    assert_eq!(job["status"], "done", "{job}");
    assert_eq!(job["result"]["review_id"], id, "{job}");

    // retrack (dry run)
    let resp = crate::review_retrack::retrack_route(
        State(state.clone()),
        Path(id),
        want_async(),
        axum::body::Bytes::from_static(br#"{"dry_run":true}"#),
    )
    .await
    .unwrap_or_else(|e| panic!("retrack: {e:?}"));
    let jid = job_id(resp).await;
    let job = settle_job(&state, &jid).await;
    assert_eq!(job["kind"], "retrack", "{job}");
    assert_ne!(job["status"], "running", "{job}");
    if job["status"] == "done" {
        assert_eq!(job["result"]["id"], id, "{job}");
    }

    // retrack-bulk (dry run)
    let resp = crate::review_retrack::retrack_all_route(
        State(state.clone()),
        want_async(),
        axum::Json(crate::review_retrack::RetrackAllBody {
            repo: Some(REPO.into()),
            pinned: true,
            legacy: false,
            dry_run: true,
        }),
    )
    .await
    .unwrap_or_else(|e| panic!("retrack-bulk: {e:?}"));
    let jid = job_id(resp).await;
    let job = settle_job(&state, &jid).await;
    assert_eq!(job["kind"], "retrack-bulk", "{job}");
    assert_eq!(job["status"], "done", "{job}");
    assert_eq!(
        job["result"]["schema"],
        crate::review_retrack::RETRACK_ALL_SCHEMA,
        "{job}"
    );

    // The synchronous form is unchanged (200, the body itself).
    let sync = crate::review_retrack::retrack_route(
        State(state.clone()),
        Path(id),
        Query(crate::review_retrack::AsyncParams::default()),
        axum::body::Bytes::from_static(br#"{"dry_run":true}"#),
    )
    .await
    .unwrap_or_else(|e| panic!("retrack: {e:?}"))
    .into_response();
    assert_eq!(sync.status(), axum::http::StatusCode::OK);
}

// =====================================================================
// v0.44 K6 — A6.f9: retrack racing snapshot/sync, and a forge that answers
// 403 / 404
// =====================================================================

/// A6.f9 — a snapshot/sync that retargets the review (persisting a new
/// auto policy) while a retrack is in its network fetch: the retrack's
/// capture was computed from the policy it started with, so it must refuse
/// 409 `base-changed` under the ops lock, mint nothing, and leave the
/// snapshot's policy alone — the mirror image of
/// `recapture_refuses_when_a_retrack_lands_during_its_fetch`.
#[test]
fn retrack_refuses_when_a_snapshot_retargets_during_its_fetch() {
    use crate::review_retrack::{retrack_sync, tests_seam::AFTER_FETCH};
    let fx = fixture();
    git(&fx.author, &["checkout", "-q", "-B", "develop", "main"]);
    commit(&fx.author, "d.txt", "d");
    git(&fx.author, &["push", "-q", "origin", "develop"]);
    git(&fx.author, &["checkout", "-q", "main"]);
    fx.push_pr(&fx.m1, &["p1.rs"], "v1");
    let policy = BasePolicy::pin(&fx.m1, SetBy::Legacy, BaseSource::Legacy);
    let review = fx.pr_review(&fx.m1.clone(), Some(&policy));
    let id = review.id;
    recap(&fx, id, fetch());
    let review = fx.refetch(id);
    let before_ps = fx.store.list_patchsets(id).unwrap().len();

    // The "snapshot" lands its retarget write right after retrack's fetch.
    AFTER_FETCH.with(|h| {
        *h.borrow_mut() = Some(super::capture::TestHook(std::sync::Arc::new(
            move |store: &Store| {
                store
                    .set_review_base(id, "track", Some("develop"), None, "auto", None)
                    .unwrap();
            },
        )));
    });
    let res = fx.with(|c| retrack_sync(c, &review, None, true, Some("main"), vec![], false));
    AFTER_FETCH.with(|h| *h.borrow_mut() = None);
    let err = res
        .err()
        .expect("a retrack computed from a replaced policy must not mint");
    assert_eq!(err.urn, URN_BASE_CHANGED, "{err}");
    assert_eq!(err.status, 409);
    assert_eq!(fx.store.list_patchsets(id).unwrap().len(), before_ps);
    let row = fx.store.get_review_base(id).unwrap().unwrap();
    assert_eq!(
        row.base_branch.as_deref(),
        Some("develop"),
        "the snapshot's policy survives"
    );
    assert_eq!(row.base_set_by, "auto");
}

/// A forge whose every GET answers `status` (a 403 from a token without
/// `repo` scope, a 404 from a private repo the token cannot see).
async fn forge_answering(status: u16) -> crate::config::GithubSection {
    // A test mock of the forge, not a daemon wire body: the body is built as a
    // Value first so the wire ratchet counts production response bodies only.
    let denied = serde_json::json!({ "message": "denied" });
    let router = axum::Router::new().fallback(move || {
        let denied = denied.clone();
        async move {
            (
                axum::http::StatusCode::from_u16(status).unwrap(),
                axum::Json(denied),
            )
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    crate::config::GithubSection {
        token_file: None,
        api_base: format!("http://{addr}"),
    }
}

/// A6.f9 — the REAL forge read failing 403 / 404 (not a synthetic warning):
/// `forge_pr_base_ref` reports the target unread with a named
/// `forge-base-unread` warning, and a retrack fed that answer classes the
/// row `unknown` and refuses to apply a guessed default — the review keeps
/// its policy. (A plain `#[test]` with its own runtime: the store fixture
/// blocks, which a runtime worker thread may not.)
#[test]
fn a_forge_answering_403_or_404_leaves_the_target_unread_and_retrack_refuses_the_guess() {
    use crate::review_retrack::retrack_sync;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let fx = fixture();
    fx.push_pr(&fx.m1, &["p1.rs"], "v1");
    let review = fx.pr_review(&fx.m1.clone(), None);
    let id = review.id;
    fx.store
        .set_review_base(id, "pin", None, None, "legacy", None)
        .unwrap();
    recap(&fx, id, fetch());
    let before = fx.store.get_review_base(id).unwrap().unwrap();
    let gh_dir = tempfile::tempdir().unwrap();

    for status in [403u16, 404] {
        let (base, warnings) = rt.block_on(async {
            let github = forge_answering(status).await;
            let state = unregistered_state_with(&fx, github).await;
            let st = state.clone();
            let row = tokio::task::spawn_blocking(move || {
                st.review_stores.register_repo(
                    &st.store,
                    REPO,
                    Some("https://github.com/acme/widgets.git"),
                );
                st.store.store_for_repo_name(REPO).unwrap().unwrap()
            })
            .await
            .unwrap();
            let handle = store_handle_of(&row);
            crate::reviews::forge_pr_base_ref(
                &state,
                &handle,
                REPO,
                PR,
                state.github.for_test().with_cli_token(None),
                fake_gh(gh_dir.path(), "alice"),
            )
            .await
        });
        assert_eq!(base, None, "{status}: nothing readable");
        assert!(
            warnings
                .iter()
                .any(|w| w.code == crate::reviews::FORGE_BASE_UNREAD),
            "{status}: {warnings:?}"
        );

        let review = fx.refetch(id);
        let w = warnings.clone();
        let dry = fx
            .with(|c| retrack_sync(c, &review, None, true, base.as_deref(), w, true))
            .unwrap();
        assert_eq!(
            dry.class,
            RetrackClass::Unknown,
            "{status}: {:?}",
            dry.warnings
        );
        let err = fx
            .with(|c| retrack_sync(c, &review, None, true, base.as_deref(), warnings, false))
            .err()
            .expect("a guessed target must not be applied");
        assert_eq!(err.urn, URN_BASE_UNDETERMINED, "{status}: {err}");
        let after = fx.store.get_review_base(id).unwrap().unwrap();
        assert_eq!(after.base_set_by, before.base_set_by, "{status}");
        assert_eq!(after.base_mode, before.base_mode, "{status}");
    }
}

/// A6.f5 / A6-8: a recapture whose DB write FAILS must surface as a 500
/// `capture-failed`, never as a 200 carrying a `base` block the DB does not
/// hold. The failing write is injected with a `BEFORE UPDATE` trigger that
/// aborts on the review's base columns.
#[test]
fn a_failing_base_write_fails_the_recapture_instead_of_reporting_it_done() {
    let fx = fixture();
    fx.push_pr(&fx.m1, &["p1.rs"], "v1");
    let review = fx.pr_review(&fx.m1.clone(), None);
    let id = review.id;
    // Control: without the trigger the same recapture succeeds.
    recap(&fx, id, fetch());
    let before = fx.store.get_review_base(id).unwrap().unwrap();

    fx.store.exec_sql_for_test(
        "CREATE TRIGGER boom_base BEFORE UPDATE OF base_status, base_ref, base_mode, \
         base_branch, base_set_by ON reviews \
         BEGIN SELECT RAISE(ABORT, 'boom'); END;",
    );
    let current = fx.refetch(id);
    let err = fx
        .with(|ctx| ctx.recapture(&current, &fetch()))
        .expect_err("a failed base write must not be reported as a finished capture");
    assert_eq!(err.status, 500, "{err:?}");
    assert_eq!(err.urn, URN_CAPTURE_FAILED, "{err:?}");
    assert!(err.message.contains("boom"), "{err:?}");
    let after = fx.store.get_review_base(id).unwrap().unwrap();
    assert_eq!(after.base_status, before.base_status, "nothing was written");
}
