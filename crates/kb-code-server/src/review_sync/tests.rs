//! RS-U10b — `review sync` / `review status` tests. The first half is PURE
//! (the reason vocabulary, the dry-run prediction, dates, the body shape,
//! `head_moved`); the second half drives a READY review store over
//! hermetic fixtures — a local bare "forge" (synthetic acme/widgets,
//! `refs/pull/<n>/head` pushed by hand), one member clone, a temp DB — and
//! maps each re-capture onto the sync reason exactly as `sync_one` does
//! (through `reason_from_envelope` over the reuse envelope's own fields),
//! with the GitHub-equal file count. No network. Test-only direct `git`
//! spawns build the fixtures (this file is in SEC-17's
//! `GIT_SPAWNING_FILES`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;
use crate::config::{RepoEntry, ReviewSection};
use crate::git::roots::WorkTreeRoot;
use crate::review_base::capture::{CaptureOpts, Member, NewReview, Recapture, StoreCtx};
use crate::review_base::{BasePolicy, BaseSource, EffectiveBase, SetBy};
use crate::review_store::registry::{Registration, ReviewStores};
use crate::store::Store;
use kb_core::events::EventBus;

// =====================================================================
// pure
// =====================================================================

#[test]
fn capture_answers_map_onto_the_closed_reason_set() {
    use SyncReason::*;
    let rows: &[(bool, Option<&str>, bool, SyncReason)] = &[
        (false, None, false, Unchanged),
        // An unminted capture reports the LATEST patchset's old kind — it
        // must never read as a move.
        (false, Some("rebase"), false, Unchanged),
        (false, Some("push"), false, Unchanged),
        (true, Some("push"), false, HeadMoved),
        (true, Some("rebase"), false, HeadMoved),
        (true, Some("initial"), false, HeadMoved),
        (true, Some("base-moved"), false, BaseMoved),
        (true, Some("base-corrected"), false, BaseMoved),
        (true, Some("retarget"), false, Retargeted),
        // A followed retarget whose merge-base left the pair unchanged.
        (false, Some("push"), true, Retargeted),
    ];
    for (minted, kind, retargeted, want) in rows {
        assert_eq!(
            SyncReason::from_capture(*minted, *kind, *retargeted),
            *want,
            "{minted} {kind:?} {retargeted}"
        );
    }
    let all = [
        Created,
        HeadMoved,
        BaseMoved,
        Retargeted,
        Unchanged,
        MergedFinal,
    ];
    let names: Vec<&str> = all.iter().map(|r| r.as_str()).collect();
    assert_eq!(
        names,
        [
            "created",
            "head-moved",
            "base-moved",
            "retargeted",
            "unchanged",
            "merged-final"
        ]
    );
}

#[test]
fn the_reuse_envelope_names_its_reason() {
    let reused = |minted: bool, kind: &str, warnings: Value| json!({ "reused": true, "minted": minted, "kind": kind, "warnings": warnings });
    assert_eq!(
        reason_from_envelope(StatusCode::CREATED, &json!({ "minted": true })),
        SyncReason::Created
    );
    assert_eq!(
        reason_from_envelope(StatusCode::OK, &reused(false, "initial", json!([]))),
        SyncReason::Unchanged
    );
    assert_eq!(
        reason_from_envelope(StatusCode::OK, &reused(true, "rebase", json!([]))),
        SyncReason::HeadMoved
    );
    assert_eq!(
        reason_from_envelope(
            StatusCode::OK,
            &reused(
                false,
                "initial",
                json!([{ "code": "retargeted", "message": "…" }])
            )
        ),
        SyncReason::Retargeted
    );
    // Another warning never reads as a retarget.
    assert_eq!(
        reason_from_envelope(
            StatusCode::OK,
            &reused(
                false,
                "initial",
                json!([{ "code": "pr-target-differs", "message": "…" }])
            )
        ),
        SyncReason::Unchanged
    );
}

#[test]
fn dry_run_predicts_from_the_forge_answer() {
    use SyncReason::*;
    let (a, b) = ("a".repeat(40), "b".repeat(40));
    let (a, b) = (a.as_str(), b.as_str());
    let p = |tip: Option<&str>, head: Option<&str>, set_by: &str, fb: Option<&str>| {
        SyncReason::predict(tip, head, Some("track"), Some("main"), set_by, fb)
    };
    assert_eq!(p(Some(a), Some(a), "auto", Some("main")), Unchanged);
    assert_eq!(p(Some(a), Some(b), "auto", Some("main")), HeadMoved);
    assert_eq!(p(Some(a), Some(a), "auto", Some("release")), Retargeted);
    // A person set the base: the daemon would not follow the retarget.
    assert_eq!(p(Some(a), Some(a), "user", Some("release")), Unchanged);
    // No forge answer: nothing to compare with.
    assert_eq!(p(Some(a), None, "auto", None), Unchanged);
    assert_eq!(
        SyncReason::predict(Some(a), Some(a), Some("pin"), None, "legacy", Some("main")),
        Unchanged
    );
}

