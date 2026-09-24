//! RS-U3 — the base-URL ladder and store membership (README §5.1).
//!
//! Runs ONCE per repo, at registration, and decides which forge project
//! (`store_key`) the repo's reviews live in:
//!
//! 1. explicit input (`store set-base-url`);
//! 2. `[[review.repos]] base_url`;
//! 3. the `owner/name` slug shared by the repo's existing PR bindings
//!    (`reviews.pr_repo_slug`, recorded from the origin at `start-pr` bind
//!    time) — matched against the repo's own remotes, so the host comes
//!    from a real remote, never a guess;
//! 4. the remote `gh repo set-default` marked (`remote.<R>.gh-resolved =
//!    base`);
//! 5. the repo's single forge remote;
//! 6. `upstream`, only when the forge API verifies `origin` is a fork of
//!    it ([`ForkCheck`]; the daemon's own implementation is
//!    [`NoForkCheck`] until the api slot is wired — see its doc);
//! 7. otherwise REFUSE with `base-url-ambiguous`. No guessing.
//!
//! **Membership** (README §5.1 "Joining"): before rungs 3–7 run, a repo
//! whose remote normalizes to an EXISTING store's key joins that store.
//! Rungs 1–2 are operator statements and run first — an operator who
//! wrote `base_url` means it, even if some other remote also happens to
//! match a store. When more than one remote matches existing stores, the
//! ladder decides among them (its answer then joins the matching store).
//!
//! Pure: every input is passed in, so every rung is unit-testable
//! without git, a DB, or the network.

use super::key::{key_matches_slug, store_key_for_url};

/// One `remote.<name>` of a member clone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteInfo {
    pub name: String,
    /// `remote.<name>.url` as written — may carry userinfo; it is only
    /// ever normalized ([`store_key_for_url`]), never echoed.
    pub url: String,
    /// `remote.<name>.gh-resolved` (`base` when `gh repo set-default`
    /// chose it).
    pub gh_resolved: Option<String>,
}

impl RemoteInfo {
    fn key(&self) -> Option<String> {
        store_key_for_url(&self.url)
    }
}

/// Where the base URL came from — persisted as `review_stores.base_url_source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseUrlSource {
    Explicit,
    Config,
    /// Joined an existing store whose key one of the repo's remotes has.
    Member,
    PrSlug,
    GhResolved,
    Single,
    UpstreamVerified,
    /// No forge remote at all: a `local:` store.
    Local,
}

impl BaseUrlSource {
    pub fn slug(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Config => "config",
            Self::Member => "member",
            Self::PrSlug => "pr-slug",
            Self::GhResolved => "gh-resolved",
            Self::Single => "single",
            Self::UpstreamVerified => "upstream-verified",
            Self::Local => "local",
        }
    }
}

/// Rung 6's API check: is `origin_key`'s project a fork of `upstream_key`'s?
pub trait ForkCheck {
    /// `Some(true)` verified fork, `Some(false)` verified NOT a fork,
    /// `None` = could not ask (no api credential, offline, unsupported
    /// forge) — treated as "rung does not apply".
    fn origin_is_fork_of(&self, origin_key: &str, upstream_key: &str) -> Option<bool>;
}

/// The daemon's rung-6 implementation in RS-U3: always "could not ask".
///
/// Verifying a fork needs a forge API GET (`/repos/{origin}` → `parent`)
/// through the api credential slot, which `github.rs`'s REST ladder is
/// rewired onto in a later unit. Until then rung 6 is honestly
/// unavailable — never assumed — and a repo that would need it refuses
/// with `base-url-ambiguous`, which the operator resolves in one line of
/// config (`base_url`) or `store set-base-url`.
pub struct NoForkCheck;

impl ForkCheck for NoForkCheck {
    fn origin_is_fork_of(&self, _: &str, _: &str) -> Option<bool> {
        None
    }
}

/// Everything the ladder reads.
pub struct LadderInput<'a> {
    pub explicit: Option<&'a str>,
    pub config_base_url: Option<&'a str>,
    /// Distinct non-null `reviews.pr_repo_slug` values for this repo.
    pub pr_slugs: &'a [String],
    pub remotes: &'a [RemoteInfo],
    /// `store_key` of every store that already exists.
    pub existing_keys: &'a [String],
    pub fork_check: &'a dyn ForkCheck,
}

