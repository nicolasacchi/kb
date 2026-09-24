//! RS-U2 — `StoreGit`: the ONLY git spawner in this crate allowed to carry
//! a credential (README §8, design-internal-store §3.3/§4.4/§5.2/§5.5/§13).
//!
//! # Why a new spawner (and not `history::run_git_raw`)
//!
//! This crate has six private git wrappers (`history`, `diff`, `blame`,
//! `checkout`, `github`, `reviews`), all operating on the USER's working
//! tree with the daemon's ambient environment, unauthenticated. None of
//! them clears the environment, bounds output, or kills on a deadline —
//! and none should grow that, because none of them talks to the network.
//! `StoreGit` is the first hardened one, parallel to them by design: it
//! only ever runs against the kb-owned review store (or a network remote
//! into it) and is the one place a credential can enter a git process.
//!
//! # The contract every call gets (everything except `inherit`)
//!
//! * `env_clear()` then an explicit allowlist: `PATH` (the daemon's),
//!   `HOME`/`XDG_CONFIG_HOME` = a store-owned EMPTY dir, `LC_ALL=C`/
//!   `LANG=C` (the classifier reads English), `GIT_CONFIG_NOSYSTEM=1`,
//!   `GIT_CONFIG_GLOBAL=/dev/null`, `GIT_TERMINAL_PROMPT=0`, empty
//!   `GIT_ASKPASS`/`SSH_ASKPASS` + `SSH_ASKPASS_REQUIRE=never`,
//!   `GIT_OPTIONAL_LOCKS=0`, `GIT_ALLOW_PROTOCOL` = exactly the profile's
//!   transports (never `ext`/`fd`), proxy variables passed through, and
//!   `GIT_DIR` when the call targets the store. `GIT_SSH_COMMAND` is not
//!   set here; the Phase-2 deploy-key profile will own it.
//! * `-c` hardening in argv: every inherited `credential.helper` reset,
//!   `core.hooksPath=/dev/null`, `core.askPass=`, `core.fsmonitor=false`,
//!   `protocol.allow=never` + the profile's transports, `gc.auto=0`,
//!   `maintenance.auto=false`.
//! * A per-call timeout, enforced on the whole PROCESS GROUP
//!   ([`super::proc`]), and bounded stdout/stderr capture.
//! * Every captured stderr is REDACTED ([`super::redact`]) before it is
//!   classified, returned, or logged; failures are typed
//!   ([`super::classify::FailureClass`], stable `urn:kb:errors:<slug>`).
//! * argv carries only static flags and validated atoms ([`GitArgs`]):
//!   remote NAMES, refspecs, ref names, `Revspec`s, allowlisted URLs,
//!   absolute paths — none of which can begin with `-` — and dynamic
//!   positionals follow `--end-of-options` in the helpers below.
//! * NO push: there is no push method, `GitArgs` carrying the push family
//!   is refused at run time, the SEC-17 lint refuses it in source, and
//!   [`StoreGit::configure_remote`] writes `pushurl = kbcode-no-push://refused`
//!   on every remote.
//!
//! # How a token reaches git without touching argv, env, or disk
//!
//! For [`FetchAuth::Token`] the daemon creates a `pipe2(O_CLOEXEC)`,
//! writes `username=…\npassword=…\n` into it, closes the write end, and
//! clears `FD_CLOEXEC` on the read end ONLY inside the forked child
//! (the pre-exec hook), so git — and only git's own process tree —
//! inherits it. The configured helper is a fixed shell snippet that
//! answers a `get` for exactly the scope's protocol + host (anything
//! else: no answer) by `cat <&N`. The snippet contains the fd number and
//! host only; the token exists in the daemon's memory, in the pipe
//! buffer, and in git's memory — never in any `/proc/<pid>/cmdline` or
//! `/proc/<pid>/environ`, never on disk. The pipe is one-shot: git caches
//! the credential for the rest of its run.
//!
//! # `inherit` (legacy, amber)
//!
//! Keeps the ambient environment (the user's ssh-agent, credential
//! helpers, `~/.ssh/config` aliases), minus the git plumbing variables
//! that would retarget a call (`GIT_DIR`, `GIT_WORK_TREE`, …). Adds
//! `GIT_TERMINAL_PROMPT=0`, the same hardening flags except the helper
//! reset, the timeout, and `GIT_SSH_COMMAND="ssh -o BatchMode=yes -o
//! ConnectTimeout=10"` when neither `GIT_SSH_COMMAND`/`GIT_SSH` nor
//! `core.sshCommand` is set.
//!
//! Synchronous: call it from `spawn_blocking`.

