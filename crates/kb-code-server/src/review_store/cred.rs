//! RS-U2 — review-store credential profiles and the fetch ladder
//! (README §8, D5/D9/D12; design-internal-store §5.1/§5.4;
//! api-access-and-credentials §5).
//!
//! # Two slots
//!
//! * **fetch** — git transport into the store: [`FetchCredential`],
//!   chosen by [`resolve_fetch_credential`] and handed to
//!   [`super::git::StoreGit`] as a [`FetchAuth`].
//! * **api** — forge metadata: [`ApiCredential`]. This unit only EXPOSES
//!   it (the gh-cli token read for the fetch slot can fill the api slot
//!   too, [`GhCliCredential::api_credential`], or be read on demand,
//!   [`ApiCredential::from_gh_cli`]); `github.rs`'s REST ladder is rewired
//!   onto it by a later unit.
//!
//! The pre-existing `--gh-token-from-cli` relay (`kb-code-cli` runs
//! `gh auth token` itself and posts `CreateReviewPrBody.gh_token`,
//! admitted loopback-only by `github::admit_cli_github_token`) is left
//! exactly as it is: it is the CALLER-SUPPLIED rung of the api ladder
//! ([`ApiCredentialSource::CallerSupplied`]). It never reaches the fetch
//! slot — a caller's token is not a store credential.
//!
//! # Fetch ladder (README §8), per store, with its reason
//!
//! 1. explicit pin (`credential = gh-cli|deploy-key|token|anonymous|inherit|none`)
//!    — a pinned rung that fails is an ERROR, never a fall-through;
//! 2. **`gh-cli`** — the operator's `gh` login;
//! 3. deploy key — Phase 2; recorded as skipped;
//! 4. `token_file` — an owner-only (0600/0400) file, HTTPS;
//! 5. anonymous HTTPS — only if a scrubbed `ls-remote … HEAD` succeeds;
//! 6. `inherit` — only if `allow_inherited_credentials`; amber;
//! 7. `none`.
//!
//! Under `auto`, a rung that does not apply is skipped WITH a recorded
//! reason ([`SkippedRung`]); the one exception is
//! `credential-account-mismatch`, which stops the ladder: D12 makes it an
//! error, and falling through to `anonymous` would turn it into a silent
//! downgrade.
//!
//! # `gh-cli` (D12)
//!
//! kb never runs `gh auth git-credential` (it answers with whichever
//! account is ACTIVE). It reads `gh auth status --hostname H --json hosts`
//! (logins, active flag, scopes — never the token), picks the account —
//! the pinned `gh_user`, else the active one, which must equal the
//! previously recorded `cred_account` if there is one — and then runs
//! `gh auth token --hostname H --user <that login>`, so the token read is
//! bound to the account that was checked (no switch-in-between race). The
//! token is held in memory only ([`SecretToken`], zeroized on drop,
//! redacting `Debug`/`Display`, no `Serialize`) and reaches git through
//! `StoreGit`'s inherited-pipe credential helper.
//!
//! `gh` runs with a cleared environment plus a small allowlist (it needs
//! `HOME`/XDG dirs and the D-Bus session to reach its keyring). `GH_TOKEN`,
//! `GITHUB_TOKEN` and the enterprise variants are deliberately NOT passed:
//! they would override the keyring and make `--user` meaningless.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use zeroize::{Zeroize, Zeroizing};

use super::classify::FailureClass;
use super::git::{FetchAuth, StoreGit, StoreGitError, LS_REMOTE_TIMEOUT};
use super::proc::{self, RunSpec};
use super::redact::redact_bytes;
use super::url::{Protocol, RemoteUrl};

/// Default HTTPS username paired with a token (GitHub accepts any
/// non-empty username with a token; this one is the documented form).
pub const DEFAULT_TOKEN_USERNAME: &str = "x-access-token";

/// `gh` deadline. `gh auth status` validates each account over the
/// network, so this is looser than a local call.
pub const GH_TIMEOUT: Duration = Duration::from_secs(20);

