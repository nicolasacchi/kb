//! v0.4 D2 — `kb get` + `kb related` smoke. Pure CLI invocations
//! against an unreachable URL; full daemon-attached path is covered
//! by the kb-server e2e tests + manual smoke.

use assert_cmd::Command;

#[test]
fn get_help_lists_kb_format_daemon() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["get", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("--kb"), "missing --kb");
    assert!(text.contains("--format"), "missing --format");
    assert!(text.contains("--daemon"), "missing --daemon");
}

#[test]
fn get_dies_cleanly_when_daemon_unreachable() {
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "get",
            "abc123",
            "--kb",
            "smoke",
            "--daemon",
            "http://127.0.0.1:1",
        ])
        .assert()
        .failure();
}

#[test]
fn get_rejects_unknown_format() {
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "get",
            "abc123",
            "--kb",
            "smoke",
            "--format",
            "yaml",
            "--daemon",
            "http://127.0.0.1:1",
        ])
        .assert()
        .failure();
}

#[test]
fn related_help_lists_kb_depth_daemon_json() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["related", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("--kb"));
    assert!(text.contains("--depth"));
    assert!(text.contains("--daemon"));
    assert!(text.contains("--json"));
}

#[test]
fn related_dies_cleanly_when_daemon_unreachable() {
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "related",
            "abc123",
            "--kb",
            "smoke",
            "--daemon",
            "http://127.0.0.1:1",
        ])
        .assert()
        .failure();
}

#[test]
fn tools_emits_subcommand_per_top_level_verb() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["tools"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    // Must include every top-level verb header.
    for verb in [
        "## kb add",
        "## kb search",
        "## kb model",
        "## kb get",
        "## kb related",
        "## kb comments",
        "## kb atlas",
        "## kb push",
        "## kb token",
        "## kb tools",
        "## kb find",
        "## kb reindex",
    ] {
        assert!(text.contains(verb), "missing {verb} in tools output");
    }
}

// L1 — `kb find` and `--path` flags on `kb comments` subcommands.

#[test]
fn find_help_lists_kb_json_daemon() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["find", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("--kb"), "missing --kb");
    assert!(text.contains("--json"), "missing --json");
    assert!(text.contains("--daemon"), "missing --daemon");
}

#[test]
fn find_dies_cleanly_when_daemon_unreachable() {
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "find",
            "atlas.html",
            "--kb",
            "smoke",
            "--daemon",
            "http://127.0.0.1:1",
        ])
        .assert()
        .failure();
}

#[test]
fn comments_list_advertises_path_and_daemon_flags() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["comments", "list", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("--path"), "missing --path on `comments list`");
    assert!(
        text.contains("--daemon"),
        "missing --daemon on `comments list`"
    );
}

#[test]
fn comments_resolve_advertises_path_and_all() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["comments", "resolve", "--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.contains("--path"),
        "missing --path on `comments resolve`"
    );
    assert!(
        text.contains("--all"),
        "missing --all on `comments resolve`"
    );
}

#[test]
fn comments_resolve_requires_comment_id_or_all() {
    // No comment_id, no --all → must error before reaching the
    // network so this remains useful even when no daemon is running.
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "comments",
            "resolve",
            "--kb",
            "smoke",
            "--path",
            "atlas.html",
            "--daemon",
            "http://127.0.0.1:1",
        ])
        .assert()
        .failure();
}