use std::ffi::OsString;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use zeroize::Zeroize;

use super::classify::{classify, AuthContext, FailureClass};
use super::cred::HttpsCredential;
use super::proc::{self, RunSpec};
use super::redact::redact_bytes;
use super::url::{FetchRefspec, Protocol, RefName, RemoteName, RemoteUrl, UrlRejected};
use crate::git::Revspec;

/// Every store remote's `pushurl` (design §3.3, S-16). Not a real
/// transport: a push that somehow got past everything else fails here.
pub const NO_PUSH_URL: &str = "kbcode-no-push://refused";

/// `ls-remote` probe deadline (design §4.4).
pub const LS_REMOTE_TIMEOUT: Duration = Duration::from_secs(15);
/// Base (network) fetch deadline.
pub const BASE_FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// Work (local) fetch / materialize deadline.
pub const WORK_FETCH_TIMEOUT: Duration = Duration::from_secs(120);
/// Short local plumbing (config, rev-parse, update-ref).
pub const LOCAL_OP_TIMEOUT: Duration = Duration::from_secs(60);

const STDERR_CAP: usize = 64 * 1024;
const DEFAULT_STDOUT_CAP: usize = 16 * 1024 * 1024;
/// Cap on the redacted stderr carried in an error.
const DETAIL_CAP: usize = 2048;

const INHERIT_SSH_COMMAND: &str = "ssh -o BatchMode=yes -o ConnectTimeout=10";

/// git plumbing variables stripped even in `inherit` mode: any of them
/// would silently retarget a store call at another repository.
const RETARGETING_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_CEILING_DIRECTORIES",
    "GIT_PREFIX",
];

/// Proxy variables passed through the scrubbed environment.
const PROXY_VARS: &[&str] = &[
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "NO_PROXY",
    "no_proxy",
    "ALL_PROXY",
    "all_proxy",
];

/// git subcommands a [`GitArgs`] may never carry.
const WRITE_TO_REMOTE: &[&str] = &["push", "send-pack", "receive-pack"];

/// How a call authenticates — and therefore which transports it may use.
#[derive(Debug, Clone, Copy)]
pub enum FetchAuth<'a> {
    /// Local store plumbing and `work-<id>` fetches: `file` only, no
    /// credential of any kind.
    LocalOnly,
    /// HTTPS with no credential (public repos; the anonymous probe).
    Anonymous,
    /// HTTPS with a token served by kb's in-memory credential helper.
    Token(&'a HttpsCredential),
    /// The ambient environment (legacy, amber). See the module doc.
    Inherit,
}

impl FetchAuth<'_> {
    fn context(&self) -> AuthContext {
        match self {
            Self::LocalOnly | Self::Anonymous => AuthContext::None,
            Self::Token(_) => AuthContext::Token,
            Self::Inherit => AuthContext::Ambient,
        }
    }

    fn protocols(&self) -> Vec<&'static str> {
        match self {
            Self::LocalOnly => vec!["file"],
            Self::Anonymous => vec!["https"],
            Self::Token(c) => vec![c.scope().protocol().as_str()],
            Self::Inherit => vec!["https", "ssh"],
        }
    }

    /// May this auth talk to `url` at all? A token is only ever offered to
    /// its own protocol + host.
    fn admits(&self, url: &RemoteUrl) -> bool {
        match self {
            Self::LocalOnly => url.protocol() == Protocol::File,
            Self::Anonymous => url.protocol() == Protocol::Https,
            Self::Token(c) => {
                url.protocol() == c.scope().protocol() && url.authority() == c.scope().authority()
            }
            Self::Inherit => matches!(url.protocol(), Protocol::Https | Protocol::Ssh),
        }
    }
}

/// A git argv under construction. Only static flags and validated atoms
/// can be added — see the module doc.
#[derive(Debug, Clone)]
pub struct GitArgs {
    sub: &'static str,
    v: Vec<OsString>,
}

