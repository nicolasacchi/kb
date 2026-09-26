//! RS-U2 — typed failure classes for store git/credential operations, and
//! the `LC_ALL=C` stderr classifier (design-internal-store §4.4).
//!
//! Every class has a STABLE slug, used verbatim as the
//! `urn:kb:errors:<slug>` problem type and as the persisted store/review
//! state. Renaming a slug is a wire break; add a new one instead.
//!
//! The classifier reads git's REDACTED stderr (it never needs a secret to
//! decide) and is ordered most-specific first: an ssh host-key MISMATCH
//! also prints "Host key verification failed", and a disk-full fetch may
//! also print a generic "fatal: …".

use std::fmt;
use std::sync::LazyLock;

use regex::Regex;

/// A quoted URL or path in git's messages (`unable to access '<url>'`,
/// `repository '<url>' not found`). Its CONTENT is attacker/user text — a
/// repo named `openssl-tls` must not classify as `tls` — so it is replaced
/// by a placeholder before any phrase is matched.
static QUOTED_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"'(?:[a-z][a-z0-9+.\-]*://|/|~|git@)[^'\n]*'").expect("static regex")
});

/// Exact TLS phrases from curl/OpenSSL/GnuTLS/NSS/schannel — never a bare
/// `ssl`/`tls` substring.
const TLS_PHRASES: &[&str] = &[
    "ssl certificate",
    "ssl: certificate",
    "certificate problem",
    "certificate verify failed",
    "certificate has expired",
    "unable to get local issuer certificate",
    "self-signed certificate",
    "self signed certificate",
    "server certificate verification failed",
    "ssl_connect",
    "ssl connect error",
    "ssl routines",
    "gnutls_handshake",
    "gnutls recv error",
    "tls handshake",
    "tlsv1 alert",
    "schannel:",
    "openssl ssl_read",
];

/// What kind of credential the failing call carried — the same stderr
/// means different things with and without one (a 401 after we SENT a
/// token is a rejected credential; without one it is "auth required").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthContext {
    /// No credential at all (local-only, anonymous).
    None,
    /// An HTTPS token served by kb's own credential helper.
    Token,
    /// An ssh deploy key (Phase 2).
    DeployKey,
    /// The ambient environment (`inherit`) — we cannot know what it sent.
    Ambient,
}

/// A failure class. See [`FailureClass::slug`] for the wire names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureClass {
    /// The requested ref (or object) is not on the remote.
    Vanished,
    /// DNS, connection refused/reset, unreachable network.
    Offline,
    /// The per-call deadline fired; the process group was killed.
    Timeout,
    /// The credential kb supplied was refused.
    CredentialRejected,
    /// A deploy key answered "repository not found" (key for another repo).
    CredentialWrongRepo,
    /// The remote says the repository does not exist (or hides it), and
    /// no credential was sent.
    RepoNotFound,
    /// A token WAS sent and the forge still says "repository not found" —
    /// GitHub's answer for "this account cannot see it". Auth-class.
    AuthNoAccess,
    /// ssh has no pinned host key for the host.
    HostKeyUnknown,
    /// ssh host key CHANGED — never auto-repaired.
    HostKeyMismatch,
    /// The remote wants credentials and none were supplied.
    AuthRequired,
    /// TLS / certificate failure.
    Tls,
    /// ENOSPC while writing objects or refs.
    DiskFull,
    /// A shallow-repository constraint refused the operation.
    Shallow,
    /// git refused the transport (`GIT_ALLOW_PROTOCOL`).
    ProtocolRefused,
    /// A URL failed the store's allowlist (never echoed).
    UrlRejected,
    /// The gh account answering is not the pinned/recorded one (D12).
    CredentialAccountMismatch,
    /// A credential source exists but cannot be read now (locked keyring,
    /// unreadable token file, gh timeout).
    CredentialUnavailable,
    /// No credential rung applies.
    NoCredentials,
    /// The subprocess could not be started at all (binary missing).
    SpawnFailed,
    /// Anything else.
    Failed,
}

