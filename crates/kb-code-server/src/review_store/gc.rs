//! RS-U5 — store-wide ref GC attribution (README §5.4) and the invariant
//! recreate that pairs with it (patchset `ps<n>`/`ps<n>-base` refs, in
//! `super::seed::verify_connectivity`).
//!
//! GC runs across the WHOLE STORE, never per member repo (D2's shared-store
//! rule — the critique this fixes: "shared stores delete another clone's
//! refs"):
//!
//! * `refs/kbc/review/<id>/ps<n>[-base]` is kept iff review `id` exists in
//!   the DB, from WHICHEVER member it belongs to;
//! * `refs/kbc/pr/<n>` and `refs/kbc/prm/<n>` are kept iff some OPEN review
//!   in ANY member binds `(store, n)` — closed-only bindings do not keep
//!   the ref. The legacy per-repo route (`reviews::gc_review_refs`) kept
//!   ANY state, and did so over the requester's own CLONE refs; against a
//!   store it is deliberately NOT a second, looser engine — it delegates
//!   the whole pass to `super::maint::run_gc_pass`, so there is exactly
//!   ONE store-wide keep-set and ONE apply path, whatever route asked.
//! * `refs/remotes/work-<id>/*` (a member's mirrored heads) and the
//!   store-only `refs/kbc/hint/<id>/*` cache are kept iff `id` is a
//!   CURRENTLY REGISTERED member — removed only when unregistered, never
//!   because one member's reviews happen to have closed while another
//!   member's are still open.
//!
//! [`keep_set`] gathers the DB half (pure data, one round trip per
//! member); [`attribute`] is the pure classification (unit-tested with NO
//! git process — every case in README §15.1 is a plain in-memory table);
//! [`delete_candidates`] filters it; `apply` is the one
//! `update-ref --stdin` transaction, each line old-value-guarded so a
//! fetch or capture racing the GC can never be clobbered — call it under
//! the store's `ops` lock (README §5.4/§4.2) and behind an
//! `ApplyGuard`. A ref this module's parser
//! (`crate::reviews::parse_kbc_ref`) or [`work_repo_id`] cannot classify is
//! NEVER a delete candidate (same "never guess" posture as the parser
//! itself).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::git::{GitArgs, GitCall, StoreGit, StoreGitError};
use crate::reviews::{parse_kbc_ref, KbcRef};
use crate::store::{Result as StoreResult, Store};

/// A ref this pass is prepared to delete, with the old value git must see
/// unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcCandidate {
    pub refname: String,
    pub old_oid: String,
}

/// Every store ref this pass looked at, classified. `kind` is
/// [`KbcRef::kind`] for a `refs/kbc/*` name, or `"work"` for a
/// `refs/remotes/work-<id>/*` mirror ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributedStoreRef {
    pub refname: String,
    pub oid: String,
    pub kind: &'static str,
    /// Set for `patchset`/`patchset-base`, and for `pr`/`prm` when an open
    /// review binding is on record (display only — the keep DECISION for
    /// pr/prm is membership in `open_pr_numbers`, not this field).
    pub review_id: Option<i64>,
    /// Set for `work` and `hint` (the member repo id the ref belongs to).
    pub repo_id: Option<i64>,
    /// `"bound"` | `"orphan"`.
    pub status: &'static str,
}

/// The DB half of the keep-set (README §5.4), gathered across every member
/// of ONE store. Pure data — no git I/O.
#[derive(Debug, Clone, Default)]
pub struct GcKeepSet {
    /// Every review id that exists (any state), across every member.
    pub review_ids: BTreeSet<i64>,
    /// PR numbers bound by an OPEN review, across every member.
    pub open_pr_numbers: BTreeSet<u32>,
    /// The first OPEN review id bound to each PR number — display only.
    pub open_pr_review: BTreeMap<u32, i64>,
    /// Repo ids CURRENTLY registered as members of this store.
    pub registered_members: BTreeSet<i64>,
}