impl GitArgs {
    pub fn new(subcommand: &'static str) -> Self {
        Self {
            sub: subcommand,
            v: vec![OsString::from(subcommand)],
        }
    }

    /// A literal flag (a `&'static str` — never a runtime value).
    pub fn flag(mut self, f: &'static str) -> Self {
        self.v.push(f.into());
        self
    }

    /// `--end-of-options`: everything after is positional.
    pub fn end_of_options(self) -> Self {
        self.flag("--end-of-options")
    }

    pub fn remote(mut self, r: &RemoteName) -> Self {
        self.v.push(r.as_str().into());
        self
    }

    pub fn refspec(mut self, r: &FetchRefspec) -> Self {
        self.v.push(r.as_arg().into());
        self
    }

    pub fn refname(mut self, r: &RefName) -> Self {
        self.v.push(r.as_str().into());
        self
    }

    pub fn rev(mut self, r: &Revspec) -> Self {
        self.v.push(r.as_str().into());
        self
    }

    pub fn url(mut self, u: &RemoteUrl) -> Self {
        self.v.push(u.as_str().into());
        self
    }

    /// An absolute path (the store dir, a seed source).
    pub fn abs_path(mut self, p: &Path) -> Result<Self, UrlRejected> {
        if !p.is_absolute() {
            return Err(UrlRejected::NotAbsolute);
        }
        self.v.push(p.as_os_str().to_owned());
        Ok(self)
    }

    /// Module-private: a value this module composes itself from validated
    /// parts (a `remote.<name>.url` config key).
    fn composed(mut self, s: String) -> Self {
        self.v.push(s.into());
        self
    }

    pub fn as_slice(&self) -> &[OsString] {
        &self.v
    }
}

/// One `StoreGit` invocation.
#[derive(Debug)]
pub struct GitCall<'a> {
    op: &'static str,
    args: GitArgs,
    git_dir: Option<&'a Path>,
    auth: FetchAuth<'a>,
    timeout: Duration,
    stdin: Option<Vec<u8>>,
    stdout_cap: usize,
    allow_nonzero: bool,
}

impl<'a> GitCall<'a> {
    /// A local-only call with the default timeout. `op` names the call in
    /// errors and logs.
    pub fn new(op: &'static str, args: GitArgs) -> Self {
        Self {
            op,
            args,
            git_dir: None,
            auth: FetchAuth::LocalOnly,
            timeout: LOCAL_OP_TIMEOUT,
            stdin: None,
            stdout_cap: DEFAULT_STDOUT_CAP,
            allow_nonzero: false,
        }
    }
    pub fn git_dir(mut self, d: &'a Path) -> Self {
        self.git_dir = Some(d);
        self
    }
    pub fn auth(mut self, a: FetchAuth<'a>) -> Self {
        self.auth = a;
        self
    }
    pub fn timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }
    pub fn stdin(mut self, bytes: Vec<u8>) -> Self {
        self.stdin = Some(bytes);
        self
    }
    pub fn stdout_cap(mut self, cap: usize) -> Self {
        self.stdout_cap = cap;
        self
    }
    /// Return a non-zero exit as `Ok` (with `exit_code`) instead of an
    /// error — for probes like `rev-parse --verify`.
    pub fn allow_nonzero(mut self) -> Self {
        self.allow_nonzero = true;
        self
    }
}

/// A successful (or `allow_nonzero`) call's result.
pub struct GitOutput {
    pub exit_code: Option<i32>,
    /// Raw stdout (data: shas, ref lists). Use [`Self::stdout_redacted`]
    /// for anything that is logged.
    pub stdout: Vec<u8>,
    pub stdout_truncated: bool,
    /// Redacted, capped stderr.
    pub stderr: String,
    pub elapsed: Duration,
}

impl GitOutput {
    pub fn stdout_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.stdout)
    }
    pub fn stdout_redacted(&self) -> String {
        redact_bytes(&self.stdout, &[], DETAIL_CAP)
    }
    pub fn success(&self) -> bool {
        self.exit_code == Some(0)
    }
}

impl std::fmt::Debug for GitOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitOutput")
            .field("exit_code", &self.exit_code)
            .field("stdout", &self.stdout_redacted())
            .field("stdout_truncated", &self.stdout_truncated)
            .field("stderr", &self.stderr)
            .field("elapsed", &self.elapsed)
            .finish()
    }
}