const GH_ENV_PASSTHROUGH: &[&str] = &[
    "PATH",
    "HOME",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
    "XDG_RUNTIME_DIR",
    "DBUS_SESSION_BUS_ADDRESS",
    "GH_CONFIG_DIR",
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "NO_PROXY",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

// ---------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------

/// A credential failure. No variant ever carries a token; gh stderr is
/// redacted before it lands in [`CredError::Unavailable`].
#[derive(Debug, thiserror::Error)]
pub enum CredError {
    #[error("gh is not installed")]
    GhNotInstalled,
    #[error("gh is not logged in to {host}")]
    GhNotLoggedIn { host: String },
    #[error(
        "credential-account-mismatch: expected gh account `{expected}` on {host}, but `{found}` answers"
    )]
    AccountMismatch {
        host: String,
        expected: String,
        found: String,
    },
    #[error("credential unavailable: {0}")]
    Unavailable(String),
    #[error("invalid credential: {0}")]
    Invalid(&'static str),
    #[error("{0}")]
    Refused(String),
    #[error(transparent)]
    Git(#[from] StoreGitError),
}

impl CredError {
    pub fn class(&self) -> FailureClass {
        match self {
            Self::GhNotInstalled | Self::GhNotLoggedIn { .. } | Self::Unavailable(_) => {
                FailureClass::CredentialUnavailable
            }
            Self::AccountMismatch { .. } => FailureClass::CredentialAccountMismatch,
            Self::Invalid(_) => FailureClass::CredentialRejected,
            Self::Refused(_) => FailureClass::NoCredentials,
            Self::Git(e) => e.class,
        }
    }
}

// ---------------------------------------------------------------------
// Secret material
// ---------------------------------------------------------------------

/// A token held in memory only: zeroized on drop, `Debug`/`Display`
/// print `[redacted]`, no `Serialize`. Rejects whitespace/control
/// characters — a newline would inject lines into the credential
/// protocol.
#[derive(Clone)]
pub struct SecretToken(Zeroizing<String>);

impl SecretToken {
    pub fn new(raw: &str) -> Result<Self, CredError> {
        let t = raw.trim();
        if t.is_empty() {
            return Err(CredError::Invalid("empty token"));
        }
        if t.len() > 1024 {
            return Err(CredError::Invalid("token too long"));
        }
        if t.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err(CredError::Invalid(
                "token contains whitespace or control characters",
            ));
        }
        Ok(Self(Zeroizing::new(t.to_string())))
    }

    /// The secret itself. Every caller is a place the token can leak —
    /// keep them few (the credential pipe, a REST `Authorization` header).
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretToken([redacted])")
    }
}

impl fmt::Display for SecretToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(super::redact::REDACTED)
    }
}

/// Where a helper answers: exactly one protocol + `host[:port]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialScope {
    protocol: Protocol,
    authority: String,
}

impl CredentialScope {
    /// The scope of an https URL. `None` for ssh/local — a token is never
    /// offered to anything but https.
    pub fn for_url(url: &RemoteUrl) -> Option<Self> {
        match url.protocol() {
            Protocol::Https => Some(Self {
                protocol: Protocol::Https,
                authority: url.authority().to_string(),
            }),
            #[cfg(test)]
            Protocol::Http => Some(Self {
                protocol: Protocol::Http,
                authority: url.authority().to_string(),
            }),
            _ => None,
        }
    }
    pub fn protocol(&self) -> Protocol {
        self.protocol
    }
    pub fn authority(&self) -> &str {
        &self.authority
    }
}

/// An HTTPS username + token bound to one [`CredentialScope`].
#[derive(Clone)]
pub struct HttpsCredential {
    scope: CredentialScope,
    username: String,
    token: SecretToken,
}