impl FailureClass {
    /// The stable wire slug.
    pub fn slug(self) -> &'static str {
        match self {
            Self::Vanished => "vanished",
            Self::Offline => "offline",
            Self::Timeout => "timeout",
            Self::CredentialRejected => "credential-rejected",
            Self::CredentialWrongRepo => "credential-wrong-repo",
            Self::RepoNotFound => "repo-not-found",
            Self::AuthNoAccess => "auth-no-access",
            Self::HostKeyUnknown => "host-key-unknown",
            Self::HostKeyMismatch => "host-key-mismatch",
            Self::AuthRequired => "auth-required",
            Self::Tls => "tls",
            Self::DiskFull => "disk-full",
            Self::Shallow => "shallow",
            Self::ProtocolRefused => "protocol-refused",
            Self::UrlRejected => "url-rejected",
            Self::CredentialAccountMismatch => "credential-account-mismatch",
            Self::CredentialUnavailable => "credential-unavailable",
            Self::NoCredentials => "no-credentials",
            Self::SpawnFailed => "spawn-failed",
            Self::Failed => "failed",
        }
    }

    /// The class a wire slug names — the exact inverse of [`Self::slug`].
    ///
    /// A `BaseFetch::Skipped`/`Failed` `code` crosses the wire as a bare
    /// string, so a consumer that must tell a CREDENTIAL skip (`offline`
    /// is not one) from a benign one has to parse it. `None` for a code
    /// that names no class at all — the structural skips
    /// (`no-base-remote`, `offline-seed`, `adopted-existing`,
    /// `store-disabled`), which are exactly the ones that must NOT read
    /// as failures.
    ///
    /// Exhaustive by construction: `from_slug`/`slug` are two `match`es
    /// over the same variants, and `from_slug_is_the_exact_inverse_of_slug`
    /// in the tests fails if a new class is added to one and not the
    /// other. Use [`Self::is_auth`] on the result rather than comparing
    /// slugs, so the credential set stays defined in ONE place.
    pub fn from_slug(slug: &str) -> Option<Self> {
        Some(match slug {
            "vanished" => Self::Vanished,
            "offline" => Self::Offline,
            "timeout" => Self::Timeout,
            "credential-rejected" => Self::CredentialRejected,
            "credential-wrong-repo" => Self::CredentialWrongRepo,
            "repo-not-found" => Self::RepoNotFound,
            "auth-no-access" => Self::AuthNoAccess,
            "host-key-unknown" => Self::HostKeyUnknown,
            "host-key-mismatch" => Self::HostKeyMismatch,
            "auth-required" => Self::AuthRequired,
            "tls" => Self::Tls,
            "disk-full" => Self::DiskFull,
            "shallow" => Self::Shallow,
            "protocol-refused" => Self::ProtocolRefused,
            "url-rejected" => Self::UrlRejected,
            "credential-account-mismatch" => Self::CredentialAccountMismatch,
            "credential-unavailable" => Self::CredentialUnavailable,
            "no-credentials" => Self::NoCredentials,
            "spawn-failed" => Self::SpawnFailed,
            "failed" => Self::Failed,
            _ => return None,
        })
    }

    /// `urn:kb:errors:<slug>`.
    pub fn urn(self) -> String {
        format!("urn:kb:errors:{}", self.slug())
    }

    /// Network-shaped: worth a retry later, and grounds for an offline
    /// fallback to cached refs.
    pub fn is_transient(self) -> bool {
        matches!(self, Self::Offline | Self::Timeout)
    }

    /// Auth-shaped: grounds for re-probing the credential ladder (§5.1).
    ///
    /// This is ALSO the set `store sync` reads a base-fetch skip against: a
    /// skip whose code lands here means a credential could not be resolved,
    /// which D12 calls an error, not a warning. It deliberately excludes
    /// the structural skips (`no-base-remote`, `offline-seed`, …) and the
    /// network classes — `store sync --offline` is a supported, successful
    /// operation, so `Offline` must never join this set.
    pub fn is_auth(self) -> bool {
        matches!(
            self,
            Self::CredentialRejected
                | Self::CredentialWrongRepo
                | Self::AuthNoAccess
                | Self::AuthRequired
                | Self::CredentialAccountMismatch
                | Self::CredentialUnavailable
                | Self::NoCredentials
        )
    }
}