/// A typed store-git failure. `detail` is redacted and capped.
#[derive(Debug, Clone, thiserror::Error)]
#[error("store git `{op}` failed ({class}): {detail}")]
pub struct StoreGitError {
    pub op: &'static str,
    pub class: FailureClass,
    pub exit_code: Option<i32>,
    pub detail: String,
}

impl StoreGitError {
    fn new(op: &'static str, class: FailureClass, detail: impl Into<String>) -> Self {
        Self {
            op,
            class,
            exit_code: None,
            detail: detail.into(),
        }
    }
    pub fn slug(&self) -> &'static str {
        self.class.slug()
    }
}

/// The hardened spawner. Cheap to clone; build once at boot.
#[derive(Debug, Clone)]
pub struct StoreGit {
    git_home: PathBuf,
    path_env: Option<OsString>,
    proxies: Vec<(OsString, OsString)>,
    /// `inherit` only: whether the ambient config has `core.sshCommand`
    /// (computed once, lazily).
    inherit_has_ssh_command: Arc<OnceLock<bool>>,
}

impl StoreGit {
    /// `git_home` becomes `HOME`/`XDG_CONFIG_HOME` for every scrubbed call;
    /// it is created (0700) if missing and must stay EMPTY — nothing kb
    /// writes belongs there.
    pub fn new(git_home: impl Into<PathBuf>) -> std::io::Result<Self> {
        Self::with_env_fn(git_home, |k| std::env::var_os(k))
    }

