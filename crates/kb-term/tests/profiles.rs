//! Launch-profile tests (`profiles_*`) plus the term-int login-defence
//! test (`term_int_*`). No harness is ever run; all fixtures synthetic.

use kb_term::profiles::{self, PlanError, PlanRequest};
use std::path::PathBuf;

fn base_env() -> Vec<(String, String)> {
    vec![
        ("HOME".into(), "/home/x".into()),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("LANG".into(), "C.UTF-8".into()),
        ("USER".into(), "u".into()),
    ]
}

/// A plan request in a (created-but-empty) run dir.
fn request<'a>(
    harness: &'a str,
    run_dir: &'a std::path::Path,
    env: &'a [(String, String)],
) -> PlanRequest<'a> {
    PlanRequest {
        harness,
        term_id: "abc123",
        resume_sid: None,
        cwd: "/w/x",
        job_ulid: None,
        run_dir,
        base_env: env,
        extra_passthrough: &[],
    }
}

#[test]
fn profiles_env_is_allowlist_only() {
    let dir = tempfile::tempdir().unwrap();
    let env: Vec<(String, String)> = vec![
        ("KITTY_WINDOW_ID".into(), "1".into()),
        ("GHOSTTY_RESOURCES_DIR".into(), "/r".into()),
        ("WEZTERM_PANE".into(), "2".into()),
        ("TERM_PROGRAM".into(), "x".into()),
        ("LC_TERMINAL".into(), "t".into()),
        ("ZELLIJ_SESSION_NAME".into(), "z".into()),
        ("TMUX".into(), "/t,1,2".into()),
        ("TERM".into(), "tmux-256color".into()),
        ("FOO_SECRET".into(), "s".into()),
        ("HOME".into(), "/home/u".into()),
    ];
    let plan = profiles::plan(&request("claude", dir.path(), &env)).unwrap();
    assert_eq!(
        plan.env,
        vec![
            (
                "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN".to_string(),
                "1".to_string()
            ),
            ("HOME".to_string(), "/home/u".to_string()),
            ("KB_TERM_ID".to_string(), "abc123".to_string()),
        ]
    );
    // TMUX/TERM absent from the plan env (tmux supplies them); the
    // LC_TERMINAL prefix trap did not forward anything.
    assert!(plan.env.iter().all(|(k, _)| k != "TMUX" && k != "TERM"));
}

#[test]
fn profiles_extra_passthrough_rejects_forbidden_names() {
    let dir = tempfile::tempdir().unwrap();
    let env = base_env();
    for bad in [
        "GHOSTTY_X",
        "lowercase",
        "TERM_PROGRAM",
        "TMUX",
        "KB_SESSION_ID",
    ] {
        let mut req = request("claude", dir.path(), &env);
        let names = vec![bad.to_string()];
        req.extra_passthrough = &names;
        assert_eq!(
            profiles::plan(&req),
            Err(PlanError::ForbiddenEnvName(bad.to_string())),
            "passthrough {bad}"
        );
    }
    // An allowed name is copied from base_env.
    let mut env = base_env();
    env.push(("KB_CAPTURE_SPOOL".into(), "/spool".into()));
    let names = vec!["KB_CAPTURE_SPOOL".to_string()];
    let mut req = request("claude", dir.path(), &env);
    req.extra_passthrough = &names;
    let plan = profiles::plan(&req).unwrap();
    assert!(plan
        .env
        .contains(&("KB_CAPTURE_SPOOL".to_string(), "/spool".to_string())));
    // A missing name is skipped silently.
    let names = vec!["KB_BIN_DIR".to_string()];
    let mut req = request("claude", dir.path(), &env);
    req.extra_passthrough = &names;
    let plan = profiles::plan(&req).unwrap();
    assert!(plan.env.iter().all(|(k, _)| k != "KB_BIN_DIR"));
}

