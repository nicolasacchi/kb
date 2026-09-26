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
//!   `GIT_CONFIG_GLOBAL=<git_home>/gitconfig` (kb-written; only
//!   `safe.directory` for registered local sources), `GIT_TERMINAL_PROMPT=0`, empty
//!   `GIT_ASKPASS`/`SSH_ASKPASS` + `SSH_ASKPASS_REQUIRE=never`,
//!   `GIT_OPTIONAL_LOCKS=0`, `GIT_ALLOW_PROTOCOL` = exactly the profile's
//!   transports (never `ext`/`fd`), proxy + CA-bundle variables passed
//!   through, PATH minus empty/relative entries, and
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
//! (the pre-exec hook, which `dup2`s it onto fd 3 so any POSIX `sh`,
//! dash included, can redirect from it), so git — and only git's own
//! process tree — inherits it. The configured helper is a fixed shell
//! snippet that answers a `get` for exactly the scope's protocol + host
//! (anything else: no answer) by `cat <&3`. The snippet contains the fd number and
//! host only; the token exists in the daemon's memory, in the pipe
//! buffer, and in git's memory — never in any `/proc/<pid>/cmdline` or
//! `/proc/<pid>/environ`, never on disk. The pipe is one-shot: git caches
//! the credential for the rest of its run.
//!
//! # `inherit` (legacy, amber)
//!
//! Keeps the ambient environment (the user's ssh-agent, credential
//! helpers, `~/.ssh/config` aliases), minus the git plumbing variables
//! that would retarget a call (`GIT_DIR`, `GIT_WORK_TREE`, …), the
//! prompting/debug ones (`GIT_ASKPASS`, `SSH_ASKPASS`, `DISPLAY`,
//! `GIT_EXEC_PATH`, `GIT_TRACE*`, `GIT_CURL_VERBOSE`, `GIT_SSL_NO_VERIFY`).
//! Adds `GIT_TERMINAL_PROMPT=0`, `SSH_ASKPASS_REQUIRE=never`, the same
//! hardening flags except the helper reset, the timeout, and
//! `GIT_SSH_COMMAND="ssh -o BatchMode=yes -o ConnectTimeout=10"` only when
//! neither `GIT_SSH_COMMAND`/`GIT_SSH` is set AND a probe DEFINITIVELY
//! found no `core.sshCommand` (env beats config, so an unknown answer
//! never overrides; a failed probe is not cached).
//!
//! Synchronous: call it from `spawn_blocking`.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
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

/// The descriptor the credential pipe is dup'ed onto in the child. A single
/// digit on purpose: POSIX only guarantees `<&N` for N in 0–9, and dash
/// (Debian's `/bin/sh`) rejects multi-digit redirections.
const HELPER_FD: RawFd = 3;

/// kb's own global git config inside `git_home` (replaces
/// `GIT_CONFIG_GLOBAL=/dev/null`). Holds ONLY `safe.directory` entries for
/// registered local sources — see [`StoreGit::allow_local_source`].
const GLOBAL_CONFIG_NAME: &str = "gitconfig";

/// Extra ambient variables `inherit` mode strips (beyond
/// [`RETARGETING_VARS`] and every `GIT_TRACE*`): GUI/askpass prompting,
/// a redirected exec path, and debug switches that dump headers.
const INHERIT_STRIPPED_VARS: &[&str] = &[
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "GIT_EXEC_PATH",
    "GIT_CURL_VERBOSE",
    "GIT_SSL_NO_VERIFY",
];

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