impl HttpsCredential {
    pub fn new(
        scope: CredentialScope,
        username: &str,
        token: SecretToken,
    ) -> Result<Self, CredError> {
        let ok = !username.is_empty()
            && username.len() <= 128
            && username
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'+'));
        if !ok {
            return Err(CredError::Invalid("bad token username"));
        }
        Ok(Self {
            scope,
            username: username.to_string(),
            token,
        })
    }

    pub fn scope(&self) -> &CredentialScope {
        &self.scope
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub(crate) fn secret(&self) -> &str {
        self.token.expose_secret()
    }

    /// The git-credential answer written into the helper pipe.
    pub(crate) fn helper_payload(&self) -> Zeroizing<Vec<u8>> {
        let mut s = String::with_capacity(64 + self.secret().len());
        s.push_str("username=");
        s.push_str(&self.username);
        s.push_str("\npassword=");
        s.push_str(self.secret());
        s.push('\n');
        let v = Zeroizing::new(s.as_bytes().to_vec());
        s.zeroize();
        v
    }
}

impl fmt::Debug for HttpsCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpsCredential")
            .field("scope", &self.scope)
            .field("username", &self.username)
            .field("token", &self.token)
            .finish()
    }
}

// ---------------------------------------------------------------------
// gh CLI
// ---------------------------------------------------------------------

/// One account `gh auth status --json hosts` reports (never the token).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhAccount {
    pub login: String,
    pub active: bool,
    pub state: String,
    pub scopes: Vec<String>,
}

/// The `gh` binary, run with a scrubbed environment and a deadline.
#[derive(Debug, Clone)]
pub struct GhCli {
    program: OsString,
    env: Vec<(OsString, OsString)>,
    timeout: Duration,
}

fn gh_host_ok(h: &str) -> bool {
    let b = h.as_bytes();
    !b.is_empty()
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(*c, b'.' | b'-'))
}

/// GitHub login grammar: alphanumeric first, then alphanumerics/`-`.
fn gh_login_ok(u: &str) -> bool {
    let b = u.as_bytes();
    !b.is_empty()
        && b.len() <= 39
        && b[0].is_ascii_alphanumeric()
        && b.iter().all(|c| c.is_ascii_alphanumeric() || *c == b'-')
}

impl GhCli {
    /// `gh` from the daemon's PATH, with the daemon's allowlisted env.
    pub fn from_process_env() -> Self {
        Self::from_env_fn("gh", |k| std::env::var_os(k))
    }

    /// `program` with only [`GH_ENV_PASSTHROUGH`] read through `get`.
    pub fn from_env_fn(
        program: impl Into<OsString>,
        get: impl Fn(&str) -> Option<OsString>,
    ) -> Self {
        let env = GH_ENV_PASSTHROUGH
            .iter()
            .filter_map(|k| get(k).map(|v| (OsString::from(k), v)))
            .collect();
        Self {
            program: program.into(),
            env,
            timeout: GH_TIMEOUT,
        }
    }

