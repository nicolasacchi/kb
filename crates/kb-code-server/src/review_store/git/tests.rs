//! RS-U2 — `StoreGit` tests. Hermetic: real git against local fixture
//! repos and loopback sockets, a fake `git` on a private PATH for the
//! exact-environment check. No network.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::*;
use crate::review_store::cred::{CredentialScope, HttpsCredential, SecretToken};
use crate::review_store::url::{RefName, RefSource};

const FAKE_TOKEN: &str = "ghp_FAKEkbrsU2tokenDoNotUse0123456789abcd";

fn store_git(home: &Path) -> StoreGit {
    StoreGit::new(home.join("git-home")).unwrap()
}

/// A fixture repo with one commit on `main`. Test-only direct spawn
/// (allowed: this file is in SEC-17's `GIT_SPAWNING_FILES`).
fn fixture_repo(dir: &Path) -> String {
    let run = |args: &[&str]| {
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
        String::from_utf8(out.stdout).unwrap()
    };
    std::fs::create_dir_all(dir).unwrap();
    run(&["init", "-q", "-b", "main"]);
    run(&["commit", "-q", "--allow-empty", "-m", "one"]);
    run(&["rev-parse", "HEAD"]).trim().to_string()
}

fn test_cred(port: u16) -> HttpsCredential {
    let url = RemoteUrl::test_http_loopback(port, "acme/widgets.git");
    HttpsCredential::new(
        CredentialScope::for_url(&url).unwrap(),
        "x-access-token",
        SecretToken::new(FAKE_TOKEN).unwrap(),
    )
    .unwrap()
}

/// Every readable `/proc/<pid>/{cmdline,environ}` on the box.
fn proc_scan() -> Vec<(u32, Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    for e in std::fs::read_dir("/proc").unwrap().flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let cmd = std::fs::read(e.path().join("cmdline")).unwrap_or_default();
        let env = std::fs::read(e.path().join("environ")).unwrap_or_default();
        out.push((pid, cmd, env));
    }
    out
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

fn read_head(s: &mut TcpStream) -> String {
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut head = String::new();
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        head.push_str(&line);
    }
    head
}

#[test]
fn the_token_reaches_the_server_but_never_argv_or_environ() {
    let tmp = tempfile::tempdir().unwrap();
    let sg = store_git(tmp.path());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx_auth, rx_auth) = mpsc::channel::<String>();
    let (tx_go, rx_go) = mpsc::channel::<()>();
    let server = std::thread::spawn(move || {
        for s in listener.incoming() {
            let mut s = s.unwrap();
            let head = read_head(&mut s);
            let auth = head.lines().find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("authorization")
                    .then(|| v.trim().to_string())
            });
            match auth {
                Some(a) => {
                    tx_auth.send(a).unwrap();
                    let _ = rx_go.recv_timeout(Duration::from_secs(30));
                    let _ = s.write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                    return;
                }
                None => {
                    let _ = s.write_all(
                        b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"kb\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
            }
        }
    });

    let cred = test_cred(port);
    let url = RemoteUrl::test_http_loopback(port, "acme/widgets.git");
    let sg2 = sg.clone();
    let cred2 = cred.clone();
    let client = std::thread::spawn(move || {
        sg2.ls_remote(
            &url,
            FetchAuth::Token(&cred2),
            &["HEAD"],
            Duration::from_secs(60),
        )
    });

    let header = rx_auth
        .recv_timeout(Duration::from_secs(30))
        .expect("git never retried with credentials");
    use base64::Engine;
    let want = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{FAKE_TOKEN}"))
    );
    assert_eq!(header, want, "the helper did not deliver the token");

    // git + git-remote-http are alive right now (the server is holding
    // the authenticated request). Nothing on the box may carry the token.
    let scan = proc_scan();
    let port_s = format!("127.0.0.1:{port}");
    assert!(
        scan.iter()
            .any(|(_, cmd, _)| contains(cmd, port_s.as_bytes())),
        "the git process tree was not alive during the scan"
    );
    for (pid, cmd, env) in &scan {
        assert!(
            !contains(cmd, FAKE_TOKEN.as_bytes()),
            "token in /proc/{pid}/cmdline"
        );
        assert!(
            !contains(env, FAKE_TOKEN.as_bytes()),
            "token in /proc/{pid}/environ"
        );
    }
    tx_go.send(()).unwrap();
    let err = client.join().unwrap().expect_err("the fixture answers 404");
    server.join().unwrap();
    assert!(!err.detail.contains(FAKE_TOKEN));
    assert!(!err.to_string().contains(FAKE_TOKEN));
    assert_eq!(err.class, FailureClass::RepoNotFound, "{err}");
    // nothing landed on disk under the store home either
    for e in walk(sg.git_home()) {
        let bytes = std::fs::read(&e).unwrap_or_default();
        assert!(
            !contains(&bytes, FAKE_TOKEN.as_bytes()),
            "token on disk: {e:?}"
        );
    }
}

