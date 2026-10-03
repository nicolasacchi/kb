//! `kb version` (v0.44 F8): the hook contract the kb-memory hooks compare
//! against, and the build stamp `kb doctor --hooks` checks for skew.

use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn version_contract_prints_one_bare_integer() {
    Command::cargo_bin("kb")
        .unwrap()
        .args(["version", "--contract"])
        .assert()
        .success()
        .stdout("1\n");
}

#[test]
fn version_json_names_stamp_and_contract() {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["version", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON");
    assert_eq!(v["hook_contract"], 1);
    assert!(v["build_sha"].is_string());
    assert!(v["stamp_missing"].is_boolean());
}

#[test]
fn version_human_line_carries_the_contract() {
    Command::cargo_bin("kb")
        .unwrap()
        .args(["version"])
        .assert()
        .success()
        .stdout(contains("hook-contract 1"));
}