#[test]
fn head_moved_compares_the_remote_head_with_the_latest_tip() {
    let (a, b) = ("a".repeat(40), "b".repeat(40));
    let (a, b) = (a.as_str(), b.as_str());
    assert_eq!(head_moved(Some(a), Some(a)), Some(false));
    assert_eq!(head_moved(Some(b), Some(a)), Some(true));
    assert_eq!(head_moved(None, Some(a)), None);
    assert_eq!(head_moved(Some(a), None), None);
}

#[test]
fn merged_since_accepts_a_date_or_rfc3339_and_filters_inclusively() {
    let day = parse_since("2026-09-24").unwrap();
    assert_eq!(day, parse_since("2026-09-24T00:00:00Z").unwrap());
    assert_eq!(parse_since(" 2026-09-24 ").unwrap(), day);
    assert!(parse_since("yesterday").is_err());
    assert!(parse_since("2026-13-01").is_err());
    assert!(merged_on_or_after(Some("2026-09-24T00:00:00Z"), day));
    assert!(merged_on_or_after(Some("2026-09-25T08:30:00Z"), day));
    assert!(!merged_on_or_after(Some("2026-09-23T23:59:59Z"), day));
    assert!(!merged_on_or_after(None, day));
    assert!(!merged_on_or_after(Some("not a date"), day));
}

fn body(v: Value) -> SyncBody {
    serde_json::from_value(v).unwrap()
}

#[test]
fn the_sync_body_takes_exactly_one_target() {
    assert!(validate_sync_body(&body(json!({ "repo": "r", "pr_number": 7 }))).is_ok());
    assert!(validate_sync_body(&body(json!({ "repo": "r", "open": true }))).is_ok());
    assert!(validate_sync_body(&body(
        json!({ "repo": "r", "open": true, "merged_since": "2026-09-24" })
    ))
    .is_ok());
    for bad in [
        json!({ "repo": "r" }),
        json!({ "repo": "r", "pr_number": 7, "open": true }),
        json!({ "repo": "r", "pr_number": 7, "merged_since": "2026-09-24" }),
        json!({ "repo": "r", "open": true, "merged_since": "soon" }),
        json!({ "repo": "r", "open": true, "base_ref": "main" }),
    ] {
        assert!(validate_sync_body(&body(bad.clone())).is_err(), "{bad}");
    }
    // A negative or non-numeric PR never deserializes at all.
    assert!(serde_json::from_value::<SyncBody>(json!({ "repo": "r", "pr_number": -1 })).is_err());
}

#[test]
fn the_forge_block_folds_merged_into_state() {
    let pr = PrSyncOut {
        number: 7,
        title: "Add à feature".into(),
        state: "closed".into(),
        merged: true,
        merged_at: Some("2026-09-24T10:00:00Z".into()),
        head_sha: "c".repeat(40),
        head_ref: "feature".into(),
        base_ref: "main".into(),
        changed_files: Some(3),
    };
    let f = ForgeOut::from_pr(&pr);
    assert!(f.available && f.is_merged() && !f.is_closed_unmerged());
    assert_eq!(f.changed_files, Some(3));
    let closed = ForgeOut::from_pr(&PrSyncOut {
        merged: false,
        merged_at: None,
        ..pr.clone()
    });
    assert!(closed.is_closed_unmerged() && !closed.is_merged());
    let none = ForgeOut::unavailable("not-github");
    assert!(!none.available && !none.is_merged());
    let v = json!(none);
    for k in [
        "available",
        "state",
        "changed_files",
        "title",
        "base_ref",
        "head_sha",
        "merged_at",
        "unavailable",
    ] {
        assert!(v.get(k).is_some(), "forge block lacks {k}: {v}");
    }
}

#[test]
fn the_status_route_is_declared_with_no_required_params() {
    for c in RS_U10B_ROUTES {
        assert!(c.path.starts_with("/api/"));
        assert!((c.params_accept_without)(""), "{}", c.path);
        assert!(c.required_params.is_empty());
    }
    assert!(ReviewStatusParams {
        fetch: Some("1".into())
    }
    .wants_fetch());
    assert!(!ReviewStatusParams::default().wants_fetch());
}