/// Gather [`GcKeepSet`] for store `store_id`, whose CURRENTLY REGISTERED
/// member ids are `registered_member_ids`.
///
/// RS-U5 review fix (BLOCKER 1) — this is DB-only, via
/// [`Store::review_ids_for_store`]/[`Store::pr_bound_reviews_for_store`]'s
/// `repo_stores -> repos.name -> reviews.repo` join: it must NEVER resolve
/// member repo NAMEs by filtering a live `[[repos]] config list against
/// `registered_member_ids`, because a member whose `repo_stores` row (and
/// reviews) still exist but has since left config would then be silently
/// excluded — and store-wide GC would delete its still-live review refs as
/// "orphan". `registered_member_ids` itself stays DB-only too (the
/// caller's `Store::store_members(store_id)`), so this function never
/// touches config at all.
pub fn keep_set(
    store: &Store,
    store_id: i64,
    registered_member_ids: &[i64],
) -> StoreResult<GcKeepSet> {
    let review_ids: BTreeSet<i64> = store.review_ids_for_store(store_id)?.into_iter().collect();
    let mut open_pr_numbers = BTreeSet::new();
    let mut open_pr_review = BTreeMap::new();
    for (review_id, pr_number, state) in store.pr_bound_reviews_for_store(store_id)? {
        if state != "open" || !(1..=i64::from(u32::MAX)).contains(&pr_number) {
            continue;
        }
        let n = pr_number as u32;
        open_pr_numbers.insert(n);
        open_pr_review.entry(n).or_insert(review_id);
    }
    Ok(GcKeepSet {
        review_ids,
        open_pr_numbers,
        open_pr_review,
        registered_members: registered_member_ids.iter().copied().collect(),
    })
}

/// `refs/remotes/work-<repo_id>/<branch>` → `repo_id`. `None` for anything
/// else, including `refs/remotes/base/*` (never a GC candidate — it is the
/// credentialed base fetch's own namespace).
pub fn work_repo_id(refname: &str) -> Option<i64> {
    let rest = refname.strip_prefix("refs/remotes/work-")?;
    let (id_s, _branch) = rest.split_once('/')?;
    if id_s.is_empty() || !id_s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let id: i64 = id_s.parse().ok()?;
    (id >= 1 && id_s == id.to_string()).then_some(id)
}

/// Pure: classify every listed ref (`(oid, refname)`, as
/// [`super::seed::list_refs`] returns) against `keep`. Never mutates
/// anything and never guesses: a `refs/kbc/*` name `parse_kbc_ref` rejects,
/// or any other unrecognized ref, is SKIPPED entirely — it never appears in
/// the output, so it can never become a delete candidate either.
pub fn attribute(refs: &[(String, String)], keep: &GcKeepSet) -> Vec<AttributedStoreRef> {
    let mut out = Vec::with_capacity(refs.len());
    for (oid, name) in refs {
        if let Some(parsed) = parse_kbc_ref(name) {
            let (review_id, repo_id, bound) = match &parsed {
                KbcRef::Patchset { review_id, .. } | KbcRef::PatchsetBase { review_id, .. } => {
                    (Some(*review_id), None, keep.review_ids.contains(review_id))
                }
                KbcRef::Pr { number } | KbcRef::Prm { number } => (
                    keep.open_pr_review.get(number).copied(),
                    None,
                    keep.open_pr_numbers.contains(number),
                ),
                KbcRef::Hint { repo_id, .. } => (
                    None,
                    Some(*repo_id),
                    keep.registered_members.contains(repo_id),
                ),
            };
            out.push(AttributedStoreRef {
                refname: parsed.as_refname(),
                oid: oid.clone(),
                kind: parsed.kind(),
                review_id,
                repo_id,
                status: if bound { "bound" } else { "orphan" },
            });
            continue;
        }
        if let Some(repo_id) = work_repo_id(name) {
            out.push(AttributedStoreRef {
                refname: name.clone(),
                oid: oid.clone(),
                kind: "work",
                review_id: None,
                repo_id: Some(repo_id),
                status: if keep.registered_members.contains(&repo_id) {
                    "bound"
                } else {
                    "orphan"
                },
            });
        }
        // Anything else (`refs/remotes/base/*`, a foreign ref this scan
        // was never asked to classify) is silently skipped — not an
        // orphan, not bound, just out of scope for this pass.
    }
    out
}

/// The orphan subset of [`attribute`]'s output, ready for `apply`.
pub fn delete_candidates(attributed: &[AttributedStoreRef]) -> Vec<GcCandidate> {
    attributed
        .iter()
        .filter(|r| r.status == "orphan")
        .map(|r| GcCandidate {
            refname: r.refname.clone(),
            old_oid: r.oid.clone(),
        })
        .collect()
}

/// Proof that the caller ran the three guards [`apply`] cannot run for
/// itself (restore-guard, DB-truth high-water, pre-apply bundle).
///
/// The field is private and the only constructor is [`ApplyGuard::mint`],
/// which is `pub(in crate::review_store)`: a caller OUTSIDE that subtree —
/// `reviews.rs`, the CLI, a downstream crate — cannot mint a token, and
/// `apply` being `pub(crate)` keeps them from reaching it at all. Inside
/// `review_store` the token is a statement of intent the reviewer can read
/// at the one call site that is allowed to make it:
/// [`super::maint::apply_gc_candidates`].
pub(crate) struct ApplyGuard {
    _private: (),
}

impl ApplyGuard {
    pub(in crate::review_store) fn mint() -> Self {
        Self { _private: () }
    }
}