    /// As [`Self::new`], reading PATH/proxies through `get` (tests).
    pub fn with_env_fn(
        git_home: impl Into<PathBuf>,
        get: impl Fn(&str) -> Option<OsString>,
    ) -> std::io::Result<Self> {
        let git_home = git_home.into();
        if !git_home.is_absolute() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "StoreGit git_home must be absolute",
            ));
        }
        std::fs::create_dir_all(&git_home)?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&git_home, std::fs::Permissions::from_mode(0o700))?;
        }
        let proxies = PROXY_VARS
            .iter()
            .filter_map(|k| get(k).map(|v| (OsString::from(k), v)))
            .collect();
        Ok(Self {
            git_home,
            path_env: get("PATH"),
            proxies,
            inherit_has_ssh_command: Default::default(),
        })
    }

    pub fn git_home(&self) -> &Path {
        &self.git_home
    }

    /// Build the `Command` (no spawn). `helper_fd` is the token pipe's read
    /// end for [`FetchAuth::Token`].
    fn build(&self, call: &GitCall<'_>, helper_fd: Option<RawFd>) -> Command {
        let mut cmd = Command::new("git");
        let protocols = call.auth.protocols();
        let inherit = matches!(call.auth, FetchAuth::Inherit);
        if inherit {
            for v in RETARGETING_VARS {
                cmd.env_remove(v);
            }
            let ambient_ssh = std::env::var_os("GIT_SSH_COMMAND").is_some()
                || std::env::var_os("GIT_SSH").is_some();
            if !ambient_ssh && !self.inherit_has_ssh_command() {
                cmd.env("GIT_SSH_COMMAND", INHERIT_SSH_COMMAND);
            }
        } else {
            cmd.env_clear();
            if let Some(p) = &self.path_env {
                cmd.env("PATH", p);
            }
            cmd.env("HOME", &self.git_home)
                .env("XDG_CONFIG_HOME", &self.git_home)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_ASKPASS", "")
                .env("SSH_ASKPASS", "")
                .env("SSH_ASKPASS_REQUIRE", "never")
                .env("GIT_OPTIONAL_LOCKS", "0")
                .env("GIT_CEILING_DIRECTORIES", &self.git_home);
            for (k, v) in &self.proxies {
                cmd.env(k, v);
            }
        }
        cmd.env("GIT_TERMINAL_PROMPT", "0")
            .env("LC_ALL", "C")
            .env("LANG", "C")
            .env("GIT_ALLOW_PROTOCOL", protocols.join(":"));
        match call.git_dir {
            Some(d) => {
                cmd.env("GIT_DIR", d);
            }
            None => {
                cmd.env_remove("GIT_DIR");
            }
        }
        cmd.current_dir(&self.git_home);

        let mut cfg: Vec<String> = Vec::new();
        if !inherit {
            cfg.push("credential.helper=".into());
        }
        cfg.push("core.hooksPath=/dev/null".into());
        cfg.push("core.askPass=".into());
        cfg.push("core.fsmonitor=false".into());
        cfg.push("protocol.allow=never".into());
        for p in &protocols {
            cfg.push(format!("protocol.{p}.allow=always"));
        }
        cfg.push("gc.auto=0".into());
        cfg.push("maintenance.auto=false".into());
        if let (FetchAuth::Token(c), Some(fd)) = (call.auth, helper_fd) {
            cfg.push(format!("credential.helper={}", helper_snippet(fd, c)));
            cfg.push("credential.useHttpPath=false".into());
        }
        for c in cfg {
            cmd.arg("-c").arg(c);
        }
        cmd.args(call.args.as_slice());
        cmd
    }

    fn inherit_has_ssh_command(&self) -> bool {
        *self.inherit_has_ssh_command.get_or_init(|| {
            let mut cmd = Command::new("git");
            for v in RETARGETING_VARS {
                cmd.env_remove(v);
            }
            cmd.env("GIT_TERMINAL_PROMPT", "0")
                .env("LC_ALL", "C")
                .current_dir(&self.git_home)
                .args(["config", "--get", "core.sshCommand"]);
            let spec = RunSpec {
                timeout: Duration::from_secs(5),
                stdout_cap: 4096,
                stderr_cap: 4096,
                stdin: None,
            };
            proc::run(&mut cmd, &spec)
                .map(|c| c.status.is_some_and(|s| s.success()) && !c.stdout.is_empty())
                .unwrap_or(false)
        })
    }

    /// Run one call. See the module doc for the contract.
    pub fn run(&self, call: GitCall<'_>) -> Result<GitOutput, StoreGitError> {
        if WRITE_TO_REMOTE.contains(&call.args.sub) {
            return Err(StoreGitError::new(
                call.op,
                FailureClass::Failed,
                "the review store never writes to a remote",
            ));
        }
        let secret: Option<&str> = match call.auth {
            FetchAuth::Token(c) => Some(c.secret()),
            _ => None,
        };
        let pipe = match call.auth {
            FetchAuth::Token(c) => Some(token_pipe(&c.helper_payload()).map_err(|e| {
                StoreGitError::new(
                    call.op,
                    FailureClass::SpawnFailed,
                    format!("credential pipe: {e}"),
                )
            })?),
            _ => None,
        };
        let helper_fd = pipe.as_ref().map(|p| p.as_raw_fd());
        let mut cmd = self.build(&call, helper_fd);
        if let Some(fd) = helper_fd {
            inherit_fd_in_child(&mut cmd, fd);
        }
        let spec = RunSpec {
            timeout: call.timeout,
            stdout_cap: call.stdout_cap,
            stderr_cap: STDERR_CAP,
            stdin: call.stdin.clone(),
        };
        let res = proc::run(&mut cmd, &spec);
        // The parent's copy of the read end is closed as soon as git owns
        // its own; an unread token dies with the last reference.
        drop(pipe);
        let cap = match res {
            Ok(c) => c,
            Err(e) => {
                let class = if e.kind() == std::io::ErrorKind::NotFound {
                    FailureClass::SpawnFailed
                } else {
                    FailureClass::Failed
                };
                return Err(StoreGitError::new(
                    call.op,
                    class,
                    format!("spawn git: {e}"),
                ));
            }
        };
        let secrets: Vec<&str> = secret.into_iter().collect();
        let mut stderr = redact_bytes(&cap.stderr, &secrets, STDERR_CAP);
        if cap.stderr_truncated {
            stderr.push_str("\n…[stderr truncated]");
        }
        if cap.timed_out {
            tracing::warn!(op = call.op, timeout = ?call.timeout, "store git call timed out; process group killed");
            return Err(StoreGitError {
                op: call.op,
                class: FailureClass::Timeout,
                exit_code: None,
                detail: redact_bytes(stderr.as_bytes(), &[], DETAIL_CAP),
            });
        }
        let exit_code = cap.status.and_then(|s| s.code());
        if exit_code != Some(0) && !call.allow_nonzero {
            let class = classify(&stderr, call.auth.context());
            tracing::debug!(op = call.op, %class, exit = ?exit_code, stderr = %stderr, "store git call failed");
            return Err(StoreGitError {
                op: call.op,
                class,
                exit_code,
                detail: redact_bytes(stderr.as_bytes(), &[], DETAIL_CAP),
            });
        }
        Ok(GitOutput {
            exit_code,
            stdout: cap.stdout,
            stdout_truncated: cap.stdout_truncated,
            stderr,
            elapsed: cap.elapsed,
        })
    }

    // ---- the operations later units call -----------------------------

    /// `git init --bare` a new store at `dir` (absolute). No templates, so
    /// no sample hooks are copied in.
    pub fn init_bare(&self, dir: &Path) -> Result<(), StoreGitError> {
        let args = GitArgs::new("init")
            .flag("--bare")
            .flag("--quiet")
            .flag("--template=")
            .end_of_options()
            .abs_path(dir)
            .map_err(|e| StoreGitError::new("init", FailureClass::UrlRejected, e.to_string()))?;
        self.run(GitCall::new("init", args)).map(|_| ())
    }

    /// Point remote `name` at `url` in the store config, with
    /// `pushurl = NO_PUSH_URL`, `tagOpt = --no-tags`, and NO `fetch` line
    /// (every fetch names its refspecs explicitly). U3 calls this for
    /// `base` and every `work-<repo_id>`.
    pub fn configure_remote(
        &self,
        git_dir: &Path,
        name: &RemoteName,
        url: &RemoteUrl,
    ) -> Result<(), StoreGitError> {
        let key = |k: &str| format!("remote.{}.{k}", name.as_str());
        let set = |k: &str| {
            GitArgs::new("config")
                .flag("--replace-all")
                .end_of_options()
                .composed(key(k))
        };
        for args in [
            set("url").url(url),
            set("pushurl").composed(NO_PUSH_URL.to_string()),
            set("tagOpt").flag("--no-tags"),
        ] {
            self.run(GitCall::new("config", args).git_dir(git_dir))?;
        }
        // exit 5 = "no such key" — already clean.
        self.run(
            GitCall::new(
                "config",
                GitArgs::new("config")
                    .flag("--unset-all")
                    .end_of_options()
                    .composed(key("fetch")),
            )
            .git_dir(git_dir)
            .allow_nonzero(),
        )?;
        Ok(())
    }

    /// `git ls-remote <url> [patterns]` — the anonymous/credential probe.
    /// Returns `(oid, refname)` pairs.
    pub fn ls_remote(
        &self,
        url: &RemoteUrl,
        auth: FetchAuth<'_>,
        patterns: &[&'static str],
        timeout: Duration,
    ) -> Result<Vec<(String, String)>, StoreGitError> {
        if !auth.admits(url) {
            return Err(StoreGitError::new(
                "ls-remote",
                FailureClass::ProtocolRefused,
                "credential profile does not admit this url",
            ));
        }
        let mut args = GitArgs::new("ls-remote").end_of_options().url(url);
        for p in patterns {
            args = args.flag(p);
        }
        let out = self.run(GitCall::new("ls-remote", args).auth(auth).timeout(timeout))?;
        Ok(parse_ref_lines(&out.stdout_str()))
    }

    /// `git fetch` from a configured remote NAME with explicit refspecs:
    /// `--no-tags --no-write-fetch-head --no-auto-gc --no-auto-maintenance
    /// --quiet` (README §5.3).
    pub fn fetch(
        &self,
        git_dir: &Path,
        remote: &RemoteName,
        refspecs: &[FetchRefspec],
        auth: FetchAuth<'_>,
        timeout: Duration,
    ) -> Result<GitOutput, StoreGitError> {
        if refspecs.is_empty() {
            return Err(StoreGitError::new(
                "fetch",
                FailureClass::Failed,
                "a store fetch must name its refspecs",
            ));
        }
        let mut args = GitArgs::new("fetch")
            .flag("--no-tags")
            .flag("--no-write-fetch-head")
            .flag("--no-auto-gc")
            .flag("--no-auto-maintenance")
            .flag("--quiet")
            .end_of_options()
            .remote(remote);
        for r in refspecs {
            args = args.refspec(r);
        }
        self.run(
            GitCall::new("fetch", args)
                .git_dir(git_dir)
                .auth(auth)
                .timeout(timeout),
        )
    }

    /// Does the credential chain answer for `protocol://authority`? Runs
    /// `git credential fill` with the call's helper configuration and
    /// reports only WHETHER a password came back — the answer itself is
    /// zeroized unread (the `credential test`/doctor probe; no network).
    pub fn credential_answers(
        &self,
        auth: FetchAuth<'_>,
        protocol: &str,
        authority: &str,
    ) -> Result<bool, StoreGitError> {
        let clean = |s: &str| !s.is_empty() && !s.contains(['\n', '\r', '\0', '=']);
        if !clean(protocol) || !clean(authority) {
            return Err(StoreGitError::new(
                "credential",
                FailureClass::Failed,
                "bad credential probe attributes",
            ));
        }
        let input = format!("protocol={protocol}\nhost={authority}\n\n").into_bytes();
        let out = self.run(
            GitCall::new("credential", GitArgs::new("credential").flag("fill"))
                .auth(auth)
                .stdin(input)
                .stdout_cap(8192)
                .timeout(Duration::from_secs(10))
                .allow_nonzero(),
        )?;
        let mut stdout = out.stdout;
        let answered = out.exit_code == Some(0)
            && stdout
                .split(|b| *b == b'\n')
                .any(|l| l.starts_with(b"password=") && l.len() > "password=".len());
        stdout.zeroize();
        Ok(answered)
    }
}

