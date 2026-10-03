//! `kb import claude-memory` dry-run: no daemon, writes nothing, golden JSON.

use assert_cmd::Command;

fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let mem = tmp.path().join("-home-alice-project-kb/memory");
    std::fs::create_dir_all(&mem).unwrap();
    std::fs::write(mem.join("MEMORY.md"), "- [x](x.md)").unwrap();
    std::fs::write(
        mem.join("x.md"),
        "---\nname: Use fast profile\ndescription: speed\nmetadata:\n  type: feedback\n---\nBuild with --profile fast.\n",
    )
    .unwrap();
    tmp
}

#[test]
fn dry_run_json_maps_and_writes_nothing() {
    let tmp = fixture();
    let before: Vec<_> = walk(tmp.path());
    let out = Command::cargo_bin("kb")
        .unwrap()
        .env("HOME", "/home/alice")
        .env("KB_HOME", tmp.path().join("kbhome"))
        .args(["import", "claude-memory", "--json", "--dir"])
        .arg(tmp.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["dry_run"], true);
    let c = &v["candidates"][0];
    assert_eq!(c["title"], "Use fast profile");
    assert_eq!(c["target"], "memory-kb");
    assert_eq!(c["status"], "new");
    assert_eq!(c["tags"][0], "imported-claude-memory");
    assert_eq!(c["tags"][1], "claude-type-feedback");
    assert_eq!(v["skipped"][0]["reason"], "index-file");
    assert_eq!(v["submitted"].as_array().unwrap().len(), 0);
    assert_eq!(walk(tmp.path()), before, "dry-run must not write");
}

#[test]
fn link_override_replaces_derived_target() {
    let tmp = fixture();
    let out = Command::cargo_bin("kb")
        .unwrap()
        .env("HOME", "/home/alice")
        .args([
            "import",
            "claude-memory",
            "--json",
            "--link",
            "memory-other",
            "--dir",
        ])
        .arg(tmp.path())
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["candidates"][0]["target"], "memory-other");
}

fn walk(p: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = walkdir::WalkDir::new(p)
        .into_iter()
        .filter_map(|e| e.ok())
        .map(|e| e.path().display().to_string())
        .collect();
    v.sort();
    v
}