/// Proxy + CA-bundle variables passed through the scrubbed environment.
const PASSTHROUGH_VARS: &[&str] = &[
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
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

    /// A ref name as a rev-list EXCLUSION (`^<ref>`) — the grammar
    /// `git bundle create <file> <git-rev-list-args>` and `git rev-list`
    /// itself share (RS-U9, README §5.4/§8: a backup bundle carries
    /// `refs/kbc/*` minus what `refs/remotes/base/*` reaches). The `^`
    /// prefix is a static, code-controlled literal composed onto an
    /// already-validated [`RefName`] — never attacker input, same
    /// "compose from validated parts" posture as [`Self::composed`].
    pub fn exclude_ref(mut self, r: &RefName) -> Self {
        self.v.push(format!("^{}", r.as_str()).into());
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
    /// The daemon's PATH with empty and relative entries dropped.
    path_env: Option<OsString>,
    passthrough: Vec<(OsString, OsString)>,
    /// `<git_home>/gitconfig` — the scrubbed calls' `GIT_CONFIG_GLOBAL`.
    global_config: PathBuf,
    /// `safe.directory` entries written into `global_config`.
    safe_dirs: Arc<Mutex<BTreeSet<String>>>,
    /// `inherit` only: `Some(has core.sshCommand)` once a probe gave a
    /// DEFINITIVE answer; `None` = never probed or the probe failed (a
    /// failure is never cached — the next call probes again).
    inherit_ssh_probe: Arc<Mutex<Option<bool>>>,
    /// Test hook: the "ambient environment" `inherit` mode inspects.
    ambient_override: Option<Arc<Vec<(OsString, OsString)>>>,
}

impl StoreGit {
    /// `git_home` becomes `HOME`/`XDG_CONFIG_HOME` for every scrubbed call;
    /// it is created (0700) if missing. The only file kb keeps there is
    /// its own `gitconfig` (`safe.directory` entries); nothing else
    /// belongs there.
    pub fn new(git_home: impl Into<PathBuf>) -> std::io::Result<Self> {
        Self::with_env_fn(git_home, |k| std::env::var_os(k))
    }

    /// As [`Self::new`], reading PATH/proxies/CA vars through `get` (tests).
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
        let passthrough = PASSTHROUGH_VARS
            .iter()
            .filter_map(|k| get(k).map(|v| (OsString::from(k), v)))
            .collect();
        let sg = Self {
            global_config: git_home.join(GLOBAL_CONFIG_NAME),
            git_home,
            path_env: get("PATH").and_then(|p| sanitize_path(&p)),
            passthrough,
            safe_dirs: Default::default(),
            inherit_ssh_probe: Default::default(),
            ambient_override: None,
        };
        sg.write_global_config(&BTreeSet::new())?;
        Ok(sg)
    }

    pub fn git_home(&self) -> &Path {
        &self.git_home
    }

    /// The `git` a scrubbed call will execute (first executable `git` on
    /// the sanitized PATH) — for the boot log line. `None` = not found.
    pub fn resolved_git(&self) -> Option<PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        let path = self.path_env.as_ref()?;
        std::env::split_paths(path)
            .map(|d| d.join("git"))
            .find(|p| {
                std::fs::metadata(p)
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            })
    }

    /// Trust a LOCAL source repository (a member clone the store seeds or
    /// fetches from) that may be owned by another uid: adds it to
    /// `safe.directory` in kb's own global config. Scoped to exactly the
    /// registered paths — never `*`. `path` must be absolute; control
    /// characters are refused (they would inject config lines).
    pub fn allow_local_source(&self, path: &Path) -> std::io::Result<()> {
        let s = path
            .to_str()
            .filter(|s| path.is_absolute() && !s.chars().any(|c| c.is_control()))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "safe.directory source must be an absolute path without control characters",
                )
            })?;
        let mut dirs = self.safe_dirs.lock().unwrap_or_else(|p| p.into_inner());
        if dirs.insert(s.to_string()) {
            self.write_global_config(&dirs)?;
        }
        Ok(())
    }

    fn write_global_config(&self, dirs: &BTreeSet<String>) -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut body = String::from(
            "# Written by kb-code (review store). Regenerated; do not edit.\n[safe]\n",
        );
        for d in dirs {
            let esc = d.replace('\\', "\\\\").replace('"', "\\\"");
            body.push_str(&format!("\tdirectory = \"{esc}\"\n"));
        }
        let tmp = self.git_home.join(format!("{GLOBAL_CONFIG_NAME}.tmp"));
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, &self.global_config)
    }

    /// Test hook: the ambient environment `inherit` mode inspects.
    #[cfg(test)]
    pub(crate) fn with_ambient_for_test(mut self, vars: Vec<(OsString, OsString)>) -> Self {
        self.ambient_override = Some(Arc::new(vars));
        self
    }

    fn ambient_vars(&self) -> Vec<(OsString, OsString)> {
        match &self.ambient_override {
            Some(v) => v.as_ref().clone(),
            None => std::env::vars_os().collect(),
        }
    }

    /// Build the `Command` (no spawn). `helper_fd` is the token pipe's read
    /// end for [`FetchAuth::Token`].
    fn build(&self, call: &GitCall<'_>, helper_fd: Option<RawFd>) -> Command {
        let mut cmd = Command::new("git");
        let protocols = call.auth.protocols();
        let inherit = matches!(call.auth, FetchAuth::Inherit);
        if inherit {
            let ambient = self.ambient_vars();
            for v in RETARGETING_VARS.iter().chain(INHERIT_STRIPPED_VARS) {
                cmd.env_remove(v);
            }
            for (k, _) in &ambient {
                if k.to_string_lossy().starts_with("GIT_TRACE") {
                    cmd.env_remove(k);
                }
            }
            cmd.env("SSH_ASKPASS_REQUIRE", "never");
            let has = |k: &str| ambient.iter().any(|(a, _)| a == k);
            // Only when we KNOW nothing else picks the ssh command: env
            // beats `core.sshCommand`, so a guess would silently override
            // the user's key selection.
            if !has("GIT_SSH_COMMAND") && !has("GIT_SSH") && self.inherit_ssh_command_absent() {
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
                .env("GIT_CONFIG_GLOBAL", &self.global_config)
                .env("GIT_ASKPASS", "")
                .env("SSH_ASKPASS", "")
                .env("SSH_ASKPASS_REQUIRE", "never")
                .env("GIT_OPTIONAL_LOCKS", "0")
                .env("GIT_CEILING_DIRECTORIES", &self.git_home);
            for (k, v) in &self.passthrough {
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
        if let (FetchAuth::Token(c), Some(_)) = (call.auth, helper_fd) {
            cfg.push(format!("credential.helper=!{}", helper_snippet(c)));
            cfg.push("credential.useHttpPath=false".into());
        }
        for c in cfg {
            cmd.arg("-c").arg(c);
        }
        cmd.args(call.args.as_slice());
        cmd
    }

    /// `true` only when a probe DEFINITIVELY found no `core.sshCommand`
    /// in the ambient config. A failed/timed-out probe answers `false`
    /// ("unknown: don't override") and is not cached.
    fn inherit_ssh_command_absent(&self) -> bool {
        let mut cached = self
            .inherit_ssh_probe
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(has) = *cached {
            return !has;
        }
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
        let verdict = proc::run(&mut cmd, &spec).ok().and_then(|c| {
            ssh_probe_verdict(c.timed_out, c.status.and_then(|s| s.code()), &c.stdout)
        });
        *cached = verdict;
        verdict == Some(false)
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

/// Interpret `git config --get core.sshCommand`: exit 0 with a value =
/// set; exit 1 = definitively unset; anything else (timeout, error) =
/// unknown (`None`, never cached).
fn ssh_probe_verdict(timed_out: bool, code: Option<i32>, stdout: &[u8]) -> Option<bool> {
    match (timed_out, code) {
        (false, Some(0)) => Some(!stdout.iter().all(|b| b.is_ascii_whitespace())),
        (false, Some(1)) => Some(false),
        _ => None,
    }
}

/// Drop empty and relative PATH entries (a relative entry resolves
/// against the child's cwd — attacker-influenced in general).
fn sanitize_path(p: &std::ffi::OsStr) -> Option<OsString> {
    let kept: Vec<PathBuf> = std::env::split_paths(p)
        .filter(|d| d.is_absolute())
        .collect();
    if kept.is_empty() {
        return None;
    }
    std::env::join_paths(kept).ok()
}

/// The credential-helper shell snippet (without the leading `!`): answer
/// `get` for exactly the credential's protocol + host by relaying the
/// inherited pipe on [`HELPER_FD`]; consume and ignore anything else
/// (`store`/`erase`, other hosts).
fn helper_snippet(cred: &HttpsCredential) -> String {
    let proto = cred.scope().protocol().as_str();
    let host = cred.scope().authority();
    // Both come from a validated `RemoteUrl`; re-assert the alphabet the
    // snippet relies on (no shell metacharacter can reach it).
    assert!(host
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b':')));
    assert!(proto.bytes().all(|c| c.is_ascii_lowercase()));
    format!(
        "f() {{ test \"$1\" = get || {{ cat >/dev/null; exit 0; }}; p=; h=; \
         while IFS= read -r l; do [ -z \"$l\" ] && break; case \"$l\" in \
         protocol=*) p=\"${{l#protocol=}}\";; host=*) h=\"${{l#host=}}\";; esac; done; \
         [ \"$p\" = '{proto}' ] && [ \"$h\" = '{host}' ] || exit 0; cat <&{fd}; }}; f",
        fd = HELPER_FD
    )
}

/// Make `fd` (CLOEXEC in the daemon) appear as [`HELPER_FD`] in THIS
/// child only, surviving exec. Runs after stdio is set up (0–2), so fd 3
/// is the first free slot; whatever the child had there (necessarily a
/// CLOEXEC descriptor about to vanish on exec, or a stray inherited one we
/// would not want git to see anyway) is replaced.
fn inherit_fd_in_child(cmd: &mut Command, fd: RawFd) {
    use std::os::unix::process::CommandExt;
    let hook = move || -> std::io::Result<()> {
        // SAFETY: `dup2`/`fcntl` are async-signal-safe and touch only
        // `fd` (kept open by the parent until spawn returns) and fd 3.
        let rc = unsafe {
            if fd == HELPER_FD {
                libc::fcntl(fd, libc::F_SETFD, 0)
            } else {
                // dup2 clears FD_CLOEXEC on the new descriptor.
                libc::dup2(fd, HELPER_FD)
            }
        };
        if rc == -1 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    };
    // SAFETY: the hook only calls `dup2`/`fcntl` (see above) — no
    // allocation, no locks — as required between fork and exec.
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