/// Delete every [`GcCandidate`] in ONE `update-ref --stdin` transaction,
/// each line guarded by the old value the scan observed — git refuses the
/// WHOLE transaction if any guard has gone stale (a fetch or a capture
/// landed between [`attribute`] and this call), so a race never deletes a
/// ref that just became live again. Call under the store's `ops` lock.
///
/// Deliberately UN-guarded — there is no bundle, no restore-guard check and
/// no high-water check in here, only the old-value guards — so this is
/// `pub(crate)`, NOT `pub`, and it takes an [`ApplyGuard`] it cannot
/// obtain for itself. The ONLY production minter of that token is
/// [`super::maint::apply_gc_candidates`], which installs all three guards
/// after checking them; an unguarded apply from anywhere outside
/// `review_store` is therefore a COMPILE ERROR, not a convention a later
/// caller can quietly break. The only other minter is `super::seed`'s
/// end-to-end GC test, which drives the transaction directly to keep its
/// delete/sibling-invariance assertions clear of the guard plumbing.
pub(crate) fn apply(
    _guard: &ApplyGuard,
    git: &StoreGit,
    git_dir: &Path,
    delete: &[GcCandidate],
) -> Result<(), StoreGitError> {
    if delete.is_empty() {
        return Ok(());
    }
    let mut tx = String::new();
    for c in delete {
        tx.push_str(&format!("delete {} {}\n", c.refname, c.old_oid));
    }
    git.run(
        GitCall::new("update-ref", GitArgs::new("update-ref").flag("--stdin"))
            .git_dir(git_dir)
            .stdin(tx.into_bytes()),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kb(review_ids: &[i64], open_prs: &[(u32, i64)], members: &[i64]) -> GcKeepSet {
        GcKeepSet {
            review_ids: review_ids.iter().copied().collect(),
            open_pr_numbers: open_prs.iter().map(|(n, _)| *n).collect(),
            open_pr_review: open_prs.iter().copied().collect(),
            registered_members: members.iter().copied().collect(),
        }
    }

    fn sha(b: u8) -> String {
        format!("{b:02x}").repeat(20)
    }

    #[test]
    fn work_repo_id_parses_only_the_shape() {
        assert_eq!(work_repo_id("refs/remotes/work-7/main"), Some(7));
        assert_eq!(work_repo_id("refs/remotes/work-7/release/2026.09"), Some(7));
        for bad in [
            "refs/remotes/work-0/main",
            "refs/remotes/work-07/main",
            "refs/remotes/work-/main",
            "refs/remotes/work-7",
            "refs/remotes/base/main",
            "refs/kbc/pr/7",
        ] {
            assert!(work_repo_id(bad).is_none(), "{bad}");
        }
    }

    /// README §15.1: two members, reviews in both — GC from either never
    /// removes the OTHER member's refs. Review ids are globally unique, so
    /// "reviews in both" really means "the keep-set the whole store shares
    /// contains both members' review ids".
    #[test]
    fn gc_never_removes_another_members_refs() {
        let keep = kb(&[1, 2], &[], &[10, 20]);
        let refs = vec![
            ("a".repeat(40), "refs/kbc/review/1/ps1".to_string()),
            ("b".repeat(40), "refs/kbc/review/2/ps1".to_string()),
            (sha(1), "refs/remotes/work-10/main".to_string()),
            (sha(2), "refs/remotes/work-20/main".to_string()),
        ];
        let attributed = attribute(&refs, &keep);
        assert!(
            attributed.iter().all(|r| r.status == "bound"),
            "{attributed:?}"
        );
        assert!(delete_candidates(&attributed).is_empty());
    }

    /// A review that belongs to neither member is gone from the DB — its
    /// refs are orphan, but the OTHER member's own review stays bound.
    #[test]
    fn a_deleted_review_is_orphan_without_touching_a_sibling_reviews_refs() {
        let keep = kb(&[2], &[], &[10, 20]);
        let refs = vec![
            ("a".repeat(40), "refs/kbc/review/1/ps1".to_string()),
            ("a".repeat(40), "refs/kbc/review/1/ps1-base".to_string()),
            ("b".repeat(40), "refs/kbc/review/2/ps1".to_string()),
        ];
        let attributed = attribute(&refs, &keep);
        let del: BTreeSet<String> = delete_candidates(&attributed)
            .into_iter()
            .map(|c| c.refname)
            .collect();
        assert_eq!(
            del,
            BTreeSet::from([
                "refs/kbc/review/1/ps1".to_string(),
                "refs/kbc/review/1/ps1-base".to_string(),
            ])
        );
        assert!(!del.contains("refs/kbc/review/2/ps1"));
    }

    /// pr/<n> is kept while ANY open review in ANY member binds it, and
    /// dropped once every review binding it is closed.
    #[test]
    fn pr_ref_kept_while_any_open_review_binds_it_dropped_once_all_closed() {
        let keep_open = kb(&[5], &[(42, 5)], &[10]);
        let refs = vec![
            ("a".repeat(40), "refs/kbc/pr/42".to_string()),
            ("a".repeat(40), "refs/kbc/prm/42".to_string()),
        ];
        let attributed = attribute(&refs, &keep_open);
        assert!(
            attributed.iter().all(|r| r.status == "bound"),
            "{attributed:?}"
        );
        assert_eq!(attributed[0].review_id, Some(5));

        // Every review binding 42 is now closed: the keep-set carries no
        // open_pr_numbers entry for it at all (this is exactly what
        // `keep_set` computes: `list_pr_bound_reviews` still returns the
        // closed row, but the `state != "open"` filter drops it).
        let keep_closed = kb(&[5], &[], &[10]);
        let attributed = attribute(&refs, &keep_closed);
        assert!(
            attributed.iter().all(|r| r.status == "orphan"),
            "{attributed:?}"
        );
        assert_eq!(delete_candidates(&attributed).len(), 2);
    }

    /// `work-<id>/*` and the `hint/<id>/*` cache are removed ONLY when the
    /// member is unregistered — never because that member's own reviews
    /// closed (which would show up as a smaller `review_ids`/
    /// `open_pr_numbers`, not a smaller `registered_members`).
    #[test]
    fn work_and_hint_refs_survive_review_closure_and_die_on_unregister() {
        let keep_registered = kb(&[], &[], &[10]);
        let refs = vec![
            (sha(1), "refs/remotes/work-10/main".to_string()),
            (sha(2), "refs/kbc/hint/10/main".to_string()),
        ];
        let attributed = attribute(&refs, &keep_registered);
        assert!(
            attributed.iter().all(|r| r.status == "bound"),
            "still a member: {attributed:?}"
        );

        let keep_unregistered = kb(&[], &[], &[]);
        let attributed = attribute(&refs, &keep_unregistered);
        assert!(
            attributed.iter().all(|r| r.status == "orphan"),
            "unregistered: {attributed:?}"
        );
        let del = delete_candidates(&attributed);
        assert_eq!(del.len(), 2);
    }

    #[test]
    fn refs_this_scan_cannot_classify_are_never_delete_candidates() {
        let keep = kb(&[], &[], &[]);
        let refs = vec![
            (sha(1), "refs/remotes/base/main".to_string()),
            (sha(2), "refs/heads/stray".to_string()),
        ];
        let attributed = attribute(&refs, &keep);
        assert!(attributed.is_empty(), "{attributed:?}");
    }

    /// RS-U5 review fix (BLOCKER 1), DB-only regression: `keep_set` must
    /// never depend on a live `[[repos]]` config list. Model "member B
    /// removed from config" by never mentioning config at all — two repos,
    /// both members of one store via `repo_stores`, each with a review;
    /// `keep_set` must see BOTH review ids from the DB alone, and the
    /// resulting GC decision must keep both members' refs.
    #[test]
    fn keep_set_sees_a_members_reviews_even_when_absent_from_config() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(&tmp.path().join("kbc.sqlite")).unwrap();
        let a = store.upsert_repo("widgets-a", "/a").unwrap();
        let b = store.upsert_repo("widgets-b", "/b").unwrap();
        let store_id = store
            .create_review_store(
                "22222222-2222-4222-8222-222222222222",
                "github.com/acme/widgets",
                "/store.git",
                None,
                None,
                1,
            )
            .unwrap();
        store.add_repo_to_store(a, store_id).unwrap();
        store.add_repo_to_store(b, store_id).unwrap();
        let review_a = store
            .create_review("widgets-a", None, "main", "x", None, 1)
            .unwrap();
        let review_b = store
            .create_review("widgets-b", None, "main", "y", None, 1)
            .unwrap();
        let member_ids = store.store_members(store_id).unwrap();
        let keep = keep_set(&store, store_id, &member_ids).unwrap();
        assert!(keep.review_ids.contains(&review_a), "{:?}", keep.review_ids);
        assert!(
            keep.review_ids.contains(&review_b),
            "member B's review must be in the keep-set even though nothing \
             here ever named B via config: {:?}",
            keep.review_ids
        );
        let refs = vec![
            ("a".repeat(40), format!("refs/kbc/review/{review_a}/ps1")),
            ("b".repeat(40), format!("refs/kbc/review/{review_b}/ps1")),
        ];
        let attributed = attribute(&refs, &keep);
        assert!(
            attributed.iter().all(|r| r.status == "bound"),
            "{attributed:?}"
        );
    }
}