    pub fn with_timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(&self.program);
        cmd.env_clear();
        for (k, v) in &self.env {
            cmd.env(k, v);
        }
        cmd.env("LC_ALL", "C")
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("GH_NO_EXTENSION_UPDATE_NOTIFIER", "1")
            .env("GH_SPINNER_DISABLED", "1")
            .env("NO_COLOR", "1")
            .env("CLICOLOR", "0")
            .args(args);
        cmd
    }

    fn run(&self, args: &[&str], stdout_cap: usize) -> Result<proc::Captured, CredError> {
        let mut cmd = self.command(args);
        let spec = RunSpec {
            timeout: self.timeout,
            stdout_cap,
            stderr_cap: 16 * 1024,
            stdin: None,
        };
        let cap = proc::run(&mut cmd, &spec).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                CredError::GhNotInstalled
            } else {
                CredError::Unavailable(format!("spawn gh: {e}"))
            }
        })?;
        if cap.timed_out {
            return Err(CredError::Unavailable(
                "gh timed out (keyring locked?)".into(),
            ));
        }
        Ok(cap)
    }

    /// The accounts `gh` holds for `host`. Empty ⇒ not logged in.
    pub fn accounts(&self, host: &str) -> Result<Vec<GhAccount>, CredError> {
        if !gh_host_ok(host) {
            return Err(CredError::Invalid("bad gh hostname"));
        }
        let cap = self.run(
            &["auth", "status", "--hostname", host, "--json", "hosts"],
            1 << 20,
        )?;
        let stderr = redact_bytes(&cap.stderr, &[], 512);
        let ok = cap.status.is_some_and(|s| s.success());
        if !ok {
            let l = stderr.to_ascii_lowercase();
            if l.contains("not logged in") || l.contains("no oauth token") {
                return Ok(Vec::new());
            }
            return Err(CredError::Unavailable(format!("gh auth status: {stderr}")));
        }
        let v: serde_json::Value = serde_json::from_slice(&cap.stdout).map_err(|_| {
            CredError::Unavailable("gh auth status: unparseable --json output".into())
        })?;
        let list = v
            .get("hosts")
            .and_then(|h| h.get(host))
            .and_then(|a| a.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(list
            .iter()
            .filter_map(|a| {
                let login = a.get("login")?.as_str()?.to_string();
                if !gh_login_ok(&login) {
                    return None;
                }
                Some(GhAccount {
                    login,
                    active: a.get("active").and_then(|x| x.as_bool()).unwrap_or(false),
                    state: a
                        .get("state")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    scopes: a
                        .get("scopes")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect(),
                })
            })
            .collect())
    }

    /// `gh auth token --hostname <host> --user <user>`.
    pub fn token(&self, host: &str, user: &str) -> Result<SecretToken, CredError> {
        if !gh_host_ok(host) {
            return Err(CredError::Invalid("bad gh hostname"));
        }
        if !gh_login_ok(user) {
            return Err(CredError::Invalid("bad gh login"));
        }
        let mut cap = self.run(&["auth", "token", "--hostname", host, "--user", user], 4096)?;
        let ok = cap.status.is_some_and(|s| s.success());
        let res = if ok {
            match std::str::from_utf8(&cap.stdout) {
                Ok(s) => SecretToken::new(s),
                Err(_) => Err(CredError::Invalid("gh printed a non-UTF-8 token")),
            }
        } else {
            let stderr = redact_bytes(&cap.stderr, &[], 512);
            Err(CredError::Unavailable(format!("gh auth token: {stderr}")))
        };
        cap.stdout.zeroize();
        res
    }
}

/// A resolved `gh-cli` credential: the token plus the account it belongs
/// to (for U3 to record as `cred_account`) and that account's scopes (for
/// the "broader than needed" label, D9).
#[derive(Clone)]
pub struct GhCliCredential {
    host: String,
    account: String,
    scopes: Vec<String>,
    https: HttpsCredential,
}

impl fmt::Debug for GhCliCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GhCliCredential")
            .field("host", &self.host)
            .field("account", &self.account)
            .field("scopes", &self.scopes)
            .field("https", &self.https)
            .finish()
    }
}