#[test]
fn profiles_unset_lists_never_name_tmux_owned_vars() {
    let dir = tempfile::tempdir().unwrap();
    let env: Vec<(String, String)> = vec![
        ("GHOSTTY_RESOURCES_DIR".into(), "/r".into()),
        ("ZELLIJ_SESSION_NAME".into(), "z".into()),
        ("TERM_PROGRAM".into(), "x".into()),
        ("TMUX".into(), "/t,1,2".into()),
        ("TMUX_PANE".into(), "%1".into()),
        ("HOME".into(), "/home/u".into()),
    ];
    let plan = profiles::plan(&request("omp", dir.path(), &env)).unwrap();
    // Every forbidden exact name is present.
    for name in profiles::env::FORBIDDEN_EXACT {
        assert!(
            plan.unset_after_login.contains(&name.to_string()),
            "unset list missing {name}"
        );
    }
    // Concrete forbidden-prefixed names in base_env are present.
    assert!(plan
        .unset_after_login
        .contains(&"GHOSTTY_RESOURCES_DIR".to_string()));
    assert!(plan
        .unset_after_login
        .contains(&"ZELLIJ_SESSION_NAME".to_string()));
    // Never a tmux-owned name.
    for name in ["TMUX", "TMUX_PANE", "TERM"] {
        assert!(
            !plan.unset_after_login.contains(&name.to_string()),
            "unset list must not name {name}"
        );
    }
}

#[test]
fn profiles_unset_prelude_covers_login_reintroduction() {
    let names = vec!["KITTY_WINDOW_ID".to_string(), "TERM_PROGRAM".to_string()];
    let prelude = profiles::env::shell_unset_prelude(&names).unwrap();
    assert_eq!(prelude, "unset KITTY_WINDOW_ID TERM_PROGRAM; ");
    // A non-identifier is rejected, never emitted as shell.
    assert!(profiles::env::shell_unset_prelude(&["9BAD".to_string()]).is_err());
    assert!(profiles::env::shell_unset_prelude(&["A;rm".to_string()]).is_err());
    // The prefix snippet names all seven forbidden prefixes.
    let snippet = profiles::env::shell_unset_prefixes_snippet();
    for prefix in profiles::env::FORBIDDEN_PREFIX {
        assert!(snippet.contains(prefix), "snippet missing {prefix}");
    }
    assert!(snippet.contains("unset \"$v\""));
}

#[test]
fn profiles_golden_plan_per_harness_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let env = base_env();
    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("omp", vec!["omp"]),
        ("claude", vec!["claude"]),
        ("codex", vec!["codex", "--no-alt-screen"]),
        ("kimi", vec!["kimi"]),
        ("grok", vec!["grok", "--minimal", "--no-alt-screen"]),
        ("opencode", vec!["opencode", "--mini"]),
    ];
    for (harness, want) in cases {
        let plan = profiles::plan(&request(harness, dir.path(), &env)).unwrap();
        let want: Vec<String> = want.iter().map(|s| s.to_string()).collect();
        assert_eq!(plan.argv, want, "fresh argv for {harness}");
    }
    // Exact sorted env for omp fresh.
    let plan = profiles::plan(&request("omp", dir.path(), &env)).unwrap();
    let overlay = dir.path().join("omp-overlay.yml");
    assert_eq!(
        plan.env,
        vec![
            ("HOME".to_string(), "/home/x".to_string()),
            ("KB_TERM_ID".to_string(), "abc123".to_string()),
            ("LANG".to_string(), "C.UTF-8".to_string()),
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            (
                "PI_CONFIG_FILES".to_string(),
                overlay.to_string_lossy().into_owned()
            ),
            ("PI_FORCE_IMAGE_PROTOCOL".to_string(), "kitty".to_string()),
            ("USER".to_string(), "u".to_string()),
        ]
    );
    // Exact argv + profile env spot-locks per harness.
    let claude = profiles::plan(&request("claude", dir.path(), &env)).unwrap();
    assert!(claude.env.contains(&(
        "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN".to_string(),
        "1".to_string()
    )));
    let kimi = profiles::plan(&request("kimi", dir.path(), &env)).unwrap();
    assert!(kimi
        .env
        .contains(&("PI_IMAGE_PROTOCOL".to_string(), "iterm2".to_string())));
    let codex = profiles::plan(&request("codex", dir.path(), &env)).unwrap();
    assert!(!codex.env.iter().any(|(k, _)| k.starts_with("PI_")));
}

