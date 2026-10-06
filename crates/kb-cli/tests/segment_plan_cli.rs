//! `kb sessions segment-plan` through the real binary: the wire shape and the
//! `--emit` stream the adapter consumes (v0.46 SEG-B).

use assert_cmd::Command;
use serde_json::Value;

fn session() -> String {
    let mut s = String::new();
    s.push_str("{\"type\":\"session\",\"id\":\"sid-1\",\"cwd\":\"/c\"}\n");
    s.push_str("{\"type\":\"title\",\"title\":\"  A title  \"}\n");
    let mut parent = "null".to_string();
    for i in 0..6 {
        let id = format!("\"m{i}\"");
        let role = if i % 2 == 0 { "user" } else { "assistant" };
        s.push_str(&format!(
            "{{\"type\":\"message\",\"id\":{id},\"parentId\":{parent},\"message\":{{\"role\":\"{role}\",\"content\":[{{\"type\":\"text\",\"text\":\"{}\"}}]}}}}\n",
            "z".repeat(300)
        ));
        parent = id;
    }
    s
}

#[test]
fn segment_plan_prints_the_plan_and_emits_parts() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("s.jsonl");
    std::fs::write(&src, session()).unwrap();
    let state = dir.path().join("st");
    let out = Command::cargo_bin("kb")
        .unwrap()
        .args(["sessions", "segment-plan", "--source"])
        .arg(&src)
        .arg("--state")
        .arg(&state)
        .args(["--target-bytes", "700", "--print-chain"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema"], "segment-plan/1");
    assert_eq!(v["session_id"], "sid-1");
    assert_eq!(v["title"], "A title");
    assert_eq!(v["checkpoint"]["status"], "absent");
    assert_eq!(v["chain_ids"].as_array().unwrap().len(), 6);
    let parts = v["parts"].as_array().unwrap();
    assert!(parts.len() >= 2, "{v}");
    assert_eq!(parts[0]["state"], "frozen");
    assert_eq!(parts[parts.len() - 1]["state"], "live");
    assert!(state.exists());

    let emit = Command::cargo_bin("kb")
        .unwrap()
        .args(["sessions", "segment-plan", "--source"])
        .arg(&src)
        .arg("--state")
        .arg(&state)
        .args(["--target-bytes", "700", "--emit", "1"])
        .output()
        .unwrap();
    assert!(emit.status.success());
    let text = String::from_utf8(emit.stdout).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines[0].contains("\"type\":\"session\""));
    assert!(lines[1].contains("\"type\":\"title\""));
    assert!(lines[2].contains("\"m0\""));

    let bad = Command::cargo_bin("kb")
        .unwrap()
        .args(["sessions", "segment-plan", "--source"])
        .arg(&src)
        .arg("--state")
        .arg(&state)
        .args(["--target-bytes", "700", "--emit", "99"])
        .output()
        .unwrap();
    assert!(!bad.status.success());
}