fn walk(p: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let q = e.path();
            if q.is_dir() {
                v.extend(walk(&q));
            } else {
                v.push(q);
            }
        }
    }
    v
}

#[test]
fn the_helper_answers_only_for_its_own_protocol_and_host() {
    let tmp = tempfile::tempdir().unwrap();
    let sg = store_git(tmp.path());
    let cred = test_cred(4242);
    let auth = FetchAuth::Token(&cred);
    assert!(sg
        .credential_answers(auth, "http", "127.0.0.1:4242")
        .unwrap());
    assert!(!sg
        .credential_answers(auth, "http", "127.0.0.1:4243")
        .unwrap());
    assert!(!sg
        .credential_answers(auth, "http", "evil.example.com")
        .unwrap());
    assert!(!sg
        .credential_answers(auth, "https", "127.0.0.1:4242")
        .unwrap());
    // and with no credential at all nothing answers (no inherited helper)
    assert!(!sg
        .credential_answers(FetchAuth::Anonymous, "http", "127.0.0.1:4242")
        .unwrap());
    // the one-shot pipe: a second call gets a fresh pipe and answers again
    assert!(sg
        .credential_answers(auth, "http", "127.0.0.1:4242")
        .unwrap());
}

#[test]
fn the_scrubbed_environment_is_exactly_the_allowlist() {
    let tmp = tempfile::tempdir().unwrap();
    let fake_dir = tmp.path().join("fakebin");
    std::fs::create_dir_all(&fake_dir).unwrap();
    let out_dir = tmp.path().join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    let fake = fake_dir.join("git");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\n/usr/bin/env > {o}/env\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > {o}/argv\nexit 0\n",
            o = out_dir.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    crate::review_store::cred::tests::wait_executable(&fake);

    let fake_path = fake_dir.clone();
    let sg = StoreGit::with_env_fn(tmp.path().join("git-home"), move |k| match k {
        "PATH" => Some(fake_path.clone().into_os_string()),
        "HTTPS_PROXY" => Some("http://proxy.example.invalid:3128".into()),
        // Present in the "daemon env", must NOT reach git.
        "SSH_AUTH_SOCK" | "GH_TOKEN" | "GIT_DIR" => Some("leak".into()),
        _ => None,
    })
    .unwrap();
    let store = tmp.path().join("store.git");
    let cred = test_cred(4242);
    sg.run(
        GitCall::new("fetch", GitArgs::new("fetch").flag("--quiet"))
            .git_dir(&store)
            .auth(FetchAuth::Token(&cred)),
    )
    .unwrap();

    let env: BTreeMap<String, String> = std::fs::read_to_string(out_dir.join("env"))
        .unwrap()
        .lines()
        .filter_map(|l| {
            l.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        // added by /bin/sh itself
        .filter(|(k, _)| !matches!(k.as_str(), "PWD" | "SHLVL" | "_" | "OLDPWD"))
        .collect();
    let home = tmp.path().join("git-home").display().to_string();
    let want: BTreeMap<String, String> = [
        ("PATH", fake_dir.display().to_string()),
        ("HOME", home.clone()),
        ("XDG_CONFIG_HOME", home.clone()),
        ("GIT_CEILING_DIRECTORIES", home.clone()),
        ("GIT_CONFIG_NOSYSTEM", "1".into()),
        ("GIT_CONFIG_GLOBAL", "/dev/null".into()),
        ("GIT_ASKPASS", "".into()),
        ("SSH_ASKPASS", "".into()),
        ("SSH_ASKPASS_REQUIRE", "never".into()),
        ("GIT_OPTIONAL_LOCKS", "0".into()),
        ("GIT_TERMINAL_PROMPT", "0".into()),
        ("LC_ALL", "C".into()),
        ("LANG", "C".into()),
        ("GIT_ALLOW_PROTOCOL", "http".into()),
        ("GIT_DIR", store.display().to_string()),
        ("HTTPS_PROXY", "http://proxy.example.invalid:3128".into()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    assert_eq!(env, want);

    let argv = std::fs::read_to_string(out_dir.join("argv")).unwrap();
    assert!(!argv.contains(FAKE_TOKEN), "token in argv");
    let lines: Vec<&str> = argv.lines().collect();
    // hardening flags precede the subcommand
    let sub = lines.iter().position(|l| *l == "fetch").unwrap();
    let cfg: Vec<&str> = lines[..sub]
        .iter()
        .copied()
        .filter(|l| *l != "-c")
        .collect();
    assert_eq!(
        cfg[0], "credential.helper=",
        "inherited helpers are reset first"
    );
    for must in [
        "core.hooksPath=/dev/null",
        "core.askPass=",
        "core.fsmonitor=false",
        "protocol.allow=never",
        "protocol.http.allow=always",
        "gc.auto=0",
        "maintenance.auto=false",
        "credential.useHttpPath=false",
    ] {
        assert!(cfg.contains(&must), "missing -c {must}: {cfg:?}");
    }
    let helper = cfg
        .iter()
        .filter(|c| c.starts_with("credential.helper=!"))
        .collect::<Vec<_>>();
    assert_eq!(helper.len(), 1);
    assert!(helper[0].contains("'127.0.0.1:4242'"));
    assert!(helper[0].contains("cat <&"));
}

#[test]
fn local_fetch_writes_named_refs_and_remotes_cannot_push() {
    let tmp = tempfile::tempdir().unwrap();
    let sg = store_git(tmp.path());
    let src = tmp.path().join("src");
    let head = fixture_repo(&src);
    let store = tmp.path().join("store.git");
    sg.init_bare(&store).unwrap();
    assert!(
        !store.join("hooks").join("pre-push.sample").exists(),
        "no template hooks in the store"
    );
    let work = RemoteName::work(7);
    sg.configure_remote(&store, &work, &RemoteUrl::local_seed(&src).unwrap())
        .unwrap();
    // idempotent
    sg.configure_remote(&store, &work, &RemoteUrl::local_seed(&src).unwrap())
        .unwrap();

    let get = |key: &'static str| {
        let out = sg
            .run(
                GitCall::new(
                    "config",
                    GitArgs::new("config")
                        .flag("--get-all")
                        .end_of_options()
                        .composed(key.into()),
                )
                .git_dir(&store)
                .allow_nonzero(),
            )
            .unwrap();
        out.stdout_str().trim().to_string()
    };
    assert_eq!(get("remote.work-7.pushurl"), NO_PUSH_URL);
    assert_eq!(get("remote.work-7.tagopt"), "--no-tags");
    assert_eq!(get("remote.work-7.fetch"), "");

    let dst = RefName::parse("refs/remotes/work-7/main").unwrap();
    let spec = FetchRefspec::new(
        true,
        RefSource::Ref(RefName::branch("main").unwrap()),
        dst.clone(),
    );
    sg.fetch(
        &store,
        &work,
        &[spec],
        FetchAuth::LocalOnly,
        WORK_FETCH_TIMEOUT,
    )
    .unwrap();
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
    assert_eq!(got.stdout_str().trim(), head);
    assert!(!store.join("FETCH_HEAD").exists(), "--no-write-fetch-head");

    // A missing ref is typed.
    let spec = FetchRefspec::new(
        true,
        RefSource::Ref(RefName::branch("nope").unwrap()),
        RefName::parse("refs/remotes/work-7/nope").unwrap(),
    );
    let err = sg
        .fetch(
            &store,
            &work,
            &[spec],
            FetchAuth::LocalOnly,
            WORK_FETCH_TIMEOUT,
        )
        .unwrap_err();
    assert_eq!(err.class, FailureClass::Vanished, "{err}");

    // The push family is refused before anything spawns…
    let err = sg
        .run(GitCall::new("push", GitArgs::new("push").remote(&work)).git_dir(&store))
        .unwrap_err();
    assert_eq!(err.class, FailureClass::Failed);
    // …and even a raw push through the store config lands on the refused pushurl.
    let raw = Command::new("git")
        .env("GIT_DIR", &store)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "push",
            "work-7",
            "refs/remotes/work-7/main:refs/heads/pwned",
        ])
        .output()
        .unwrap();
    assert!(!raw.status.success());
    let src_refs = Command::new("git")
        .current_dir(&src)
        .args(["for-each-ref", "refs/heads/pwned"])
        .output()
        .unwrap();
    assert!(src_refs.stdout.is_empty(), "a push reached the source repo");
}

#[test]
fn ext_transport_and_hooks_are_neutralised_even_from_store_config() {
    let tmp = tempfile::tempdir().unwrap();
    let sg = store_git(tmp.path());
    let src = tmp.path().join("src");
    fixture_repo(&src);
    let store = tmp.path().join("store.git");
    sg.init_bare(&store).unwrap();
    let pwned = tmp.path().join("pwned");
    let hook_dir = tmp.path().join("hooks");
    std::fs::create_dir_all(&hook_dir).unwrap();
    let hook = hook_dir.join("reference-transaction");
    std::fs::write(&hook, format!("#!/bin/sh\ntouch {}\n", pwned.display())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    // A hostile store config (written behind kb's back).
    let cfg = |k: &str, v: &str| {
        assert!(Command::new("git")
            .env("GIT_DIR", &store)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(["config", k, v])
            .status()
            .unwrap()
            .success());
    };
    cfg("core.hooksPath", &hook_dir.display().to_string());
    cfg(
        "remote.evil.url",
        &format!("ext::sh -c touch% {}", pwned.display()),
    );
    cfg("remote.work-1.url", &src.display().to_string());

    let evil = RemoteName::parse("evil").unwrap();
    let spec = FetchRefspec::new(
        true,
        RefSource::Ref(RefName::branch("main").unwrap()),
        RefName::parse("refs/remotes/evil/main").unwrap(),
    );
    let err = sg
        .fetch(
            &store,
            &evil,
            &[spec],
            FetchAuth::LocalOnly,
            WORK_FETCH_TIMEOUT,
        )
        .unwrap_err();
    assert_eq!(err.class, FailureClass::ProtocolRefused, "{err}");

    // A legitimate fetch updates refs — the hook must not fire.
    let spec = FetchRefspec::new(
        true,
        RefSource::Ref(RefName::branch("main").unwrap()),
        RefName::parse("refs/remotes/work-1/main").unwrap(),
    );
    sg.fetch(
        &store,
        &RemoteName::work(1),
        &[spec],
        FetchAuth::LocalOnly,
        WORK_FETCH_TIMEOUT,
    )
    .unwrap();
    assert!(!pwned.exists(), "a hook or ext:: transport ran");
}

#[test]
fn profiles_only_talk_to_their_own_transports() {
    let tmp = tempfile::tempdir().unwrap();
    let sg = store_git(tmp.path());
    let https = RemoteUrl::parse_remote("https://github.com/acme/widgets.git").unwrap();
    let local = RemoteUrl::local_seed(Path::new("/srv/acme/widgets")).unwrap();
    let err = sg
        .ls_remote(&https, FetchAuth::LocalOnly, &[], LS_REMOTE_TIMEOUT)
        .unwrap_err();
    assert_eq!(err.class, FailureClass::ProtocolRefused);
    let err = sg
        .ls_remote(&local, FetchAuth::Anonymous, &[], LS_REMOTE_TIMEOUT)
        .unwrap_err();
    assert_eq!(err.class, FailureClass::ProtocolRefused);
    // A token bound to one host is never offered to another.
    let cred = test_cred(4242);
    let other = RemoteUrl::test_http_loopback(4243, "acme/widgets.git");
    let err = sg
        .ls_remote(&other, FetchAuth::Token(&cred), &[], LS_REMOTE_TIMEOUT)
        .unwrap_err();
    assert_eq!(err.class, FailureClass::ProtocolRefused);
}

#[test]
fn an_unreachable_remote_is_offline_and_redacted() {
    let tmp = tempfile::tempdir().unwrap();
    let sg = store_git(tmp.path());
    // Grab a free port, then close it: connection refused.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let url = RemoteUrl::test_http_loopback(port, "acme/widgets.git");
    let cred = test_cred(port);
    let err = sg
        .ls_remote(&url, FetchAuth::Token(&cred), &["HEAD"], LS_REMOTE_TIMEOUT)
        .unwrap_err();
    assert_eq!(err.class, FailureClass::Offline, "{err}");
    assert!(!err.to_string().contains(FAKE_TOKEN));
}

#[test]
fn a_hung_remote_times_out_and_the_whole_group_dies() {
    let tmp = tempfile::tempdir().unwrap();
    let sg = store_git(tmp.path());
    // Accepts and never answers — the TLS handshake hangs forever.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let held = std::thread::spawn(move || {
        let mut keep = Vec::new();
        listener.set_nonblocking(true).unwrap();
        let until = Instant::now() + Duration::from_secs(20);
        while Instant::now() < until {
            if let Ok((s, _)) = listener.accept() {
                keep.push(s);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    let url =
        RemoteUrl::parse_remote(&format!("https://127.0.0.1:{port}/acme/widgets.git")).unwrap();
    let t0 = Instant::now();
    let err = sg
        .ls_remote(
            &url,
            FetchAuth::Anonymous,
            &["HEAD"],
            Duration::from_millis(1500),
        )
        .unwrap_err();
    assert_eq!(err.class, FailureClass::Timeout, "{err}");
    assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
    let needle = format!("127.0.0.1:{port}");
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let alive: Vec<u32> = proc_scan()
            .into_iter()
            .filter(|(_, cmd, _)| contains(cmd, needle.as_bytes()))
            .map(|(p, _, _)| p)
            .collect();
        if alive.is_empty() {
            break;
        }
        assert!(
            Instant::now() < until,
            "git helpers survived the timeout: {alive:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(held);
}