#[test]
fn profiles_golden_plan_per_harness_resume() {
    let dir = tempfile::tempdir().unwrap();
    let env = base_env();
    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("omp", vec!["omp", "-r", "sid-1"]),
        ("claude", vec!["claude", "-r", "sid-1"]),
        ("codex", vec!["codex", "--no-alt-screen", "resume", "sid-1"]),
        ("kimi", vec!["kimi", "-S", "sid-1"]),
        (
            "grok",
            vec![
                "grok",
                "--minimal",
                "--no-alt-screen",
                "-r",
                "sid-1",
                "--cwd",
                "/w/x",
            ],
        ),
        ("opencode", vec!["opencode", "--mini", "--session", "sid-1"]),
    ];
    for (harness, want) in cases {
        let mut req = request(harness, dir.path(), &env);
        req.resume_sid = Some("sid-1");
        let plan = profiles::plan(&req).unwrap();
        let want: Vec<String> = want.iter().map(|s| s.to_string()).collect();
        assert_eq!(plan.argv, want, "resume argv for {harness}");
    }
}

#[test]
fn profiles_never_emit_kb_off_or_bare_continue() {
    let dir = tempfile::tempdir().unwrap();
    let env = base_env();
    for harness in profiles::HARNESSES {
        for resume in [None, Some("sid-1")] {
            let mut req = request(harness, dir.path(), &env);
            req.resume_sid = resume;
            let plan = profiles::plan(&req).unwrap();
            for arg in &plan.argv {
                assert!(
                    !profiles::table::FORBIDDEN_ARGV.contains(&arg.as_str()),
                    "{harness} resume={resume:?} emitted forbidden `{arg}`"
                );
            }
        }
    }
    // Fresh argv carries no resume token.
    let plan = profiles::plan(&request("omp", dir.path(), &env)).unwrap();
    assert!(!plan.argv.iter().any(|a| a == "-r" || a == "resume"));
}

#[test]
fn profiles_omp_overlay_written_0600_absolute() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let env = base_env();
    let plan = profiles::plan(&request("omp", dir.path(), &env)).unwrap();
    let path = plan.overlay_path.clone().unwrap();
    assert!(path.is_absolute());
    assert_eq!(path, dir.path().join("omp-overlay.yml"));
    let meta = std::fs::metadata(&path).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    let content = std::fs::read_to_string(&path).unwrap();
    assert_eq!(content, profiles::OMP_OVERLAY);
    assert!(!content.contains("keepRecentTokens"));
    // PI_CONFIG_FILES points at exactly that absolute path.
    assert!(plan.env.contains(&(
        "PI_CONFIG_FILES".to_string(),
        path.to_string_lossy().into_owned()
    )));
    // Non-omp plans never produce an overlay path.
    let plan = profiles::plan(&request("claude", dir.path(), &env)).unwrap();
    assert_eq!(plan.overlay_path, None);
}

#[test]
fn profiles_rejects_bad_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let env = base_env();
    let mut req = request("zzz", dir.path(), &env);
    assert!(matches!(
        profiles::plan(&req),
        Err(PlanError::UnknownHarness)
    ));
    for sid in ["-x", "a b", &"a".repeat(129)] {
        let mut req = request("omp", dir.path(), &env);
        req.resume_sid = Some(sid);
        assert!(
            matches!(
                profiles::plan(&req),
                Err(PlanError::UnsafeToken("resume_sid"))
            ),
            "sid {sid:?}"
        );
    }
    // Relative cwd and cwd with a newline.
    let mut req = request("omp", dir.path(), &env);
    req.cwd = "relative";
    assert!(matches!(profiles::plan(&req), Err(PlanError::BadCwd)));
    let mut req = request("omp", dir.path(), &env);
    req.cwd = "/w/x\ny";
    assert!(matches!(profiles::plan(&req), Err(PlanError::BadCwd)));
    // Bad term ids.
    for tid in ["AB", "a-b", "", &"a".repeat(17)] {
        let mut req = request("omp", dir.path(), &env);
        req.term_id = tid;
        assert!(
            matches!(profiles::plan(&req), Err(PlanError::BadTermId)),
            "term {tid:?}"
        );
    }
}