impl fmt::Display for FailureClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.slug())
    }
}

/// Classify a (redacted, `LC_ALL=C`) git stderr.
pub fn classify(stderr: &str, auth: AuthContext) -> FailureClass {
    let lower = stderr.to_ascii_lowercase();
    let s = QUOTED_URL.replace_all(&lower, "'<url>'");
    let has = |n: &str| s.contains(n);

    if has("no space left on device") || has("enospc") || has("disk quota exceeded") {
        return FailureClass::DiskFull;
    }
    if has("remote host identification has changed") {
        return FailureClass::HostKeyMismatch;
    }
    if has("host key verification failed")
        || has("no ecdsa host key is known")
        || has("no ed25519 host key is known")
        || has("no rsa host key is known")
    {
        return FailureClass::HostKeyUnknown;
    }
    if has("transport '") && has("' not allowed") {
        return FailureClass::ProtocolRefused;
    }
    if has("couldn't find remote ref") || has("not our ref") || has("no such remote ref") {
        return FailureClass::Vanished;
    }
    if has("shallow update not allowed")
        || has("attempt to fetch/clone from a shallow repository")
        || has("is a shallow repository")
    {
        return FailureClass::Shallow;
    }
    if has("permission denied (publickey") {
        return FailureClass::CredentialRejected;
    }
    if has("repository not found") || (has("repository '") && has("' not found")) {
        return match auth {
            AuthContext::DeployKey => FailureClass::CredentialWrongRepo,
            AuthContext::Token => FailureClass::AuthNoAccess,
            AuthContext::None | AuthContext::Ambient => FailureClass::RepoNotFound,
        };
    }
    if TLS_PHRASES.iter().any(|p| has(p)) {
        return FailureClass::Tls;
    }
    let auth_shaped = has("authentication failed")
        || has("could not read username")
        || has("could not read password")
        || has("invalid username or")
        || has("returned error: 401")
        || has("returned error: 403")
        || has("http 401")
        || has("http 403");
    if auth_shaped {
        return if auth == AuthContext::Token || auth == AuthContext::DeployKey {
            FailureClass::CredentialRejected
        } else {
            FailureClass::AuthRequired
        };
    }
    if has("timed out") || has("timeout was reached") {
        return FailureClass::Timeout;
    }
    if has("could not resolve host")
        || has("could not resolve hostname")
        || has("name or service not known")
        || has("temporary failure in name resolution")
        || has("failed to connect")
        || has("could not connect to server")
        || has("couldn't connect to server")
        || has("connection refused")
        || has("connection reset")
        || has("network is unreachable")
        || has("no route to host")
        || has("the remote end hung up unexpectedly")
        || has("rpc failed")
        || has("could not read from remote repository")
    {
        return FailureClass::Offline;
    }
    FailureClass::Failed
}

#[cfg(test)]
mod tests {
    use super::*;
    use AuthContext as A;
    use FailureClass as F;

