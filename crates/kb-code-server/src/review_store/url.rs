//! RS-U2 — the store's URL allowlist and the validated argv atoms
//! ([`RemoteUrl`], [`RemoteName`], [`RefName`], [`FetchRefspec`]) that are
//! the ONLY dynamic values [`super::git::StoreGit`] will place in argv.
//!
//! # URL allowlist (design-internal-store §5.5, README §8)
//!
//! A REMOTE (network) URL must be one of
//!
//! * `https://host[:port]/path` — no userinfo at all (a token in a URL is
//!   visible in `/proc/<pid>/cmdline` and in every error git prints);
//! * `ssh://git@host[:port]/path` or scp-form `git@host:path` — the user
//!   must be `git` (the forge-deploy-key convention; anything else is a
//!   personal account smuggled into the store).
//!
//! Everything else is rejected, including `file://`, plain `http://`, any
//! `<transport>::<address>` remote-helper form (`ext::`, `fd::`), control
//! characters and whitespace, and IPv6 literals (they need `::`).
//!
//! A LOCAL seed source (the user's clone, design §3.2) goes through a
//! separate constructor, [`RemoteUrl::local_seed`]: an absolute path, and
//! only ever used with the `file`-only protocol allowance.
//!
//! No validator here checks for a leading `-` directly: every accepted form
//! begins with a fixed scheme, `git@`, or `/`, so an option-shaped value is
//! rejected by construction (and the SEC-17 lint keeps the dash predicate
//! in `git/revspec.rs` alone). [`RefName`] delegates to
//! [`crate::git::Revspec::parse`] for exactly that reason.
//!
//! A rejected URL is NEVER echoed: [`UrlRejected`] carries only the rule it
//! broke.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::git::Revspec;

/// Why a URL is not allowed. Carries no part of the URL on purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UrlRejected {
    #[error("url is empty")]
    Empty,
    #[error("url contains a control or whitespace character")]
    ControlOrSpace,
    #[error("remote-helper transports (`<name>::`) are not allowed")]
    TransportHelper,
    #[error("file:// URLs are not allowed for a remote")]
    FileScheme,
    #[error("plain http:// is not allowed; use https://")]
    PlainHttp,
    #[error("unsupported URL form (allowed: https://, ssh://git@, git@host:path)")]
    Unsupported,
    #[error("https URLs must not carry userinfo (credentials go through the credential helper)")]
    Userinfo,
    #[error("ssh URLs must use the `git` user")]
    SshUserNotGit,
    #[error("invalid host")]
    BadHost,
    #[error("invalid port")]
    BadPort,
    #[error("invalid repository path")]
    BadPath,
    #[error("a local seed source must be an absolute UTF-8 path")]
    NotAbsolute,
}

/// Which git transport a URL uses — maps 1:1 onto a `GIT_ALLOW_PROTOCOL`
/// entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Https,
    Ssh,
    File,
    /// Test-only: plain http to a loopback fixture server.
    #[cfg(test)]
    Http,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Https => "https",
            Self::Ssh => "ssh",
            Self::File => "file",
            #[cfg(test)]
            Self::Http => "http",
        }
    }
}

/// A URL that passed the allowlist. See the module doc.
#[derive(Clone, PartialEq, Eq)]
pub struct RemoteUrl {
    raw: String,
    protocol: Protocol,
    /// `host` or `host:port` (empty for a local path).
    authority: String,
    host: String,
    /// Repository path without a leading `/` (for scp form, as written).
    path: String,
}

impl fmt::Debug for RemoteUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // No userinfo can survive parsing, so the raw form is safe.
        write!(f, "RemoteUrl({})", self.raw)
    }
}

impl fmt::Display for RemoteUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

fn host_ok(h: &str) -> bool {
    let b = h.as_bytes();
    !b.is_empty()
        && b.len() <= 253
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b'.' || *c == b'-')
        && !h.ends_with('.')
        && !h.contains("..")
}

fn port_ok(p: &str) -> bool {
    !p.is_empty() && p.len() <= 5 && p.bytes().all(|c| c.is_ascii_digit()) && {
        let n: u32 = p.parse().unwrap_or(0);
        (1..=65535).contains(&n)
    }
}