#[test]
fn profiles_experimental_marker() {
    let dir = tempfile::tempdir().unwrap();
    let env = base_env();
    for (harness, want) in [
        ("grok", true),
        ("opencode", true),
        ("omp", false),
        ("claude", false),
        ("codex", false),
        ("kimi", false),
    ] {
        let plan = profiles::plan(&request(harness, dir.path(), &env)).unwrap();
        assert_eq!(plan.experimental, want, "experimental for {harness}");
    }
}

#[test]
fn profiles_exit_steps_omp_verified_sequence() {
    use kb_term::profiles::ExitStep;
    let dir = tempfile::tempdir().unwrap();
    let env = base_env();
    let plan = profiles::plan(&request("omp", dir.path(), &env)).unwrap();
    assert_eq!(
        plan.exit_steps,
        &[
            ExitStep::Keys(&["C-c"]),
            ExitStep::WaitMs(300),
            ExitStep::Keys(&["C-c"]),
            ExitStep::WaitMs(300),
            ExitStep::Keys(&["C-d"]),
        ]
    );
    // Every harness has a non-empty exit sequence with no tmux-meta
    // chars besides named keys.
    for harness in profiles::HARNESSES {
        let plan = profiles::plan(&request(harness, dir.path(), &env)).unwrap();
        assert!(!plan.exit_steps.is_empty(), "exit steps for {harness}");
        for step in plan.exit_steps {
            match step {
                ExitStep::Keys(keys) => {
                    for k in *keys {
                        assert!(
                            (1..=16).contains(&k.len())
                                && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
                            "bad key `{k}`"
                        );
                    }
                }
                ExitStep::Text(t) => assert!(!t.is_empty()),
                ExitStep::WaitMs(_) => {}
            }
        }
    }
}

#[test]
fn profiles_job_ulid_optional() {
    let dir = tempfile::tempdir().unwrap();
    let env = base_env();
    let plan = profiles::plan(&request("omp", dir.path(), &env)).unwrap();
    assert!(plan.env.iter().all(|(k, _)| k != "KB_JOB_ULID"));
    let mut req = request("omp", dir.path(), &env);
    req.job_ulid = Some("job-01ABC");
    let plan = profiles::plan(&req).unwrap();
    assert!(plan
        .env
        .contains(&("KB_JOB_ULID".to_string(), "job-01ABC".to_string())));
    // An unsafe job ulid is rejected.
    let mut req = request("omp", dir.path(), &env);
    req.job_ulid = Some("a b");
    assert!(matches!(
        profiles::plan(&req),
        Err(PlanError::UnsafeToken("job_ulid"))
    ));
}

#[test]
fn profiles_harness_set_is_the_pinned_six() {
    assert_eq!(
        profiles::HARNESSES,
        ["claude", "codex", "opencode", "grok", "kimi", "omp"]
    );
}

// ------------------------------------------------------------------ term-int

/// Term-int lane convention: `KB_TMUX_BIN` provides the pinned tmux;
/// with `KB_REQUIRE_TMUX=1` an absent tmux FAILS (same shape as the
/// reseed tests), otherwise the test early-returns.
fn tmux_bin() -> Option<PathBuf> {
    let require = std::env::var("KB_REQUIRE_TMUX").is_ok_and(|v| v == "1");
    match std::env::var("KB_TMUX_BIN") {
        Ok(v) if !v.is_empty() && std::path::Path::new(&v).exists() => Some(PathBuf::from(v)),
        _ => {
            if require {
                panic!("KB_REQUIRE_TMUX=1 but KB_TMUX_BIN is unset or missing");
            }
            None
        }
    }
}

fn unique_tag() -> String {
    format!("{}-{}", std::process::id(), {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        N.fetch_add(1, Ordering::SeqCst)
    })
}