fn same_login(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

impl GhCliCredential {
    /// Resolve per D12 (see the module doc). `url` must be https.
    /// `pinned_user` = config `gh_user`; `recorded_account` = the store's
    /// persisted `cred_account` (checked only when nothing is pinned).
    pub fn acquire(
        gh: &GhCli,
        url: &RemoteUrl,
        pinned_user: Option<&str>,
        recorded_account: Option<&str>,
    ) -> Result<Self, CredError> {
        let scope = CredentialScope::for_url(url)
            .ok_or_else(|| CredError::Refused("gh-cli needs an https remote".into()))?;
        let host = url.host().to_string();
        let accounts = gh.accounts(&host)?;
        if accounts.is_empty() {
            return Err(CredError::GhNotLoggedIn { host });
        }
        let active = accounts.iter().find(|a| a.active);
        let chosen = match pinned_user {
            Some(want) => accounts
                .iter()
                .find(|a| same_login(&a.login, want))
                .ok_or_else(|| CredError::AccountMismatch {
                    host: host.clone(),
                    expected: want.to_string(),
                    found: active
                        .map(|a| a.login.clone())
                        .unwrap_or_else(|| accounts[0].login.clone()),
                })?,
            None => {
                let a = active.ok_or_else(|| CredError::GhNotLoggedIn { host: host.clone() })?;
                if let Some(rec) = recorded_account {
                    if !same_login(rec, &a.login) {
                        return Err(CredError::AccountMismatch {
                            host,
                            expected: rec.to_string(),
                            found: a.login.clone(),
                        });
                    }
                }
                a
            }
        };
        let token = gh.token(&host, &chosen.login)?;
        Ok(Self {
            https: HttpsCredential::new(scope, DEFAULT_TOKEN_USERNAME, token)?,
            host,
            account: chosen.login.clone(),
            scopes: chosen.scopes.clone(),
        })
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    /// The gh login this token belongs to — U3 persists it as
    /// `cred_account`.
    pub fn account(&self) -> &str {
        &self.account
    }

    pub fn scopes(&self) -> &[String] {
        &self.scopes
    }

    /// D9: true when the token can do more than kb uses it for (fetch +
    /// GET). kb labels it, it does not refuse it.
    pub fn broader_than_needed(&self) -> bool {
        self.scopes.iter().any(|s| {
            s.starts_with("admin:")
                || matches!(
                    s.as_str(),
                    "repo" | "workflow" | "delete_repo" | "write:packages" | "gist" | "user"
                )
        })
    }

    pub fn https(&self) -> &HttpsCredential {
        &self.https
    }

    /// The same token for the api slot.
    pub fn api_credential(&self) -> ApiCredential {
        ApiCredential {
            token: self.https.token.clone(),
            source: ApiCredentialSource::GhCli {
                account: self.account.clone(),
            },
        }
    }
}

// ---------------------------------------------------------------------
// api slot
// ---------------------------------------------------------------------

/// Where an [`ApiCredential`] came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiCredentialSource {
    /// The daemon's own `gh-cli` read (pinned/recorded account).
    GhCli { account: String },
    /// An owner-only `token_file`.
    TokenFile,
    /// Handed in by the caller for one request — today's
    /// `--gh-token-from-cli` / `CreateReviewPrBody.gh_token` relay,
    /// loopback-only (`github::admit_cli_github_token`).
    CallerSupplied,
}

/// A bearer token for forge REST GETs. Memory only; never persisted,
/// never logged (`Debug` redacts).
#[derive(Clone)]
pub struct ApiCredential {
    token: SecretToken,
    source: ApiCredentialSource,
}

impl fmt::Debug for ApiCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiCredential")
            .field("source", &self.source)
            .field("token", &self.token)
            .finish()
    }
}

impl ApiCredential {
    /// Read a gh-cli token on demand for one API request (D12 account
    /// rules apply, as for the fetch slot).
    pub fn from_gh_cli(
        gh: &GhCli,
        url: &RemoteUrl,
        pinned_user: Option<&str>,
        recorded_account: Option<&str>,
    ) -> Result<Self, CredError> {
        Ok(GhCliCredential::acquire(gh, url, pinned_user, recorded_account)?.api_credential())
    }

    /// Wrap a caller-supplied token (the `--gh-token-from-cli` rung).
    pub fn caller_supplied(token: &str) -> Result<Self, CredError> {
        Ok(Self {
            token: SecretToken::new(token)?,
            source: ApiCredentialSource::CallerSupplied,
        })
    }

    pub fn source(&self) -> &ApiCredentialSource {
        &self.source
    }

    /// For the `Authorization: Bearer` header only.
    pub fn bearer_token(&self) -> &str {
        self.token.expose_secret()
    }
}