    /// Real `LC_ALL=C` git stderr, captured from git 2.55 / OpenSSH 10 on
    /// synthetic repos (the fixture names are illustrative).
    const FIXTURES: &[(&str, AuthContext, FailureClass)] = &[
        ("fatal: couldn't find remote ref refs/heads/nope\n", A::None, F::Vanished),
        (
            "fatal: remote error: upload-pack: not our ref deadbeefdeadbeefdeadbeefdeadbeefdeadbeef\nfatal: git upload-pack: not our ref deadbeefdeadbeefdeadbeefdeadbeefdeadbeef\n",
            A::None,
            F::Vanished,
        ),
        (
            "fatal: unable to access 'https://nosuchhost.invalid/x.git/': Could not resolve host: nosuchhost.invalid\n",
            A::None,
            F::Offline,
        ),
        (
            "fatal: unable to access 'https://127.0.0.1:1/x.git/': Failed to connect to 127.0.0.1:1 after 0 ms: Could not connect to server\n",
            A::Token,
            F::Offline,
        ),
        (
            "ssh: connect to host github.com port 22: Connection timed out\nfatal: Could not read from remote repository.\n",
            A::DeployKey,
            F::Timeout,
        ),
        (
            "fatal: could not read Username for 'https://github.com': terminal prompts disabled\n",
            A::None,
            F::AuthRequired,
        ),
        (
            "remote: Invalid username or token. Password authentication is not supported for Git operations.\nfatal: Authentication failed for 'https://github.com/acme/widgets.git/'\n",
            A::Token,
            F::CredentialRejected,
        ),
        (
            "fatal: unable to access 'https://github.com/acme/widgets.git/': The requested URL returned error: 403\n",
            A::None,
            F::AuthRequired,
        ),
        (
            "remote: Repository not found.\nfatal: repository 'https://github.com/acme/widgets.git/' not found\n",
            A::Token,
            F::AuthNoAccess,
        ),
        (
            "remote: Repository not found.\nfatal: repository 'https://github.com/acme/widgets.git/' not found\n",
            A::None,
            F::RepoNotFound,
        ),
        // A repo NAME must not drive the class: `openssl-tls-certificate`
        // inside the quoted URL is not a TLS failure.
        (
            "fatal: unable to access 'https://github.com/acme/openssl-tls-certificate.git/': Could not resolve host: github.com\n",
            A::Token,
            F::Offline,
        ),
        (
            "fatal: repository 'https://github.com/acme/permission-denied-publickey-no-space-left-on-device.git/' not found\n",
            A::None,
            F::RepoNotFound,
        ),
        (
            "fatal: unable to access 'https://git.example.com/x.git/': SSL certificate problem: unable to get local issuer certificate\n",
            A::Token,
            F::Tls,
        ),
        (
            "fatal: unable to access 'https://git.example.com/x.git/': gnutls_handshake() failed: The TLS connection was non-properly terminated.\n",
            A::None,
            F::Tls,
        ),
        (
            "ERROR: Repository not found.\nfatal: Could not read from remote repository.\n\nPlease make sure you have the correct access rights\nand the repository exists.\n",
            A::DeployKey,
            F::CredentialWrongRepo,
        ),
        (
            "git@github.com: Permission denied (publickey).\nfatal: Could not read from remote repository.\n",
            A::DeployKey,
            F::CredentialRejected,
        ),
        (
            "No ED25519 host key is known for github.com and you have requested strict checking.\nHost key verification failed.\nfatal: Could not read from remote repository.\n",
            A::DeployKey,
            F::HostKeyUnknown,
        ),
        (
            "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\nHost key verification failed.\n",
            A::DeployKey,
            F::HostKeyMismatch,
        ),
        (
            "fatal: unable to access 'https://expired.example.com/x.git/': SSL certificate OpenSSL verify result: certificate has expired (10)\n",
            A::None,
            F::Tls,
        ),
        (
            "error: unable to write file ./objects/pack/tmp_pack_abc: No space left on device\nfatal: fetch-pack: invalid index-pack output\n",
            A::Token,
            F::DiskFull,
        ),
        (
            "fatal: attempt to fetch/clone from a shallow repository\n",
            A::None,
            F::Shallow,
        ),
        (
            " ! [rejected]        main       -> base/main  (shallow update not allowed)\n",
            A::Token,
            F::Shallow,
        ),
        ("fatal: transport 'ext' not allowed\n", A::None, F::ProtocolRefused),
        ("fatal: transport 'http' not allowed\n", A::Token, F::ProtocolRefused),
        (
            "error: RPC failed; curl 92 HTTP/2 stream 5 was not closed cleanly: CANCEL (err 8)\nfatal: expected flush after ref listing\n",
            A::Token,
            F::Offline,
        ),
        ("fatal: bad object HEAD\n", A::None, F::Failed),
    ];