fn run_tmux(
    tmux: &std::path::Path,
    sock: &str,
    args: &[&str],
) -> Result<(bool, String, String), ()> {
    let out = std::process::Command::new(tmux)
        .args(["-S", sock])
        .args(args)
        .env_clear()
        .env("LANG", "C.UTF-8")
        .output()
        .map_err(|_| ())?;
    Ok((
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

#[test]
fn term_int_login_profile_cannot_reintroduce_forbidden_env() {
    let Some(tmux) = tmux_bin() else { return };
    use kb_term::runner::launch::{gate_argv, GATE_SCRIPT};
    let tag = unique_tag();
    let sock = format!("/tmp/kbt-t10-{tag}.sock");
    let dir = tempfile::tempdir().unwrap();
    // A temp HOME whose login profile reintroduces forbidden vars.
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join(".bash_profile"),
        "export KITTY_WINDOW_ID=1 TERM_PROGRAM=x GHOSTTY_FOO=1\n",
    )
    .unwrap();
    // The plan env (from profiles::plan itself): KB_TERM_ID + allowlist
    // only.
    let base: Vec<(String, String)> = vec![
        ("HOME".into(), home.to_string_lossy().into_owned()),
        ("SHELL".into(), "/bin/bash".into()),
        ("PATH".into(), "/usr/bin:/bin".into()),
    ];
    let plan = profiles::plan(&PlanRequest {
        harness: "omp",
        term_id: "tid01x",
        resume_sid: None,
        cwd: "/tmp",
        job_ulid: None,
        run_dir: dir.path(),
        base_env: &base,
        extra_passthrough: &[],
    })
    .unwrap();
    let env: std::collections::BTreeMap<String, String> = plan.env.iter().cloned().collect();
    let argv = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "/usr/bin/env; exec sleep 300".to_string(),
    ];
    for with_defence in [true, false] {
        let script = if with_defence {
            let prelude = profiles::env::shell_unset_prelude(&plan.unset_after_login).unwrap();
            format!(
                "{prelude}{} {GATE_SCRIPT}",
                profiles::env::shell_unset_prefixes_snippet()
            )
        } else {
            GATE_SCRIPT.to_string()
        };
        let args = gate_argv(
            tmux.to_str().unwrap(),
            std::path::Path::new(&sock),
            "tid01x",
            std::path::Path::new("/tmp"),
            120,
            40,
            &env,
            &argv,
            &script,
        );
        // Dirty server env: simulates a login profile having reintroduced
        // the forbidden vars into the pane environment.
        let mut cmd = std::process::Command::new(&tmux);
        cmd.args(["-S", &sock]).args(&args[1..]);
        cmd.env_clear();
        cmd.env("LANG", "C.UTF-8");
        cmd.env("KITTY_WINDOW_ID", "1");
        cmd.env("TERM_PROGRAM", "x");
        cmd.env("GHOSTTY_FOO", "1");
        cmd.env("HOME", &home);
        let out = cmd.output().expect("spawn tmux new-session");
        assert!(
            out.status.success(),
            "new-session failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // Release the gate, then capture the pane.
        let released = run_tmux(&tmux, &sock, &["wait-for", "-S", "kbterm-go"]);
        assert!(matches!(released, Ok((true, _, _))), "wait-for -S failed");
        let mut captured = String::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if let Ok((true, stdout, _)) =
                run_tmux(&tmux, &sock, &["capture-pane", "-p", "-t", "kb"])
            {
                captured = stdout;
                if captured.contains("KB_TERM_ID") || captured.contains("GHOSTTY_FOO") {
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let _ = run_tmux(&tmux, &sock, &["kill-server"]);
        std::thread::sleep(std::time::Duration::from_millis(100));
        if with_defence {
            for name in ["KITTY_WINDOW_ID=", "TERM_PROGRAM=", "GHOSTTY_FOO="] {
                assert!(
                    !captured.contains(name),
                    "defence leaked {name}; pane env:\n{captured}"
                );
            }
            assert!(
                captured.contains("KB_TERM_ID=tid01x"),
                "KB_TERM_ID present; pane env:\n{captured}"
            );
            // tmux-supplied vars ARE present.
            assert!(
                captured.contains("TMUX="),
                "TMUX present; pane env:\n{captured}"
            );
            assert!(
                captured.contains("TERM="),
                "TERM present; pane env:\n{captured}"
            );
        } else {
            for name in ["KITTY_WINDOW_ID=", "TERM_PROGRAM=", "GHOSTTY_FOO="] {
                assert!(
                    captured.contains(name),
                    "control run must show {name}; pane env:\n{captured}"
                );
            }
        }
    }
}
