//! RS-U2 — credential profile + ladder tests. The gh tests drive a FAKE
//! `gh` script (no keyring, no network); the one live test is `#[ignore]`.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use super::*;
use crate::review_store::git::{GitArgs, GitCall, BASE_FETCH_TIMEOUT};
use crate::review_store::url::{FetchRefspec, RefName, RefSource, RemoteName};

const TOKEN_ALICE: &str = "ghp_FAKEaliceAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const TOKEN_BOB: &str = "ghp_FAKEbobBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

struct FakeGh {
    dir: tempfile::TempDir,
}

impl FakeGh {
    /// `accounts`: (login, active, scopes).
    fn new(accounts: &[(&str, bool, &str)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let script = r#"#!/bin/sh
d="$(dirname "$0")"
printf '%s\n' "$*" >> "$d/calls"
env > "$d/env"
if [ "$1 $2" = "auth status" ]; then cat "$d/status.json"; exit 0; fi
if [ "$1 $2" = "auth token" ]; then
  u="$6"
  if [ -f "$d/token.$u" ]; then cat "$d/token.$u"; exit 0; fi
  echo "no oauth token found for github.com account $u" >&2; exit 1
fi
exit 2
"#;
        std::fs::write(d.join("gh"), script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(d.join("gh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let hosts: Vec<String> = accounts
            .iter()
            .map(|(l, a, s)| {
                format!(
                    r#"{{"state":"success","active":{a},"host":"github.com","login":"{l}","tokenSource":"keyring","scopes":"{s}","gitProtocol":"https"}}"#
                )
            })
            .collect();
        std::fs::write(
            d.join("status.json"),
            format!(r#"{{"hosts":{{"github.com":[{}]}}}}"#, hosts.join(",")),
        )
        .unwrap();
        for (login, _, _) in accounts {
            let tok = if login.eq_ignore_ascii_case("alice") {
                TOKEN_ALICE
            } else {
                TOKEN_BOB
            };
            std::fs::write(d.join(format!("token.{login}")), format!("{tok}\n")).unwrap();
        }
        wait_executable(&d.join("gh"));
        Self { dir }
    }

    fn gh(&self) -> GhCli {
        let prog = self.dir.path().join("gh");
        GhCli::from_env_fn(prog, |k| match k {
            "PATH" => Some("/usr/bin:/bin".into()),
            "HOME" => Some("/nonexistent-home".into()),
            // Must NOT reach gh: they override the keyring account.
            "GH_TOKEN" | "GITHUB_TOKEN" => Some("ghp_FAKEenvLEAKLEAKLEAKLEAKLEAKLEAK".into()),
            _ => None,
        })
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("calls")).unwrap_or_default()
    }
}

/// A just-written script can fail `exec` with ETXTBSY while a sibling
/// test thread's fork (between fork and exec) still holds the write fd.
/// Once one exec succeeds no writer is left, so later execs are safe.
pub(crate) fn wait_executable(p: &Path) {
    for _ in 0..200 {
        match std::process::Command::new(p).arg("--kbrs-warmup").output() {
            Err(e) if e.raw_os_error() == Some(26) => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            _ => return,
        }
    }
    panic!("{p:?} stayed ETXTBSY");
}

fn gh_url() -> RemoteUrl {
    RemoteUrl::parse_remote("https://github.com/acme/widgets.git").unwrap()
}

/// A `gh` that never answers — the shape a keyring locked by a
/// suspending laptop leaves behind. The deadline is 200 ms rather than
/// the production 20 s so the test is quick; the SCRIPT is what makes
/// it deterministic (a real deadline race would not be), and the killed
/// process group is what ends it.
fn hung_gh() -> (tempfile::TempDir, GhCli) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("gh");
    // `wait_executable` execs the script once with `--kbrs-warmup` and WAITS
    // for it: that warm-up must return at once, or this test stalls 60 s
    // before the deadline under test even starts.
    std::fs::write(
        &p,
        "#!/bin/sh\n[ \"$1\" = \"--kbrs-warmup\" ] && exit 0\nsleep 60\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    wait_executable(&p);
    // Owned `PathBuf`, exactly as `FakeGh::gh` passes it: `from_env_fn`
    // takes `impl Into<OsString>`.
    let gh = GhCli::from_env_fn(p.clone(), |k| match k {
        "PATH" => Some("/usr/bin:/bin".into()),
        "HOME" => Some("/nonexistent-home".into()),
        _ => None,
    })
    .with_timeout(std::time::Duration::from_millis(200));
    (dir, gh)
}

/// A `gh` that outlives its deadline is NOT an identity failure, and the
/// two predicates that decide it are the ones the store path reads:
/// `registry::seed`/`sync_ready` stop the pass on `is_auth()` and
/// degrade to a recorded skip otherwise, and `kb-code store sync` exits
/// 6 only for an auth class. As a `CredentialUnavailable` a slow keyring
/// was both — the store refused to seed and the row held `absent` on a
/// credential that was perfectly good.
#[test]
fn a_gh_deadline_is_transient_and_never_an_identity_failure() {
    let (_dir, gh) = hung_gh();
    let err = gh.accounts("github.com").unwrap_err();
    assert!(
        matches!(err, CredError::TimedOut(_)),
        "the deadline must be its own error, not `Unavailable`: {err:?}"
    );
    assert_eq!(err.class(), FailureClass::Timeout);
    assert!(err.class().is_transient());
    assert!(
        !err.class().is_auth(),
        "a timeout in `is_auth()` refuses the seed on a slow machine"
    );
    // The same probe shape through the ladder: a bound store still STOPS
    // (no identity swap), and the class it stops with is the transient
    // one, so the store degrades to a skip instead of being refused.
    let p = Probes {
        gh: Some(|| Err(CredError::TimedOut(GH_TIMEOUT))),
        anon_ok: true,
        ..Default::default()
    };
    let mut c = cfg();
    c.gh_user = Some("alice".into());
    let err = resolve_fetch_credential(&c, &gh_url(), &p).unwrap_err();
    assert_eq!(err.class(), FailureClass::Timeout, "{err}");
    assert_eq!(*p.log.borrow(), ["gh"], "no fall-through past a bound rung");
}

#[test]
fn an_explicit_default_port_is_the_same_credential_scope() {
    // git normalises the default port out of a remote before it fills a
    // credential query, so a scope spelled `github.com:443` could never
    // match the helper's exact `host=github.com` test — a semantically
    // correct remote presenting as a credential fault. Fails closed, which
    // is why it is easy to mistake for a real one.
    let bare = RemoteUrl::parse_remote("https://github.com/acme/widgets.git").unwrap();
    let ported = RemoteUrl::parse_remote("https://github.com:443/acme/widgets.git").unwrap();
    let scope = |u: &RemoteUrl| CredentialScope::for_url(u).unwrap();
    assert_eq!(scope(&ported), scope(&bare));
    assert_eq!(scope(&ported).authority(), "github.com");
    // A non-default port is a different endpoint and keeps its own scope.
    let custom = RemoteUrl::parse_remote("https://github.com:8443/acme/widgets.git").unwrap();
    assert_eq!(scope(&custom).authority(), "github.com:8443");
}

#[test]
fn pinned_user_reads_that_accounts_token_even_when_another_is_active() {
    let f = FakeGh::new(&[("alice", false, "repo"), ("bob", true, "repo")]);
    let c = GhCliCredential::acquire(&f.gh(), &gh_url(), Some("alice"), None).unwrap();
    assert_eq!(c.account(), "alice");
    assert_eq!(c.https().secret(), TOKEN_ALICE);
    assert_eq!(c.https().scope().authority(), "github.com");
    assert!(f
        .calls()
        .contains("auth token --hostname github.com --user alice"));
    // gh ran with a scrubbed env: no GH_TOKEN/GITHUB_TOKEN, prompts off.
    let env = std::fs::read_to_string(f.dir.path().join("env")).unwrap();
    assert!(!env.contains("GH_TOKEN="), "{env}");
    assert!(!env.contains("GITHUB_TOKEN="), "{env}");
    assert!(env.contains("GH_PROMPT_DISABLED=1"));
    assert!(env.contains("HOME=/nonexistent-home"));
}

#[test]
fn pinned_user_missing_is_credential_account_mismatch() {
    let f = FakeGh::new(&[("mallory", true, "repo")]);
    let err = GhCliCredential::acquire(&f.gh(), &gh_url(), Some("alice"), None).unwrap_err();
    assert_eq!(err.class().slug(), "credential-account-mismatch");
    match &err {
        CredError::AccountMismatch {
            expected, found, ..
        } => {
            assert_eq!(expected, "alice");
            assert_eq!(found, "mallory");
        }
        other => panic!("{other:?}"),
    }
    assert!(
        !f.calls().contains("auth token"),
        "no token read for the wrong account"
    );
}

#[test]
fn unpinned_active_account_must_match_the_recorded_one() {
    let f = FakeGh::new(&[("mallory", true, "repo")]);
    let err = GhCliCredential::acquire(&f.gh(), &gh_url(), None, Some("alice")).unwrap_err();
    assert_eq!(err.class(), FailureClass::CredentialAccountMismatch);
    assert!(!f.calls().contains("auth token"));
    // case-insensitive, like GitHub logins
    let f = FakeGh::new(&[("Alice", true, "repo")]);
    assert!(GhCliCredential::acquire(&f.gh(), &gh_url(), None, Some("alice")).is_ok());
}

#[test]
fn unpinned_reads_the_token_bound_to_the_observed_active_login() {
    let f = FakeGh::new(&[
        ("alice", false, "repo"),
        ("bob", true, "admin:org, repo, workflow"),
    ]);
    let c = GhCliCredential::acquire(&f.gh(), &gh_url(), None, None).unwrap();
    assert_eq!(c.account(), "bob");
    assert_eq!(c.https().secret(), TOKEN_BOB);
    assert!(
        f.calls().contains("--user bob"),
        "the token read is bound to the checked login"
    );
    assert!(c.broader_than_needed());
    assert_eq!(c.scopes(), ["admin:org", "repo", "workflow"]);
    let api = c.api_credential();
    assert_eq!(api.bearer_token(), TOKEN_BOB);
    assert_eq!(
        api.source(),
        &ApiCredentialSource::GhCli {
            account: "bob".into()
        }
    );
    // Debug never prints the token.
    for dbg in [
        format!("{c:?}"),
        format!("{api:?}"),
        format!("{:?}", c.https()),
    ] {
        assert!(!dbg.contains("ghp_"), "{dbg}");
    }
}

#[test]
fn not_logged_in_and_not_installed_are_distinct() {
    let f = FakeGh::new(&[]);
    let err = GhCliCredential::acquire(&f.gh(), &gh_url(), None, None).unwrap_err();
    assert!(matches!(err, CredError::GhNotLoggedIn { .. }), "{err:?}");
    let gh = GhCli::from_env_fn("/nonexistent/kbrs/gh", |_| None);
    let err = GhCliCredential::acquire(&gh, &gh_url(), None, None).unwrap_err();
    assert!(matches!(err, CredError::GhNotInstalled), "{err:?}");
    // gh-cli never serves an ssh URL directly.
    let ssh = RemoteUrl::parse_remote("git@github.com:acme/widgets.git").unwrap();
    let f = FakeGh::new(&[("alice", true, "repo")]);
    assert!(matches!(
        GhCliCredential::acquire(&f.gh(), &ssh, None, None),
        Err(CredError::Refused(_))
    ));
}

#[test]
fn secret_tokens_refuse_protocol_injection_and_never_print() {
    assert!(SecretToken::new("ghp_FAKE\nhost=evil").is_err());
    assert!(SecretToken::new("a b").is_err());
    assert!(SecretToken::new("  ").is_err());
    let t = SecretToken::new(" ghp_FAKEtrimmedXXXXXXXXXXXXXXXX \n").unwrap();
    assert_eq!(t.expose_secret(), "ghp_FAKEtrimmedXXXXXXXXXXXXXXXX");
    assert_eq!(format!("{t}"), "[redacted]");
    assert!(!format!("{t:?}").contains("ghp_"));
    assert!(ApiCredential::caller_supplied("x\ny").is_err());
    assert_eq!(
        ApiCredential::caller_supplied("ghp_FAKEcallerXXXXXXXXXXXXXXXX")
            .unwrap()
            .source(),
        &ApiCredentialSource::CallerSupplied
    );
}

#[test]
fn token_files_must_be_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("acme.token");
    std::fs::write(&p, "ghp_FAKEfileXXXXXXXXXXXXXXXXXXXXXX\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
    let err = read_token_file(&p, DEFAULT_TOKEN_USERNAME, &gh_url()).unwrap_err();
    assert_eq!(err.class(), FailureClass::CredentialUnavailable);
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    let c = read_token_file(&p, DEFAULT_TOKEN_USERNAME, &gh_url()).unwrap();
    assert_eq!(c.secret(), "ghp_FAKEfileXXXXXXXXXXXXXXXXXXXXXX");

    // A symlink is refused even when its target is fine (O_NOFOLLOW).
    let link = dir.path().join("link.token");
    std::os::unix::fs::symlink(&p, &link).unwrap();
    let err = read_token_file(&link, DEFAULT_TOKEN_USERNAME, &gh_url()).unwrap_err();
    assert_eq!(err.class(), FailureClass::CredentialUnavailable);

    // Oversized files are refused (bounded read).
    let big = dir.path().join("big.token");
    std::fs::write(&big, "a".repeat(4097)).unwrap();
    std::fs::set_permissions(&big, std::fs::Permissions::from_mode(0o600)).unwrap();
    let err = read_token_file(&big, DEFAULT_TOKEN_USERNAME, &gh_url()).unwrap_err();
    assert!(err.to_string().contains("larger than"), "{err}");

    // A directory is not a token file.
    let err = read_token_file(dir.path(), DEFAULT_TOKEN_USERNAME, &gh_url()).unwrap_err();
    assert_eq!(err.class(), FailureClass::CredentialUnavailable);
}

#[test]
fn credential_pin_parses_tolerantly() {
    assert_eq!(
        CredentialPin::parse_tolerant("gh-cli"),
        (CredentialPin::GhCli, None)
    );
    assert_eq!(
        CredentialPin::parse_tolerant(" Inherit "),
        (CredentialPin::Inherit, None)
    );
    let (p, w) = CredentialPin::parse_tolerant("keyring");
    assert_eq!(p, CredentialPin::Auto);
    assert!(w.is_some());
    assert_eq!(ProfileKind::TokenFile.slug(), "token");
}

// ---- the ladder, against scripted probes ------------------------------

#[derive(Default)]
struct Probes {
    gh: Option<fn() -> Result<GhCliCredential, CredError>>,
    anon_ok: bool,
    token_ok: bool,
    log: RefCell<Vec<&'static str>>,
    gh_url_seen: RefCell<Option<String>>,
}

fn fake_gh_cred() -> Result<GhCliCredential, CredError> {
    let f = FakeGh::new(&[("alice", true, "repo")]);
    GhCliCredential::acquire(&f.gh(), &gh_url(), None, None)
}
fn gh_not_logged_in() -> Result<GhCliCredential, CredError> {
    Err(CredError::GhNotLoggedIn {
        host: "github.com".into(),
    })
}
fn gh_mismatch() -> Result<GhCliCredential, CredError> {
    Err(CredError::AccountMismatch {
        host: "github.com".into(),
        expected: "alice".into(),
        found: "mallory".into(),
    })
}

impl LadderProbes for Probes {
    fn gh_cli(
        &self,
        url: &RemoteUrl,
        _: Option<&str>,
        _: Option<&str>,
    ) -> Result<GhCliCredential, CredError> {
        self.log.borrow_mut().push("gh");
        *self.gh_url_seen.borrow_mut() = Some(url.as_str().to_string());
        (self.gh.unwrap_or(gh_not_logged_in))()
    }
    fn token_file(
        &self,
        _: &Path,
        username: &str,
        url: &RemoteUrl,
    ) -> Result<HttpsCredential, CredError> {
        self.log.borrow_mut().push("token");
        if self.token_ok {
            HttpsCredential::new(
                CredentialScope::for_url(url).unwrap(),
                username,
                SecretToken::new("ghp_FAKEtokenfileXXXXXXXXXXXXXXXX")?,
            )
        } else {
            Err(CredError::Unavailable("mode".into()))
        }
    }
    fn anonymous(&self, _: &RemoteUrl) -> Result<(), CredError> {
        self.log.borrow_mut().push("anon");
        if self.anon_ok {
            Ok(())
        } else {
            Err(CredError::Unavailable("probe failed".into()))
        }
    }
}

fn cfg() -> FetchCredentialConfig {
    FetchCredentialConfig::default()
}

#[test]
fn ladder_prefers_gh_cli() {
    let p = Probes {
        gh: Some(fake_gh_cred),
        anon_ok: true,
        ..Default::default()
    };
    let r = resolve_fetch_credential(&cfg(), &gh_url(), &p).unwrap();
    assert_eq!(r.credential.kind(), ProfileKind::GhCli);
    assert_eq!(r.credential.account(), Some("alice"));
    assert_eq!(*p.log.borrow(), ["gh"]);
}

#[test]
fn ladder_uses_the_https_form_of_an_ssh_remote() {
    let p = Probes {
        gh: Some(fake_gh_cred),
        ..Default::default()
    };
    let ssh = RemoteUrl::parse_remote("git@github.com:acme/widgets.git").unwrap();
    resolve_fetch_credential(&cfg(), &ssh, &p).unwrap();
    assert_eq!(
        p.gh_url_seen.borrow().as_deref(),
        Some("https://github.com/acme/widgets.git")
    );
}

#[test]
fn ladder_falls_through_with_recorded_reasons() {
    let p = Probes {
        anon_ok: true,
        ..Default::default()
    };
    let mut c = cfg();
    c.token_file = Some(PathBuf::from("/nonexistent/acme.token"));
    let r = resolve_fetch_credential(&c, &gh_url(), &p).unwrap();
    assert_eq!(r.credential.kind(), ProfileKind::Anonymous);
    assert_eq!(*p.log.borrow(), ["gh", "token", "anon"]);
    let rungs: Vec<ProfileKind> = r.skipped.iter().map(|s| s.rung).collect();
    assert_eq!(
        rungs,
        [
            ProfileKind::GhCli,
            ProfileKind::DeployKey,
            ProfileKind::TokenFile
        ]
    );
    assert_eq!(r.skipped[0].class, FailureClass::CredentialUnavailable);
}

#[test]
fn ladder_stops_on_an_account_mismatch() {
    let p = Probes {
        gh: Some(gh_mismatch),
        anon_ok: true,
        ..Default::default()
    };
    let err = resolve_fetch_credential(&cfg(), &gh_url(), &p).unwrap_err();
    assert_eq!(err.class(), FailureClass::CredentialAccountMismatch);
    assert_eq!(*p.log.borrow(), ["gh"], "no fall-through to anonymous");
}

#[test]
fn a_bound_account_stops_the_ladder_on_any_gh_failure() {
    // Every way gh can fail to produce the bound account's token, with
    // the class each one now carries. The STOP is the rule under test
    // (falling through would swap the store's identity); the class
    // column is what D12 reads, so it is pinned here too — a timeout
    // is deliberately NOT a credential class, and that difference is
    // invisible to a loop that only checks "it stopped".
    fn logged_out() -> Result<GhCliCredential, CredError> {
        Err(CredError::GhNotLoggedIn {
            host: "github.com".into(),
        })
    }
    fn missing() -> Result<GhCliCredential, CredError> {
        Err(CredError::GhNotInstalled)
    }
    fn locked() -> Result<GhCliCredential, CredError> {
        Err(CredError::TimedOut(GH_TIMEOUT))
    }
    fn too_old() -> Result<GhCliCredential, CredError> {
        Err(CredError::Unavailable(
            "gh auth status: unknown flag: --json".into(),
        ))
    }
    /// A gh rung that fails, and the class the failure must be reported
    /// as. Named because the inline form trips `type_complexity`, and
    /// because the pairing IS the assertion: every rung below must land
    /// in the class its failure actually is, not merely in `is_auth`.
    type FailingRung = (fn() -> Result<GhCliCredential, CredError>, FailureClass);
    let failures: [FailingRung; 4] = [
        (logged_out, FailureClass::CredentialUnavailable),
        (missing, FailureClass::CredentialUnavailable),
        (locked, FailureClass::Timeout),
        (too_old, FailureClass::CredentialUnavailable),
    ];
    for (bind_pin, bind_rec) in [(true, false), (false, true)] {
        for (gh, class) in failures {
            let p = Probes {
                gh: Some(gh),
                anon_ok: true,
                token_ok: true,
                ..Default::default()
            };
            let mut c = cfg();
            c.allow_inherited_credentials = true;
            c.token_file = Some(PathBuf::from("/x"));
            if bind_pin {
                c.gh_user = Some("alice".into());
            }
            if bind_rec {
                c.recorded_account = Some("alice".into());
            }
            let err = resolve_fetch_credential(&c, &gh_url(), &p).unwrap_err();
            assert_eq!(err.class(), class, "{err}");
            assert_eq!(
                *p.log.borrow(),
                ["gh"],
                "fell through past a bound gh-cli rung"
            );
        }
    }
    // Bound, but the remote has no https form (ssh on a custom port).
    let p = Probes {
        gh: Some(fake_gh_cred),
        anon_ok: true,
        ..Default::default()
    };
    let mut c = cfg();
    c.gh_user = Some("alice".into());
    let odd = RemoteUrl::parse_remote("ssh://git@git.example.com:7999/acme/widgets.git").unwrap();
    assert!(matches!(
        resolve_fetch_credential(&c, &odd, &p),
        Err(CredError::Refused(_))
    ));
    assert!(p.log.borrow().is_empty());
}

#[test]
fn ladder_inherit_only_when_allowed_else_none() {
    let p = Probes::default();
    let mut c = cfg();
    c.allow_inherited_credentials = true;
    let r = resolve_fetch_credential(&c, &gh_url(), &p).unwrap();
    assert_eq!(r.credential.kind(), ProfileKind::Inherit);
    assert!(r.credential.is_amber());
    c.allow_inherited_credentials = false;
    let r = resolve_fetch_credential(&c, &gh_url(), &p).unwrap();
    assert_eq!(r.credential.kind(), ProfileKind::None);
    assert!(r.credential.auth().is_none());
    assert_eq!(r.skipped.last().unwrap().rung, ProfileKind::Inherit);
}

/// Posture proof (N5-a): `CredentialPin::default()` is `Auto`, a ladder and
/// not a point on the access axis, so it is NOT a `Posture` type. What makes
/// it safe is behavioural: without `allow_inherited_credentials` the resolved
/// credential for the default pin is never `Inherit`, whatever the probes and
/// the remote shape do. The gate is the `cfg.allow_inherited_credentials`
/// check in `resolve_fetch_credential` (rung 6) and in the explicit-pin arm.
#[test]
fn credential_pin_default_never_inherits_without_opt_in() {
    assert_eq!(CredentialPin::default(), CredentialPin::Auto);
    let c = cfg();
    assert!(!c.allow_inherited_credentials);
    assert_eq!(c.pin, CredentialPin::default());
    let ssh = RemoteUrl::parse_remote("git@github.com:acme/widgets.git").unwrap();
    let odd = RemoteUrl::parse_remote("ssh://git@git.example.com:7999/acme/widgets.git").unwrap();
    let probe_sets = [
        Probes::default(),
        Probes {
            anon_ok: true,
            ..Default::default()
        },
        Probes {
            token_ok: true,
            ..Default::default()
        },
        Probes {
            gh: Some(fake_gh_cred),
            ..Default::default()
        },
    ];
    for p in &probe_sets {
        for url in [gh_url(), ssh.clone(), odd.clone()] {
            if let Ok(r) = resolve_fetch_credential(&c, &url, p) {
                assert_ne!(r.credential.kind(), ProfileKind::Inherit);
                assert!(!r.credential.is_amber());
            }
        }
    }
    // Every probe failing lands on `none`, with inherit recorded as skipped.
    let r = resolve_fetch_credential(&c, &gh_url(), &Probes::default()).unwrap();
    assert_eq!(r.credential.kind(), ProfileKind::None);
    // The explicit pin is refused too, not silently downgraded.
    let mut pinned = cfg();
    pinned.pin = CredentialPin::Inherit;
    assert!(matches!(
        resolve_fetch_credential(&pinned, &gh_url(), &Probes::default()),
        Err(CredError::Refused(_))
    ));
}

#[test]
fn a_pinned_rung_never_falls_through() {
    let p = Probes {
        anon_ok: true,
        ..Default::default()
    };
    let mut c = cfg();
    c.pin = CredentialPin::GhCli;
    assert!(resolve_fetch_credential(&c, &gh_url(), &p).is_err());
    assert_eq!(*p.log.borrow(), ["gh"]);

    c.pin = CredentialPin::DeployKey;
    assert!(matches!(
        resolve_fetch_credential(&c, &gh_url(), &p),
        Err(CredError::Refused(_))
    ));
    c.pin = CredentialPin::Inherit;
    assert!(resolve_fetch_credential(&c, &gh_url(), &p).is_err());
    c.allow_inherited_credentials = true;
    assert_eq!(
        resolve_fetch_credential(&c, &gh_url(), &p)
            .unwrap()
            .credential
            .kind(),
        ProfileKind::Inherit
    );
    c.pin = CredentialPin::None;
    assert_eq!(
        resolve_fetch_credential(&c, &gh_url(), &p)
            .unwrap()
            .credential
            .kind(),
        ProfileKind::None
    );
    c.pin = CredentialPin::Token;
    assert!(
        resolve_fetch_credential(&c, &gh_url(), &p).is_err(),
        "no token_file configured"
    );
    let p = Probes {
        token_ok: true,
        ..Default::default()
    };
    c.token_file = Some(PathBuf::from("/x"));
    assert_eq!(
        resolve_fetch_credential(&c, &gh_url(), &p)
            .unwrap()
            .credential
            .kind(),
        ProfileKind::TokenFile
    );
}

/// LIVE (network + the operator's `gh` login): a fully scrubbed-env
/// `ls-remote` + `fetch` of a small public GitHub repo through the gh-cli
/// profile and kb's in-memory credential helper. Never prints the token.
///
/// ```text
/// CARGO_TARGET_DIR=… cargo test -p kb-code-server --profile fast \
///     live_gh_cli_scrubbed_fetch -- --ignored --nocapture
/// # optional: KBRS_LIVE_GH_USER=<login> to exercise the pinned path,
/// #           KBRS_LIVE_REPO=https://github.com/<owner>/<repo>.git
/// ```
#[test]
#[ignore = "live: needs network and a logged-in gh"]
fn live_gh_cli_scrubbed_fetch_of_a_public_repo() {
    let repo = std::env::var("KBRS_LIVE_REPO")
        .unwrap_or_else(|_| "https://github.com/octocat/Hello-World.git".into());
    let url = RemoteUrl::parse_remote(&repo).unwrap();
    let pinned = std::env::var("KBRS_LIVE_GH_USER").ok();
    let gh = GhCli::from_process_env();
    let cred = GhCliCredential::acquire(&gh, &url, pinned.as_deref(), None).expect("gh-cli");
    eprintln!(
        "gh-cli account={} broader_than_needed={}",
        cred.account(),
        cred.broader_than_needed()
    );

    let tmp = tempfile::tempdir().unwrap();
    let sg = StoreGit::new(tmp.path().join("git-home")).unwrap();
    let auth = FetchAuth::Token(cred.https());
    let refs = sg
        .ls_remote(&url, auth, &["HEAD"], LS_REMOTE_TIMEOUT)
        .expect("ls-remote");
    assert!(refs.iter().any(|(_, n)| n == "HEAD"), "{refs:?}");

    let store = tmp.path().join("store.git");
    sg.init_bare(&store).unwrap();
    sg.configure_remote(&store, &RemoteName::base(), &url)
        .unwrap();
    let dst = RefName::parse("refs/remotes/base/HEAD-probe").unwrap();
    let head_oid = refs.iter().find(|(_, n)| n == "HEAD").unwrap().0.clone();
    let spec = FetchRefspec::new(true, RefSource::oid(&head_oid).unwrap(), dst.clone());
    sg.fetch(
        &store,
        &RemoteName::base(),
        &[spec],
        auth,
        BASE_FETCH_TIMEOUT,
    )
    .expect("fetch");
    let got = sg
        .run(
            GitCall::new(
                "rev-parse",
                GitArgs::new("rev-parse")
                    .flag("--verify")
                    .end_of_options()
                    .refname(&dst),
            )
            .git_dir(&store),
        )
        .unwrap();
    assert_eq!(got.stdout_str().trim(), head_oid);
    eprintln!("fetched HEAD {head_oid} into a scrubbed-env store");
}