// ---------------------------------------------------------------------
// fetch ladder
// ---------------------------------------------------------------------

/// `[[review.repos]] credential`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CredentialPin {
    #[default]
    Auto,
    GhCli,
    DeployKey,
    Token,
    Anonymous,
    Inherit,
    None,
}

impl CredentialPin {
    /// Tolerant parse (README §11): an unknown value falls back to `auto`
    /// and returns a warning for the caller to log.
    pub fn parse_tolerant(s: &str) -> (Self, Option<String>) {
        let v = match s.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Self::Auto,
            "gh-cli" => Self::GhCli,
            "deploy-key" => Self::DeployKey,
            "token" => Self::Token,
            "anonymous" => Self::Anonymous,
            "inherit" => Self::Inherit,
            "none" => Self::None,
            _ => {
                return (
                    Self::Auto,
                    Some("unknown `credential` value; using `auto`".to_string()),
                )
            }
        };
        (v, None)
    }
}

/// A fetch-slot profile kind; `slug()` is what U3 persists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileKind {
    GhCli,
    DeployKey,
    TokenFile,
    Anonymous,
    Inherit,
    None,
}

impl ProfileKind {
    pub fn slug(self) -> &'static str {
        match self {
            Self::GhCli => "gh-cli",
            Self::DeployKey => "deploy-key",
            Self::TokenFile => "token",
            Self::Anonymous => "anonymous",
            Self::Inherit => "inherit",
            Self::None => "none",
        }
    }
}

/// Inputs to [`resolve_fetch_credential`] (from `[[review.repos]]`,
/// `[review.store]`, and the store's persisted `cred_account`).
#[derive(Debug, Clone, Default)]
pub struct FetchCredentialConfig {
    pub pin: CredentialPin,
    pub gh_user: Option<String>,
    /// Already `~`-expanded by the caller.
    pub token_file: Option<PathBuf>,
    pub token_username: Option<String>,
    pub allow_inherited_credentials: bool,
    pub recorded_account: Option<String>,
}

/// The resolved fetch credential.
#[derive(Debug, Clone)]
pub enum FetchCredential {
    GhCli(GhCliCredential),
    TokenFile(HttpsCredential),
    Anonymous,
    Inherit,
    None,
}

impl FetchCredential {
    pub fn kind(&self) -> ProfileKind {
        match self {
            Self::GhCli(_) => ProfileKind::GhCli,
            Self::TokenFile(_) => ProfileKind::TokenFile,
            Self::Anonymous => ProfileKind::Anonymous,
            Self::Inherit => ProfileKind::Inherit,
            Self::None => ProfileKind::None,
        }
    }

    /// The `StoreGit` auth for a NETWORK fetch; `None` for the `none`
    /// profile (cached refs only — the caller reports `no-credentials`).
    pub fn auth(&self) -> Option<FetchAuth<'_>> {
        match self {
            Self::GhCli(g) => Some(FetchAuth::Token(g.https())),
            Self::TokenFile(c) => Some(FetchAuth::Token(c)),
            Self::Anonymous => Some(FetchAuth::Anonymous),
            Self::Inherit => Some(FetchAuth::Inherit),
            Self::None => None,
        }
    }

    /// The gh account, for U3 to persist as `cred_account`.
    pub fn account(&self) -> Option<&str> {
        match self {
            Self::GhCli(g) => Some(g.account()),
            _ => None,
        }
    }

    /// Shown amber in the UI (legacy ambient env).
    pub fn is_amber(&self) -> bool {
        matches!(self, Self::Inherit)
    }
}

/// A rung `auto` skipped, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedRung {
    pub rung: ProfileKind,
    pub class: FailureClass,
    pub reason: String,
}

/// The ladder's answer: the credential, the rung's reason, and every
/// skipped rung (persisted by U3, surfaced by `store show`/doctor).
#[derive(Debug, Clone)]
pub struct Resolution {
    pub credential: FetchCredential,
    pub reason: String,
    pub skipped: Vec<SkippedRung>,
}

