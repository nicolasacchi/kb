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