    #[test]
    fn the_classifier_matches_the_real_stderr_fixtures() {
        for (stderr, ctx, want) in FIXTURES {
            assert_eq!(classify(stderr, *ctx), *want, "stderr: {stderr:?}");
        }
    }

    #[test]
    fn slugs_are_stable_and_urn_shaped() {
        assert_eq!(
            F::CredentialAccountMismatch.urn(),
            "urn:kb:errors:credential-account-mismatch"
        );
        assert_eq!(F::DiskFull.slug(), "disk-full");
        assert_eq!(F::Vanished.slug(), "vanished");
        assert_eq!(F::Timeout.slug(), "timeout");
        assert!(F::Offline.is_transient());
        assert!(F::AuthRequired.is_auth());
    }

    /// Every class, so the `slug`/`from_slug` pair can be checked for
    /// being inverses. A class added to the enum and to `slug()` but
    /// forgotten in `from_slug()` would make a real failure code parse as
    /// "no class at all" — i.e. silently benign, which is the exact defect
    /// `from_slug` exists to prevent.
    const ALL: &[F] = &[
        F::Vanished,
        F::Offline,
        F::Timeout,
        F::CredentialRejected,
        F::CredentialWrongRepo,
        F::RepoNotFound,
        F::AuthNoAccess,
        F::HostKeyUnknown,
        F::HostKeyMismatch,
        F::AuthRequired,
        F::Tls,
        F::DiskFull,
        F::Shallow,
        F::ProtocolRefused,
        F::UrlRejected,
        F::CredentialAccountMismatch,
        F::CredentialUnavailable,
        F::NoCredentials,
        F::SpawnFailed,
        F::Failed,
    ];

    #[test]
    fn from_slug_is_the_exact_inverse_of_slug() {
        for &c in ALL {
            assert_eq!(F::from_slug(c.slug()), Some(c), "slug {}", c.slug());
        }
        // A structural skip names no class: parsing it must yield `None`,
        // never a fallback that could be mistaken for a failure.
        for code in [
            "no-base-remote",
            "offline-seed",
            "no-base-branches",
            "adopted-existing",
            "store-disabled",
            "",
            "Offline",
        ] {
            assert_eq!(F::from_slug(code), None, "{code:?} must name no class");
        }
    }

    /// The two halves `store sync` decides on, read through the wire slug
    /// a `BaseFetch::Skipped { code }` actually carries: a credential skip
    /// is an ERROR (D12), a structural or offline skip is not.
    #[test]
    fn is_auth_separates_a_credential_skip_from_a_benign_one() {
        for code in [
            "credential-account-mismatch",
            "credential-rejected",
            "credential-wrong-repo",
            "credential-unavailable",
            "no-credentials",
            "auth-required",
            "auth-no-access",
        ] {
            assert_eq!(
                F::from_slug(code).map(F::is_auth),
                Some(true),
                "{code} is a credential failure and must not read as benign"
            );
        }
        for code in ["offline", "timeout", "vanished", "tls", "repo-not-found"] {
            assert_eq!(
                F::from_slug(code).map(F::is_auth),
                Some(false),
                "{code} is not a credential failure"
            );
        }
        assert_eq!(F::from_slug("no-base-remote").map(F::is_auth), None);
    }
}