/// The side-effecting probes the ladder needs — a trait so the ladder's
/// ORDER and fall-through rules are unit-testable without gh or network.
pub trait LadderProbes {
    fn gh_cli(
        &self,
        url: &RemoteUrl,
        pinned_user: Option<&str>,
        recorded_account: Option<&str>,
    ) -> Result<GhCliCredential, CredError>;
    fn token_file(
        &self,
        path: &Path,
        username: &str,
        url: &RemoteUrl,
    ) -> Result<HttpsCredential, CredError>;
    fn anonymous(&self, url: &RemoteUrl) -> Result<(), CredError>;
}

/// The real probes: `gh` and a scrubbed `ls-remote`.
pub struct LiveProbes<'a> {
    pub gh: &'a GhCli,
    pub git: &'a StoreGit,
}

impl LadderProbes for LiveProbes<'_> {
    fn gh_cli(
        &self,
        url: &RemoteUrl,
        pinned_user: Option<&str>,
        recorded_account: Option<&str>,
    ) -> Result<GhCliCredential, CredError> {
        GhCliCredential::acquire(self.gh, url, pinned_user, recorded_account)
    }

    fn token_file(
        &self,
        path: &Path,
        username: &str,
        url: &RemoteUrl,
    ) -> Result<HttpsCredential, CredError> {
        read_token_file(path, username, url)
    }

    fn anonymous(&self, url: &RemoteUrl) -> Result<(), CredError> {
        self.git
            .ls_remote(url, FetchAuth::Anonymous, &["HEAD"], LS_REMOTE_TIMEOUT)
            .map(|_| ())
            .map_err(CredError::from)
    }
}

/// Read an owner-only token file (0600/0400; the same mode rule as
/// `[github] token_file`) into an [`HttpsCredential`] for `url`'s host.
pub fn read_token_file(
    path: &Path,
    username: &str,
    url: &RemoteUrl,
) -> Result<HttpsCredential, CredError> {
    let scope = CredentialScope::for_url(url)
        .ok_or_else(|| CredError::Refused("a token needs an https remote".into()))?;
    if !crate::config::token_file_mode_ok(path) {
        return Err(CredError::Unavailable(
            "token_file is missing or not owner-only (0600/0400)".into(),
        ));
    }
    let raw = Zeroizing::new(
        std::fs::read_to_string(path)
            .map_err(|e| CredError::Unavailable(format!("token_file unreadable: {}", e.kind())))?,
    );
    HttpsCredential::new(scope, username, SecretToken::new(&raw)?)
}