/// `host` or `host:port`.
fn authority_ok(a: &str) -> Result<String, UrlRejected> {
    let (host, port) = match a.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (a, None),
    };
    if !host_ok(host) {
        return Err(UrlRejected::BadHost);
    }
    if let Some(p) = port {
        if !port_ok(p) {
            return Err(UrlRejected::BadPort);
        }
    }
    Ok(host.to_ascii_lowercase())
}

fn path_ok(p: &str) -> bool {
    let b = p.as_bytes();
    !b.is_empty()
        && (b[0].is_ascii_alphanumeric() || b[0] == b'~' || b[0] == b'_')
        && b.iter().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(*c, b'.' | b'_' | b'-' | b'/' | b'~' | b'+' | b'%')
        })
        && !p.split('/').any(|seg| seg == "..")
}

impl RemoteUrl {
    /// Validate a NETWORK remote URL (https or ssh). See the module doc.
    pub fn parse_remote(s: &str) -> Result<Self, UrlRejected> {
        if s.is_empty() {
            return Err(UrlRejected::Empty);
        }
        if s.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err(UrlRejected::ControlOrSpace);
        }
        if s.contains("::") {
            return Err(UrlRejected::TransportHelper);
        }
        let lower = s.to_ascii_lowercase();
        if lower.starts_with("file:") {
            return Err(UrlRejected::FileScheme);
        }
        if lower.starts_with("http://") {
            return Err(UrlRejected::PlainHttp);
        }
        if lower.starts_with("https://") {
            let rest = &s["https://".len()..];
            let (auth, path) = rest.split_once('/').ok_or(UrlRejected::BadPath)?;
            if auth.contains('@') {
                return Err(UrlRejected::Userinfo);
            }
            let host = authority_ok(auth)?;
            if !path_ok(path) {
                return Err(UrlRejected::BadPath);
            }
            return Ok(Self {
                raw: format!("https://{}/{}", auth.to_ascii_lowercase(), path),
                protocol: Protocol::Https,
                authority: auth.to_ascii_lowercase(),
                host,
                path: path.to_string(),
            });
        }
        if lower.starts_with("ssh://") {
            let rest = &s["ssh://".len()..];
            let (auth, path) = rest.split_once('/').ok_or(UrlRejected::BadPath)?;
            let (user, hostport) = auth.split_once('@').ok_or(UrlRejected::SshUserNotGit)?;
            if user != "git" {
                return Err(UrlRejected::SshUserNotGit);
            }
            let host = authority_ok(hostport)?;
            if !path_ok(path) {
                return Err(UrlRejected::BadPath);
            }
            return Ok(Self {
                raw: format!("ssh://git@{}/{}", hostport.to_ascii_lowercase(), path),
                protocol: Protocol::Ssh,
                authority: hostport.to_ascii_lowercase(),
                host,
                path: path.to_string(),
            });
        }
        if lower.contains("://") {
            return Err(UrlRejected::Unsupported);
        }
        // scp form: `git@host:path` — the `:` must come before any `/`
        // (git's own rule for telling scp form from a local path).
        if let Some(rest) = s.strip_prefix("git@") {
            let (host, path) = rest.split_once(':').ok_or(UrlRejected::Unsupported)?;
            if host.contains('/') {
                return Err(UrlRejected::Unsupported);
            }
            let host_l = authority_ok(host)?;
            let path_trim = path.strip_prefix('/').unwrap_or(path);
            if !path_ok(path_trim) {
                return Err(UrlRejected::BadPath);
            }
            return Ok(Self {
                raw: format!("git@{host_l}:{path}"),
                protocol: Protocol::Ssh,
                authority: host_l.clone(),
                host: host_l,
                path: path_trim.to_string(),
            });
        }
        if s.contains('@') && s.contains(':') {
            return Err(UrlRejected::SshUserNotGit);
        }
        Err(UrlRejected::Unsupported)
    }

    /// A local seed source (the user's clone, by absolute path). Only ever
    /// fetched from with the `file`-only protocol allowance.
    pub fn local_seed(p: &Path) -> Result<Self, UrlRejected> {
        let s = p.to_str().ok_or(UrlRejected::NotAbsolute)?;
        if !p.is_absolute() {
            return Err(UrlRejected::NotAbsolute);
        }
        if s.chars().any(|c| c.is_control()) {
            return Err(UrlRejected::ControlOrSpace);
        }
        if s.contains("::") {
            return Err(UrlRejected::TransportHelper);
        }
        Ok(Self {
            raw: s.to_string(),
            protocol: Protocol::File,
            authority: String::new(),
            host: String::new(),
            path: s.to_string(),
        })
    }

    /// Test-only: `http://127.0.0.1:<port>/<path>` for a loopback fixture
    /// server. Production code cannot construct an http URL.
    #[cfg(test)]
    pub(crate) fn test_http_loopback(port: u16, path: &str) -> Self {
        Self {
            raw: format!("http://127.0.0.1:{port}/{path}"),
            protocol: Protocol::Http,
            authority: format!("127.0.0.1:{port}"),
            host: "127.0.0.1".into(),
            path: path.into(),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    pub fn protocol(&self) -> Protocol {
        self.protocol
    }

    /// Lowercased host (no port). Empty for a local path.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// `host[:port]` — what git's credential protocol calls `host=`.
    pub fn authority(&self) -> &str {
        &self.authority
    }

    /// Repository path (no leading `/`).
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The local path, for a [`Self::local_seed`] URL.
    pub fn local_path(&self) -> Option<PathBuf> {
        (self.protocol == Protocol::File).then(|| PathBuf::from(&self.raw))
    }

    /// The HTTPS form of an ssh URL on the same host (README §8: "the
    /// store's transport URL is the HTTPS form"). An explicit ssh PORT is
    /// dropped (it is an ssh port, meaningless for https). `None` for a
    /// local path. https URLs return themselves.
    pub fn https_equivalent(&self) -> Option<RemoteUrl> {
        match self.protocol {
            Protocol::Https => Some(self.clone()),
            Protocol::Ssh => {
                let path = if self.path.ends_with(".git") {
                    self.path.clone()
                } else {
                    format!("{}.git", self.path)
                };
                RemoteUrl::parse_remote(&format!("https://{}/{}", self.host, path)).ok()
            }
            _ => None,
        }
    }
}

/// A remote NAME in the store config (`base`, `work-<repo_id>`, a probe
/// name). `[a-z][a-z0-9-]{0,62}` — argv carries names, never URLs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RemoteName(String);

impl RemoteName {
    pub fn parse(s: &str) -> Result<Self, UrlRejected> {
        let b = s.as_bytes();
        if b.is_empty()
            || b.len() > 63
            || !b[0].is_ascii_lowercase()
            || !b
                .iter()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        {
            return Err(UrlRejected::Unsupported);
        }
        Ok(Self(s.to_string()))
    }

    /// `base` — the forge remote, written only by credentialed fetches.
    pub fn base() -> Self {
        Self("base".into())
    }

    /// `work-<repo_id>` — a member clone, local-only.
    pub fn work(repo_id: i64) -> Self {
        Self(format!("work-{}", repo_id.unsigned_abs()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RemoteName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a ref name / refspec is invalid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid ref name")]
pub struct RefNameError;

/// A full ref name (`refs/…`) valid as a refspec side: the
/// [`Revspec`] predicate (no leading dash, no `..`, no `@{`, no control
/// or whitespace) PLUS git's `check-ref-format` characters that are legal
/// in a revspec but not in a ref name (`: ? * [ \ ^ ~`), no `//`, no
/// trailing `/`, `.` or `.lock`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RefName(String);

impl RefName {
    pub fn parse(s: &str) -> Result<Self, RefNameError> {
        Revspec::parse(s).map_err(|_| RefNameError)?;
        let ok = s.starts_with("refs/")
            && s.len() > "refs/".len()
            && !s.contains(['\\', ':', '?', '*', '[', '^', '~'])
            && !s.contains("//")
            && !s.ends_with('/')
            && !s.ends_with('.')
            && !s.ends_with(".lock")
            && !s.split('/').any(|c| c.starts_with('.') || c.is_empty());
        if !ok {
            return Err(RefNameError);
        }
        Ok(Self(s.to_string()))
    }

    /// `refs/heads/<branch>` from a bare branch name.
    pub fn branch(branch: &str) -> Result<Self, RefNameError> {
        Self::parse(&format!("refs/heads/{branch}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The source side of a fetch refspec: a ref name, or a full object id
/// (fetch-by-sha for detached work tips, design §3.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefSource {
    Ref(RefName),
    Oid(String),
}

impl RefSource {
    /// A 40- or 64-hex object id.
    pub fn oid(hex: &str) -> Result<Self, RefNameError> {
        let ok = (hex.len() == 40 || hex.len() == 64)
            && hex
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
        if !ok {
            return Err(RefNameError);
        }
        Ok(Self::Oid(hex.to_string()))
    }

    fn as_str(&self) -> &str {
        match self {
            Self::Ref(r) => r.as_str(),
            Self::Oid(o) => o,
        }
    }
}

/// An explicit fetch refspec `[+]src:dst`. No wildcards, ever: every store
/// fetch names exactly what it writes (design §3.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchRefspec {
    force: bool,
    src: RefSource,
    dst: RefName,
}

impl FetchRefspec {
    pub fn new(force: bool, src: RefSource, dst: RefName) -> Self {
        Self { force, src, dst }
    }

    pub fn dst(&self) -> &RefName {
        &self.dst
    }

    /// The argv form.
    pub fn as_arg(&self) -> String {
        format!(
            "{}{}:{}",
            if self.force { "+" } else { "" },
            self.src.as_str(),
            self.dst.as_str()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_remote_forms() {
        let u = RemoteUrl::parse_remote("https://github.com/acme/widgets.git").unwrap();
        assert_eq!(u.protocol(), Protocol::Https);
        assert_eq!(u.host(), "github.com");
        assert_eq!(u.path(), "acme/widgets.git");
        let u = RemoteUrl::parse_remote("https://GitHub.com:8443/acme/widgets").unwrap();
        assert_eq!(u.authority(), "github.com:8443");
        let u = RemoteUrl::parse_remote("ssh://git@bitbucket.example.com:7999/acme/widgets.git")
            .unwrap();
        assert_eq!(u.protocol(), Protocol::Ssh);
        assert_eq!(u.host(), "bitbucket.example.com");
        let u = RemoteUrl::parse_remote("git@github.com:acme/widgets.git").unwrap();
        assert_eq!(u.protocol(), Protocol::Ssh);
        assert_eq!(
            u.https_equivalent().unwrap().as_str(),
            "https://github.com/acme/widgets.git"
        );
        let u = RemoteUrl::parse_remote("git@github.com:acme/widgets").unwrap();
        assert_eq!(
            u.https_equivalent().unwrap().as_str(),
            "https://github.com/acme/widgets.git"
        );
    }

    #[test]
    fn rejected_remote_forms_never_echo_the_url() {
        let cases: &[(&str, UrlRejected)] = &[
            ("", UrlRejected::Empty),
            (
                "https://x-access-token:ghp_FAKEaaaaaaaaaaaaaaaaaaaa@github.com/acme/w.git",
                UrlRejected::Userinfo,
            ),
            (
                "https://someone@github.com/acme/w.git",
                UrlRejected::Userinfo,
            ),
            ("ext::sh -c touch% /tmp/pwned", UrlRejected::ControlOrSpace),
            ("ext::sh", UrlRejected::TransportHelper),
            ("fd::17", UrlRejected::TransportHelper),
            ("file:///etc/passwd", UrlRejected::FileScheme),
            ("FILE:///srv/repo", UrlRejected::FileScheme),
            ("http://github.com/acme/w.git", UrlRejected::PlainHttp),
            (
                "-oProxyCommand=touch /tmp/pwned",
                UrlRejected::ControlOrSpace,
            ),
            ("-oProxyCommand=id", UrlRejected::Unsupported),
            ("--upload-pack=id", UrlRejected::Unsupported),
            ("ssh://-oProxyCommand=id/x", UrlRejected::SshUserNotGit),
            ("ssh://git@-oProxyCommand=id/x", UrlRejected::BadHost),
            ("git@-oProxyCommand=id:x", UrlRejected::BadHost),
            (
                "ssh://someone@github.com/acme/w.git",
                UrlRejected::SshUserNotGit,
            ),
            ("root@github.com:acme/w.git", UrlRejected::SshUserNotGit),
            (
                "https://github.com/acme/w.git\nhost=evil",
                UrlRejected::ControlOrSpace,
            ),
            ("https://github.com/acme/../../w.git", UrlRejected::BadPath),
            ("https://github.com/-acme/w.git", UrlRejected::BadPath),
            ("https://github.com:0/acme/w.git", UrlRejected::BadPort),
            ("https://[::1]/acme/w.git", UrlRejected::TransportHelper),
            ("/srv/git/widgets.git", UrlRejected::Unsupported),
            ("../widgets", UrlRejected::Unsupported),
            ("ftp://github.com/acme/w.git", UrlRejected::Unsupported),
        ];
        for (url, want) in cases {
            let got = RemoteUrl::parse_remote(url).expect_err(url);
            assert_eq!(got, *want, "{url}");
            let msg = got.to_string();
            assert!(!msg.contains("ghp_"), "{msg}");
            if url.len() > 6 {
                assert!(!msg.contains(url), "rejection echoed the url: {msg}");
            }
        }
    }

    #[test]
    fn local_seed_requires_an_absolute_path() {
        assert!(RemoteUrl::local_seed(Path::new("/srv/acme/widgets")).is_ok());
        assert_eq!(
            RemoteUrl::local_seed(Path::new("widgets")).unwrap_err(),
            UrlRejected::NotAbsolute
        );
        assert_eq!(
            RemoteUrl::local_seed(Path::new("-oProxy")).unwrap_err(),
            UrlRejected::NotAbsolute
        );
        assert_eq!(
            RemoteUrl::local_seed(Path::new("/srv/a\nb")).unwrap_err(),
            UrlRejected::ControlOrSpace
        );
        let u = RemoteUrl::local_seed(Path::new("/srv/acme/widgets")).unwrap();
        assert_eq!(u.protocol(), Protocol::File);
        assert!(u.https_equivalent().is_none());
    }

    #[test]
    fn remote_names_refs_and_refspecs() {
        assert_eq!(RemoteName::work(7).as_str(), "work-7");
        assert!(RemoteName::parse("base").is_ok());
        assert!(RemoteName::parse("-base").is_err());
        assert!(RemoteName::parse("Base").is_err());
        assert!(RemoteName::parse("a b").is_err());

        assert!(RefName::parse("refs/remotes/base/main").is_ok());
        assert!(RefName::branch("release/2026.09").is_ok());
        for bad in [
            "main",
            "-refs/heads/x",
            "refs/heads/a..b",
            "refs/heads/a:b",
            "refs/heads/*",
            "refs/heads/x.lock",
            "refs/heads/.hidden",
            "refs/heads//x",
            "refs/heads/x/",
            "refs/heads/a b",
            "refs/heads/@{-1}",
            "refs/",
        ] {
            assert!(RefName::parse(bad).is_err(), "{bad}");
        }
        assert!(RefName::branch("--upload-pack=x").is_ok_and(|r| r.as_str().starts_with("refs/")));

        let rs = FetchRefspec::new(
            true,
            RefSource::Ref(RefName::branch("main").unwrap()),
            RefName::parse("refs/remotes/base/main").unwrap(),
        );
        assert_eq!(rs.as_arg(), "+refs/heads/main:refs/remotes/base/main");
        let sha = "0123456789abcdef0123456789abcdef01234567";
        let rs = FetchRefspec::new(
            false,
            RefSource::oid(sha).unwrap(),
            RefName::parse("refs/kbc/review/r1/ps1").unwrap(),
        );
        assert_eq!(rs.as_arg(), format!("{sha}:refs/kbc/review/r1/ps1"));
        assert!(RefSource::oid("xyz").is_err());
        assert!(RefSource::oid(&sha.to_uppercase()).is_err());
    }
}