/// The ladder's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LadderOutcome {
    /// A forge project. `url` is the raw URL the key came from (an
    /// explicit/config value or a remote's url) — the caller derives the
    /// canonical transport URL from it, never stores it raw.
    Resolved {
        store_key: String,
        url: String,
        source: BaseUrlSource,
        /// The remote the answer came from (rungs 3–6, membership).
        remote: Option<String>,
    },
    /// No forge remote at all → a `local:` store.
    NoForgeRemote,
    /// Refused. `code` is a stable slug (`base-url-ambiguous`,
    /// `base-url-invalid`).
    Refused {
        code: &'static str,
        reason: String,
        candidates: Vec<String>,
    },
}

/// Stable refusal slug for rung 7.
pub const BASE_URL_AMBIGUOUS: &str = "base-url-ambiguous";
/// An explicit/config `base_url` that does not normalize to a forge key.
pub const BASE_URL_INVALID: &str = "base-url-invalid";

/// Run the ladder. See the module doc.
pub fn resolve(input: &LadderInput<'_>) -> LadderOutcome {
    // Rungs 1-2: operator statements.
    for (v, source, what) in [
        (input.explicit, BaseUrlSource::Explicit, "explicit base url"),
        (
            input.config_base_url,
            BaseUrlSource::Config,
            "[[review.repos]] base_url",
        ),
    ] {
        if let Some(url) = v {
            return match store_key_for_url(url) {
                Some(store_key) => LadderOutcome::Resolved {
                    store_key,
                    url: url.to_string(),
                    source,
                    remote: None,
                },
                None => LadderOutcome::Refused {
                    code: BASE_URL_INVALID,
                    reason: format!("the {what} is not a forge project URL"),
                    candidates: vec![],
                },
            };
        }
    }

    let forge: Vec<(&RemoteInfo, String)> = input
        .remotes
        .iter()
        .filter_map(|r| r.key().map(|k| (r, k)))
        .collect();
    if forge.is_empty() {
        return LadderOutcome::NoForgeRemote;
    }
    let resolved = |r: &RemoteInfo, k: &str, source| LadderOutcome::Resolved {
        store_key: k.to_string(),
        url: r.url.clone(),
        source,
        remote: Some(r.name.clone()),
    };

    // Membership: exactly one distinct existing key among the remotes.
    let mut member_keys: Vec<&String> = forge
        .iter()
        .map(|(_, k)| k)
        .filter(|k| input.existing_keys.iter().any(|e| e == *k))
        .collect();
    member_keys.sort();
    member_keys.dedup();
    if member_keys.len() == 1 {
        let key = member_keys[0];
        let (r, k) = forge.iter().find(|(_, k)| k == key).expect("present");
        return resolved(r, k, BaseUrlSource::Member);
    }

    // Rung 3: the slug every PR binding of this repo agrees on.
    let mut slugs: Vec<&str> = input.pr_slugs.iter().map(|s| s.trim()).collect();
    slugs.sort_unstable();
    slugs.dedup();
    if let [slug] = slugs.as_slice() {
        let mut hits: Vec<&String> = forge
            .iter()
            .map(|(_, k)| k)
            .filter(|k| key_matches_slug(k, slug))
            .collect();
        hits.sort();
        hits.dedup();
        if let [hit] = hits.as_slice() {
            let (r, k) = forge.iter().find(|(_, k)| k == *hit).expect("present");
            return resolved(r, k, BaseUrlSource::PrSlug);
        }
    }

    // Rung 4: `gh repo set-default`.
    let gh: Vec<&(&RemoteInfo, String)> = forge
        .iter()
        .filter(|(r, _)| r.gh_resolved.as_deref() == Some("base"))
        .collect();
    if let [(r, k)] = gh.as_slice() {
        return resolved(r, k, BaseUrlSource::GhResolved);
    }

    // Rung 5: one forge project among the remotes (several remote names
    // for the same project still count as one).
    let mut distinct: Vec<&String> = forge.iter().map(|(_, k)| k).collect();
    distinct.sort();
    distinct.dedup();
    if distinct.len() == 1 {
        let (r, k) = &forge[0];
        return resolved(r, k, BaseUrlSource::Single);
    }

    // Rung 6: upstream, only when the API verifies origin forks it.
    let by_name = |n: &str| forge.iter().find(|(r, _)| r.name == n);
    if let (Some((_, ok)), Some((ur, uk))) = (by_name("origin"), by_name("upstream")) {
        if input.fork_check.origin_is_fork_of(ok, uk) == Some(true) {
            return resolved(ur, uk, BaseUrlSource::UpstreamVerified);
        }
    }

    // Rung 7.
    let candidates: Vec<String> = distinct.into_iter().cloned().collect();
    LadderOutcome::Refused {
        code: BASE_URL_AMBIGUOUS,
        reason: format!(
            "{} remotes name {} different forge projects and no rung decides between them; \
             set [[review.repos]] base_url or run `kb-code store set-base-url`",
            forge.len(),
            candidates.len()
        ),
        candidates,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(name: &str, url: &str) -> RemoteInfo {
        RemoteInfo {
            name: name.into(),
            url: url.into(),
            gh_resolved: None,
        }
    }

    struct Fork(Option<bool>);
    impl ForkCheck for Fork {
        fn origin_is_fork_of(&self, _: &str, _: &str) -> Option<bool> {
            self.0
        }
    }

    fn run(
        explicit: Option<&str>,
        config: Option<&str>,
        slugs: &[&str],
        remotes: &[RemoteInfo],
        existing: &[&str],
        fork: Option<bool>,
    ) -> LadderOutcome {
        let slugs: Vec<String> = slugs.iter().map(|s| s.to_string()).collect();
        let existing: Vec<String> = existing.iter().map(|s| s.to_string()).collect();
        resolve(&LadderInput {
            explicit,
            config_base_url: config,
            pr_slugs: &slugs,
            remotes,
            existing_keys: &existing,
            fork_check: &Fork(fork),
        })
    }

    fn source(o: &LadderOutcome) -> BaseUrlSource {
        match o {
            LadderOutcome::Resolved { source, .. } => *source,
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    fn key(o: &LadderOutcome) -> &str {
        match o {
            LadderOutcome::Resolved { store_key, .. } => store_key,
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    /// The two-remote shape of a working clone with a personal fork.
    fn fork_pair() -> Vec<RemoteInfo> {
        vec![
            remote("origin", "git@github.com:acme/widgets.git"),
            remote("mine", "git@github.com:someone/widgets.git"),
        ]
    }

    #[test]
    fn rung1_explicit_beats_everything() {
        let o = run(
            Some("https://github.com/acme/widgets.git"),
            Some("https://github.com/other/thing.git"),
            &["someone/widgets"],
            &fork_pair(),
            &["github.com/someone/widgets"],
            Some(true),
        );
        assert_eq!(source(&o), BaseUrlSource::Explicit);
        assert_eq!(key(&o), "github.com/acme/widgets");
    }

    #[test]
    fn rung2_config_beats_membership_and_remotes() {
        let o = run(
            None,
            Some("https://github.com/acme/widgets"),
            &[],
            &fork_pair(),
            &["github.com/someone/widgets"],
            None,
        );
        assert_eq!(source(&o), BaseUrlSource::Config);
        assert_eq!(key(&o), "github.com/acme/widgets");
    }

    #[test]
    fn an_invalid_operator_url_refuses_rather_than_falling_through() {
        let o = run(None, Some("/srv/git/widgets"), &[], &fork_pair(), &[], None);
        assert!(matches!(
            o,
            LadderOutcome::Refused {
                code: BASE_URL_INVALID,
                ..
            }
        ));
    }

    #[test]
    fn membership_joins_the_one_existing_store() {
        let o = run(
            None,
            None,
            &[],
            &fork_pair(),
            &["github.com/acme/widgets"],
            None,
        );
        assert_eq!(source(&o), BaseUrlSource::Member);
        assert_eq!(key(&o), "github.com/acme/widgets");
    }

    #[test]
    fn two_existing_matches_fall_to_the_ladder() {
        let o = run(
            None,
            None,
            &["acme/widgets"],
            &fork_pair(),
            &["github.com/acme/widgets", "github.com/someone/widgets"],
            None,
        );
        assert_eq!(source(&o), BaseUrlSource::PrSlug);
        assert_eq!(key(&o), "github.com/acme/widgets");
    }

    #[test]
    fn rung3_pr_slug_picks_the_matching_remote() {
        let o = run(
            None,
            None,
            &["Acme/Widgets", "Acme/Widgets"],
            &fork_pair(),
            &[],
            None,
        );
        assert_eq!(source(&o), BaseUrlSource::PrSlug);
        match &o {
            LadderOutcome::Resolved { remote, .. } => assert_eq!(remote.as_deref(), Some("origin")),
            _ => unreachable!(),
        }
    }

    #[test]
    fn rung3_disagreeing_slugs_do_not_decide() {
        let o = run(
            None,
            None,
            &["acme/widgets", "someone/widgets"],
            &fork_pair(),
            &[],
            None,
        );
        assert!(matches!(
            o,
            LadderOutcome::Refused {
                code: BASE_URL_AMBIGUOUS,
                ..
            }
        ));
    }

    #[test]
    fn rung3_slug_with_no_matching_remote_does_not_decide() {
        let o = run(None, None, &["elsewhere/widgets"], &fork_pair(), &[], None);
        assert!(matches!(o, LadderOutcome::Refused { .. }));
    }

    #[test]
    fn rung4_gh_resolved_base() {
        let mut r = fork_pair();
        r[1].gh_resolved = Some("base".into());
        let o = run(None, None, &[], &r, &[], None);
        assert_eq!(source(&o), BaseUrlSource::GhResolved);
        assert_eq!(key(&o), "github.com/someone/widgets");
    }

    #[test]
    fn rung5_single_project_even_under_two_names() {
        let r = vec![
            remote("origin", "git@github.com:acme/widgets.git"),
            remote("https-mirror", "https://github.com/acme/widgets"),
            remote("local", "/srv/git/widgets.git"),
        ];
        let o = run(None, None, &[], &r, &[], None);
        assert_eq!(source(&o), BaseUrlSource::Single);
        assert_eq!(key(&o), "github.com/acme/widgets");
    }

    #[test]
    fn rung6_upstream_only_when_verified() {
        let r = vec![
            remote("origin", "git@github.com:someone/widgets.git"),
            remote("upstream", "https://github.com/acme/widgets.git"),
        ];
        let o = run(None, None, &[], &r, &[], Some(true));
        assert_eq!(source(&o), BaseUrlSource::UpstreamVerified);
        assert_eq!(key(&o), "github.com/acme/widgets");
        for unverified in [None, Some(false)] {
            let o = run(None, None, &[], &r, &[], unverified);
            assert!(matches!(
                o,
                LadderOutcome::Refused {
                    code: BASE_URL_AMBIGUOUS,
                    ..
                }
            ));
        }
        // The daemon's own implementation never verifies (stubbed rung).
        let slugs: Vec<String> = vec![];
        let o = resolve(&LadderInput {
            explicit: None,
            config_base_url: None,
            pr_slugs: &slugs,
            remotes: &r,
            existing_keys: &[],
            fork_check: &NoForkCheck,
        });
        assert!(matches!(o, LadderOutcome::Refused { .. }));
    }

    #[test]
    fn rung7_refuses_with_candidates_and_no_url_echo() {
        let r = vec![
            remote(
                "a",
                "https://x-access-token:ghp_FAKE1@github.com/acme/widgets.git",
            ),
            remote("b", "https://github.com/acme/gadgets.git"),
        ];
        match run(None, None, &[], &r, &[], None) {
            LadderOutcome::Refused {
                code,
                reason,
                candidates,
            } => {
                assert_eq!(code, BASE_URL_AMBIGUOUS);
                assert_eq!(
                    candidates,
                    vec!["github.com/acme/gadgets", "github.com/acme/widgets"]
                );
                assert!(!reason.contains("ghp_"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn no_forge_remote_is_a_local_store() {
        assert_eq!(
            run(None, None, &[], &[], &[], None),
            LadderOutcome::NoForgeRemote
        );
        let r = vec![remote("backup", "/mnt/backup/widgets.git")];
        assert_eq!(
            run(None, None, &[], &r, &[], None),
            LadderOutcome::NoForgeRemote
        );
    }
}
