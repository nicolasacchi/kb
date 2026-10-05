//! `kb doctor --public-mirror` (v0.45 N9): the CLI surface over
//! `kb_core::public_mirror`, plus the doc-truth test that the sample config
//! published in docs/public-mirror.md passes its own check.

use assert_cmd::Command;
use predicates::str::contains;

const DOC: &str = include_str!("../../../../docs/public-mirror.md");

/// First fenced ```toml block of the doc (the published sample config).
fn doc_sample() -> String {
    let start = DOC.find("```toml\n").expect("toml block in doc") + "```toml\n".len();
    let end = start + DOC[start..].find("```").expect("closing fence");
    DOC[start..end].to_string()
}

fn run(toml: &str, extra: &[&str]) -> assert_cmd::assert::Assert {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("kb.toml");
    std::fs::write(&p, toml).unwrap();
    Command::cargo_bin("kb")
        .unwrap()
        .args(["--config", p.to_str().unwrap(), "doctor", "--public-mirror"])
        .args(extra)
        .assert()
}

#[test]
fn doc_sample_config_passes_its_own_check() {
    run(&doc_sample(), &[]).success().stdout(contains("clean"));
}

#[test]
fn bad_mirror_config_fails_with_named_findings() {
    let bad = doc_sample().replace("strip_kb_prompt = true", "strip_kb_prompt = false");
    let bad = bad.replace("hostnames = [\"docs.example.com\"]\n", "");
    run(&bad, &[])
        .code(1)
        .stdout(contains("strip-kb-prompt"))
        .stdout(contains("hostnames-unset"));
}

#[test]
fn json_output_names_rules() {
    let bad = doc_sample().replace("127.0.0.1:4000", "0.0.0.0:4000");
    let out = run(&bad, &["--json"]).code(1).get_output().stdout.clone();
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["findings"][0]["rule"], "bind-not-private");
}

#[test]
fn missing_config_file_is_an_error_not_a_clean_pass() {
    Command::cargo_bin("kb")
        .unwrap()
        .args([
            "--config",
            "/nonexistent/kb.toml",
            "doctor",
            "--public-mirror",
        ])
        .assert()
        .failure()
        .stderr(contains("not found"));
}

#[test]
fn doc_lists_every_rule_id() {
    for r in kb_core::public_mirror::RULES {
        assert!(DOC.contains(&format!("`{r}`")), "doc lacks rule `{r}`");
    }
}