/// Parse `<oid>\t<refname>` lines.
pub fn parse_ref_lines(s: &str) -> Vec<(String, String)> {
    s.lines()
        .filter_map(|l| {
            let (oid, name) = l.split_once('\t')?;
            Some((oid.trim().to_string(), name.trim().to_string()))
        })
        .collect()
}

/// The credential-helper shell snippet: answer `get` for exactly the
/// credential's protocol + host by relaying the inherited pipe `fd`;
/// consume and ignore anything else (`store`/`erase`, other hosts).
fn helper_snippet(fd: RawFd, cred: &HttpsCredential) -> String {
    let proto = cred.scope().protocol().as_str();
    let host = cred.scope().authority();
    // Both come from a validated `RemoteUrl`; re-assert the alphabet the
    // snippet relies on (no shell metacharacter can reach it).
    assert!(host
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b':')));
    assert!(proto.bytes().all(|c| c.is_ascii_lowercase()));
    format!(
        "!f() {{ test \"$1\" = get || {{ cat >/dev/null; exit 0; }}; p=; h=; \
         while IFS= read -r l; do [ -z \"$l\" ] && break; case \"$l\" in \
         protocol=*) p=\"${{l#protocol=}}\";; host=*) h=\"${{l#host=}}\";; esac; done; \
         [ \"$p\" = '{proto}' ] && [ \"$h\" = '{host}' ] || exit 0; cat <&{fd}; }}; f"
    )
}