/// Walk the fetch ladder for a store whose canonical remote is `url`.
/// See the module doc for the rules.
pub fn resolve_fetch_credential(
    cfg: &FetchCredentialConfig,
    url: &RemoteUrl,
    probes: &dyn LadderProbes,
) -> Result<Resolution, CredError> {
    let https = url.https_equivalent();
    let need_https = |rung: &str| {
        https
            .clone()
            .ok_or_else(|| CredError::Refused(format!("{rung} needs an https-capable remote")))
    };
    let username = cfg
        .token_username
        .as_deref()
        .unwrap_or(DEFAULT_TOKEN_USERNAME);
    let done = |credential, reason: String, skipped| Resolution {
        credential,
        reason,
        skipped,
    };

    // 1. explicit pin: that rung or an error.
    match cfg.pin {
        CredentialPin::Auto => {}
        CredentialPin::GhCli => {
            let g = probes.gh_cli(
                &need_https("gh-cli")?,
                cfg.gh_user.as_deref(),
                cfg.recorded_account.as_deref(),
            )?;
            let why = format!("pinned gh-cli ({})", g.account());
            return Ok(done(FetchCredential::GhCli(g), why, vec![]));
        }
        CredentialPin::DeployKey => {
            return Err(CredError::Refused(
                "credential = deploy-key is Phase 2; not available in this build".into(),
            ))
        }
        CredentialPin::Token => {
            let path = cfg
                .token_file
                .as_deref()
                .ok_or_else(|| CredError::Refused("credential = token needs token_file".into()))?;
            let c = probes.token_file(path, username, &need_https("token")?)?;
            return Ok(done(
                FetchCredential::TokenFile(c),
                "pinned token_file".into(),
                vec![],
            ));
        }
        CredentialPin::Anonymous => {
            probes.anonymous(&need_https("anonymous")?)?;
            return Ok(done(
                FetchCredential::Anonymous,
                "pinned anonymous (probed)".into(),
                vec![],
            ));
        }
        CredentialPin::Inherit => {
            if !cfg.allow_inherited_credentials {
                return Err(CredError::Refused(
                    "credential = inherit but [review.store] allow_inherited_credentials = false"
                        .into(),
                ));
            }
            return Ok(done(
                FetchCredential::Inherit,
                "pinned inherit (ambient env)".into(),
                vec![],
            ));
        }
        CredentialPin::None => {
            return Ok(done(FetchCredential::None, "pinned none".into(), vec![]));
        }
    }

    let mut skipped: Vec<SkippedRung> = Vec::new();
    fn skip(skipped: &mut Vec<SkippedRung>, rung: ProfileKind, e: &CredError) {
        skipped.push(SkippedRung {
            rung,
            class: e.class(),
            reason: e.to_string(),
        });
    }

    // 2. gh-cli
    match &https {
        Some(h) => {
            match probes.gh_cli(h, cfg.gh_user.as_deref(), cfg.recorded_account.as_deref()) {
                Ok(g) => {
                    let why = format!("gh-cli ({})", g.account());
                    return Ok(done(FetchCredential::GhCli(g), why, skipped));
                }
                // D12: never a warning, never a fall-through.
                Err(e @ CredError::AccountMismatch { .. }) => return Err(e),
                Err(e) => skip(&mut skipped, ProfileKind::GhCli, &e),
            }
        }
        None => skip(
            &mut skipped,
            ProfileKind::GhCli,
            &CredError::Refused("gh-cli needs an https-capable remote".into()),
        ),
    }

    // 3. deploy key — Phase 2.
    skip(
        &mut skipped,
        ProfileKind::DeployKey,
        &CredError::Refused("deploy keys are Phase 2".into()),
    );

    // 4. token_file
    if let (Some(path), Some(h)) = (cfg.token_file.as_deref(), &https) {
        match probes.token_file(path, username, h) {
            Ok(c) => {
                return Ok(done(
                    FetchCredential::TokenFile(c),
                    "token_file".into(),
                    skipped,
                ))
            }
            Err(e) => skip(&mut skipped, ProfileKind::TokenFile, &e),
        }
    }

    // 5. anonymous (probed)
    match &https {
        Some(h) => match probes.anonymous(h) {
            Ok(()) => {
                return Ok(done(
                    FetchCredential::Anonymous,
                    "anonymous https (ls-remote probe succeeded)".into(),
                    skipped,
                ))
            }
            Err(e) => skip(&mut skipped, ProfileKind::Anonymous, &e),
        },
        None => skip(
            &mut skipped,
            ProfileKind::Anonymous,
            &CredError::Refused("anonymous needs an https-capable remote".into()),
        ),
    }

    // 6. inherit
    if cfg.allow_inherited_credentials {
        return Ok(done(
            FetchCredential::Inherit,
            "inherit (legacy ambient env; allow_inherited_credentials = true)".into(),
            skipped,
        ));
    }
    skip(
        &mut skipped,
        ProfileKind::Inherit,
        &CredError::Refused("allow_inherited_credentials = false".into()),
    );

    // 7. none
    Ok(done(
        FetchCredential::None,
        "no credential rung applies".into(),
        skipped,
    ))
}

#[cfg(test)]
pub(crate) mod tests;