// =====================================================================
// store fixtures (same shape as `review_base::tests`)
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

struct Fx {
    _tmp: tempfile::TempDir,
    clone: PathBuf,
    author: PathBuf,
    store: Store,
    rs: ReviewStores,
    bus: EventBus,
    m1: String,
}

const REPO: &str = "widgets";
const PR: u32 = 7;

fn fixture() -> Fx {
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
    fn with<T>(&self, f: impl FnOnce(&StoreCtx<'_>) -> T) -> T {
        let h = self
            .rs
            .handle_for_repo(&self.store, REPO)
            .expect("the store is ready");
        let r = self.rs.repo(REPO).unwrap();
        let ctx = StoreCtx {
            rs: &self.rs,
            store: &self.store,
            bus: &self.bus,
            handle: &h,
            member: Member {
                id: r.id,
                name: r.name.clone(),
                root: r.root.clone(),
            },
            max_patchsets: 50,
        };
        f(&ctx)
    }

    /// Author PR commits off `from`, force-push them as the forge's
    /// `refs/pull/<PR>/head`, return the tip.
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

    fn advance_main(&self, n: usize, tag: &str) -> String {
        git(&self.author, &["checkout", "-q", "main"]);
        let mut tip = String::new();
        for i in 0..n {
            tip = commit(&self.author, &format!("main-{tag}-{i}.txt"), tag);
        }
        git(&self.author, &["push", "-q", "origin", "main"]);
        tip
    }

    fn clone_refs(&self) -> String {
        git(
            &self.clone,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        )
    }

    /// Create the PR review the way `create_review_pr_in_store` does: the
    /// chain with the forge's `base.ref`, then ps1.
    fn create(&self) -> ReviewRow {
        let prepared = self
            .with(|c| {
                c.prepare_new(&NewReview {
                    pr: Some(PR),
                    caller_base_ref: Some("main".into()),
                    ..NewReview::default()
                })
            })
            .unwrap();
        let id = self
            .store
            .create_review(
                REPO,
                Some("t"),
                &prepared.base_ref,
                &crate::reviews::pr_ref(PR),
                None,
                1,
            )
            .unwrap();
        self.store
            .set_review_pr_binding(id, i64::from(PR), "acme/widgets", None, None, None)
            .unwrap();
        let p = &prepared.policy;
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
        let review = self.store.get_review(id).unwrap().unwrap();
        self.with(|c| {
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
        review
    }

    /// One sync of an existing review, exactly as the reuse path runs it
    /// (fetch + recapture with the forge's `base.ref`), reduced to the
    /// fields `sync_one` reports: reason (through the reuse ENVELOPE's own
    /// `minted`/`kind`/`warnings`), the latest pair, and the file count.
    fn sync(&self, id: i64, forge_base: &str) -> (SyncReason, bool, String, String, usize) {
        let review = self.store.get_review(id).unwrap().unwrap();
        let r = self
            .with(|c| {
                c.recapture(
                    &review,
                    &Recapture {
                        network: true,
                        forge_base_ref: Some(forge_base.into()),
                        ..Recapture::default()
                    },
                )
            })
            .unwrap_or_else(|e| panic!("recapture: {e}"));
        let envelope = json!({
            "reused": true,
            "minted": r.outcome.minted,
            "kind": r.outcome.kind,
            "warnings": r.warnings,
        });
        let reason = reason_from_envelope(StatusCode::OK, &envelope);
        assert_eq!(
            reason,
            SyncReason::from_capture(r.outcome.minted, r.outcome.kind.as_deref(), r.retargeted),
            "the envelope and the capture agree"
        );
        let ctx = GitCtx::for_repo(&self.store, REPO, WorkTreeRoot::user_clone(&self.clone));
        assert!(!ctx.is_fallback(), "the count reads the review store");
        let files = reviews::files_changed(&ctx, &r.outcome.ps.base_sha, &r.outcome.ps.tip_sha)
            .expect("files")
            .len();
        (
            reason,
            r.outcome.minted,
            r.outcome.ps.tip_sha.clone(),
            r.outcome.ps.base_sha.clone(),
            files,
        )
    }
}

// =====================================================================
// store-path sync semantics
// =====================================================================

/// Twice with no upstream change → the second mints nothing (`unchanged`).
/// The base branch merely advancing → still `unchanged` (the merge-base of
/// an un-rebased head does not move). A push → `head-moved`. A rebase onto
/// the new main → `head-moved` with EXACTLY the PR's own files (what
/// GitHub's `changed_files` reports), never main's.
#[test]
fn sync_is_idempotent_and_follows_pushes_and_rebases_github_equal() {
    let fx = fixture();
    let before = fx.clone_refs();
    let tip0 = fx.push_pr(&fx.m1, &["p1.rs", "p2.rs"], "v1");
    let review = fx.create();

    let (reason, minted, tip, base, files) = fx.sync(review.id, "main");
    assert_eq!((reason, minted), (SyncReason::Unchanged, false));
    assert_eq!(
        (tip.as_str(), base.as_str()),
        (tip0.as_str(), fx.m1.as_str())
    );
    assert_eq!(files, 2);
    let (reason, minted, ..) = fx.sync(review.id, "main");
    assert_eq!((reason, minted), (SyncReason::Unchanged, false));

    // main advances; the PR is not rebased → nothing to capture.
    let main2 = fx.advance_main(3, "k");
    let (reason, minted, _, base, files) = fx.sync(review.id, "main");
    assert_eq!((reason, minted), (SyncReason::Unchanged, false));
    assert_eq!(base, fx.m1, "the merge-base did not move");
    assert_eq!(files, 2);

    // A push on the PR head.
    git(&fx.author, &["checkout", "-q", "pr"]);
    let tip1 = commit(&fx.author, "p3.rs", "p3");
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
    let (reason, minted, tip, _, files) = fx.sync(review.id, "main");
    assert_eq!((reason, minted), (SyncReason::HeadMoved, true));
    assert_eq!(tip, tip1);
    assert_eq!(files, 3);

    // Rebased onto the advanced main: GitHub-equal — the PR's 3 files,
    // not main's 3 new ones.
    let tip2 = fx.push_pr(&main2, &["p1.rs", "p2.rs", "p3.rs"], "v2");
    let (reason, minted, tip, base, files) = fx.sync(review.id, "main");
    assert_eq!((reason, minted), (SyncReason::HeadMoved, true));
    assert_eq!(
        (tip.as_str(), base.as_str()),
        (tip2.as_str(), main2.as_str())
    );
    assert_eq!(files, 3);
    let (reason, minted, ..) = fx.sync(review.id, "main");
    assert_eq!((reason, minted), (SyncReason::Unchanged, false));

    // The user clone was never written.
    assert_eq!(fx.clone_refs(), before);
}

/// The forge says the PR now targets `release` (set_by=auto) → followed,
/// `retargeted`, and the policy is persisted.
#[test]
fn a_retarget_the_review_follows_is_reported_as_retargeted() {
    let fx = fixture();
    fx.push_pr(&fx.m1, &["a.rs"], "v1");
    let review = fx.create();
    let b = fx.store.get_review_base(review.id).unwrap().unwrap();
    assert_eq!(
        (b.base_branch.as_deref(), b.base_set_by.as_str()),
        (Some("main"), "auto")
    );
    // An older release branch the PR is retargeted onto.
    git(&fx.author, &["checkout", "-q", "-B", "rel", &fx.m1]);
    git(&fx.author, &["reset", "-q", "--hard", "HEAD~1"]);
    git(
        &fx.author,
        &["push", "-q", "origin", "HEAD:refs/heads/release"],
    );
    git(&fx.author, &["checkout", "-q", "main"]);
    let (reason, ..) = fx.sync(review.id, "release");
    assert_eq!(reason, SyncReason::Retargeted);
    let b = fx.store.get_review_base(review.id).unwrap().unwrap();
    assert_eq!(b.base_branch.as_deref(), Some("release"));
    // Re-syncing against the same target is quiet again.
    let (reason, minted, ..) = fx.sync(review.id, "release");
    assert_eq!((reason, minted), (SyncReason::Unchanged, false));
}

/// A person-set base is never retargeted: the sync stays `unchanged` and
/// the envelope carries `pr-target-differs`.
#[test]
fn a_user_set_base_is_not_reported_as_retargeted() {
    let fx = fixture();
    fx.push_pr(&fx.m1, &["a.rs"], "v1");
    let review = fx.create();
    let user = BasePolicy::track("main", SetBy::User, BaseSource::Explicit);
    fx.store
        .set_review_base(
            review.id,
            user.mode.as_str(),
            user.branch.as_deref(),
            user.member,
            user.set_by.as_str(),
            None,
        )
        .unwrap();
    git(
        &fx.author,
        &[
            "push",
            "-q",
            "origin",
            &format!("{}:refs/heads/release", fx.m1),
        ],
    );
    let (reason, minted, ..) = fx.sync(review.id, "release");
    assert_eq!((reason, minted), (SyncReason::Unchanged, false));
}
