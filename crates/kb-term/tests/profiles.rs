//! Launch-profile tests (`profiles_*`). No harness is ever run; all
//! fixtures synthetic. The tmux-needing login-defence test lives in
//! the `integration` binary (`term_int_login_profile_...`), the only
//! test binary the term-int CI job runs with a real tmux.

use kb_term::profiles::{self, PlanError, PlanRequest};

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
        assert!(
            matches!(profiles::plan(&req), Err(PlanError::ForbiddenEnvName(name)) if name == bad),
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

/// The exact sorted env every harness shares (`HOME`/`LANG`/`PATH`/
/// `USER` from `base_env()`, plus `KB_TERM_ID` from `request()`).
/// Per-harness profile env is inserted in sorted position.

/// One golden case: (harness, exact argv, exact sorted env).
type GoldenCase = (&'static str, Vec<&'static str>, Vec<(String, String)>);
fn shared_env() -> Vec<(String, String)> {
    vec![
        ("HOME".to_string(), "/home/x".to_string()),
        ("KB_TERM_ID".to_string(), "abc123".to_string()),
        ("LANG".to_string(), "C.UTF-8".to_string()),
        ("PATH".to_string(), "/usr/bin:/bin".to_string()),
        ("USER".to_string(), "u".to_string()),
    ]
}

/// Insert `(k, v)` into a sorted env vector (test helper).
fn with_entry(mut env: Vec<(String, String)>, k: &str, v: String) -> Vec<(String, String)> {
    env.push((k.to_string(), v));
    env.sort_by(|a, b| a.0.cmp(&b.0));
    env
}

#[test]
fn profiles_golden_plan_per_harness_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let env = base_env();
    let overlay = dir.path().join("omp-overlay.yml");
    let cases: Vec<GoldenCase> = vec![
        (
            "omp",
            vec!["omp"],
            with_entry(
                with_entry(
                    shared_env(),
                    "PI_CONFIG_FILES",
                    overlay.to_string_lossy().into_owned(),
                ),
                "PI_FORCE_IMAGE_PROTOCOL",
                "kitty".to_string(),
            ),
        ),
        (
            "claude",
            vec!["claude"],
            with_entry(
                shared_env(),
                "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN",
                "1".to_string(),
            ),
        ),
        ("codex", vec!["codex", "--no-alt-screen"], shared_env()),
        (
            "kimi",
            vec!["kimi"],
            with_entry(shared_env(), "PI_IMAGE_PROTOCOL", "iterm2".to_string()),
        ),
        (
            "grok",
            vec!["grok", "--minimal", "--no-alt-screen"],
            shared_env(),
        ),
        ("opencode", vec!["opencode", "--mini"], shared_env()),
    ];
    for (harness, want_argv, want_env) in cases {
        let plan = profiles::plan(&request(harness, dir.path(), &env)).unwrap();
        let want: Vec<String> = want_argv.iter().map(|s| s.to_string()).collect();
        assert_eq!(plan.argv, want, "fresh argv for {harness}");
        assert_eq!(plan.env, want_env, "fresh env for {harness}");
    }
}

#[test]
fn profiles_golden_plan_per_harness_resume() {
    let dir = tempfile::tempdir().unwrap();
    let env = base_env();
    let overlay = dir.path().join("omp-overlay.yml");
    let cases: Vec<GoldenCase> = vec![
        (
            "omp",
            vec!["omp", "-r", "sid-1"],
            with_entry(
                with_entry(
                    shared_env(),
                    "PI_CONFIG_FILES",
                    overlay.to_string_lossy().into_owned(),
                ),
                "PI_FORCE_IMAGE_PROTOCOL",
                "kitty".to_string(),
            ),
        ),
        (
            "claude",
            vec!["claude", "-r", "sid-1"],
            with_entry(
                shared_env(),
                "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN",
                "1".to_string(),
            ),
        ),
        (
            "codex",
            vec!["codex", "--no-alt-screen", "resume", "sid-1"],
            shared_env(),
        ),
        (
            "kimi",
            vec!["kimi", "-S", "sid-1"],
            with_entry(shared_env(), "PI_IMAGE_PROTOCOL", "iterm2".to_string()),
        ),
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
            shared_env(),
        ),
        (
            "opencode",
            vec!["opencode", "--mini", "--session", "sid-1"],
            shared_env(),
        ),
    ];
    for (harness, want_argv, want_env) in cases {
        let mut req = request(harness, dir.path(), &env);
        req.resume_sid = Some("sid-1");
        let plan = profiles::plan(&req).unwrap();
        let want: Vec<String> = want_argv.iter().map(|s| s.to_string()).collect();
        assert_eq!(plan.argv, want, "resume argv for {harness}");
        assert_eq!(plan.env, want_env, "resume env for {harness}");
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
    let req = request("zzz", dir.path(), &env);
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

/// Overlay write failures are host state (`PlanError::Io`), not a bad
/// request: an unwritable run dir (here: a run dir under a plain file,
/// so `create_dir_all` fails) yields `Io` and creates no file.
#[test]
fn profiles_overlay_io_failure_is_io_error() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"x").unwrap();
    let run_dir = blocker.join("run");
    let env = base_env();
    let req = request("omp", &run_dir, &env);
    let result = profiles::plan(&req);
    assert!(
        matches!(result, Err(PlanError::Io(_))),
        "unwritable run dir must be Io, not BadCwd: {result:?}"
    );
    assert!(!run_dir.join("omp-overlay.yml").exists());
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