/// Make `fd` (CLOEXEC in the daemon) survive exec in THIS child only.
fn inherit_fd_in_child(cmd: &mut Command, fd: RawFd) {
    use std::os::unix::process::CommandExt;
    let hook = move || -> std::io::Result<()> {
        // SAFETY: `fcntl` is async-signal-safe and touches only `fd`,
        // which the parent keeps open until after spawn returns.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, 0) } == -1 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    };
    // SAFETY: the hook only calls `fcntl` (see above) — no allocation, no
    // locks — as required between fork and exec.
    unsafe {
        cmd.pre_exec(hook);
    }
}

/// A CLOEXEC pipe pre-loaded with `payload`; returns the read end.
fn token_pipe(payload: &[u8]) -> std::io::Result<OwnedFd> {
    use std::io::Write;
    use std::os::fd::FromRawFd;
    // Well under PIPE_BUF-sized capacity, so the write can never block.
    if payload.len() >= 4096 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "credential payload too large",
        ));
    }
    let mut fds: [libc::c_int; 2] = [-1, -1];
    // SAFETY: `fds` is a valid 2-int buffer; O_CLOEXEC is set atomically
    // so no concurrently forked child can inherit either end.
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: pipe2 succeeded; each fd is fresh and owned exactly once.
    let read = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    // SAFETY: as above.
    let mut write = unsafe { std::fs::File::from_raw_fd(fds[1]) };
    write.write_all(payload)?;
    drop(write);
    Ok(read)
}

#[cfg(test)]
mod tests;
