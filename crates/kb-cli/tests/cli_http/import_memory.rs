//! `kb import claude-memory` against a live daemon: the dry-run reads (never
//! writes) to show duplicates, `--apply` queues through the real proposals
//! route, the footer carries no encoded home path, and approving the proposal
//! writes the SOURCE FILE's mtime as the memory's `kb-created`.

use assert_cmd::Command;
use serde_json::Value;
use std::net::SocketAddr;
use std::time::Duration;

const MTIME: u64 = 1_600_000_000; // 2020-09-13, well in the past

async fn boot() -> (tempfile::TempDir, SocketAddr) {
    let tmp = tempfile::tempdir().unwrap();
    let mdir = tmp.path().join("memory-kb");
    std::fs::create_dir_all(&mdir).unwrap();
    let mut section = super::common::kb_section(mdir);
    section.memory_scope = Some("project".into());
    let mut kbs = std::collections::BTreeMap::new();
    kbs.insert(kb_core::types::KbName::new("memory-kb").unwrap(), section);
    let name = format!(
        "cli-impmem-{}",
        tmp.path().file_name().unwrap().to_string_lossy()
    );
    let cfg = super::common::base_config(&name, kbs);
    let paths = kb_core::paths::KbPaths::rooted_at(tmp.path(), name);
    let (addr, _task) = kb_server::serve_on_random_port_with_paths(cfg, paths)
        .await
        .expect("serve");
    tokio::time::sleep(Duration::from_millis(400)).await;
    (tmp, addr)
}

fn fixture(root: &std::path::Path) {
    let mem = root.join("-home-alice-project-kb/memory");
    std::fs::create_dir_all(&mem).unwrap();
    let f = mem.join("x.md");
    std::fs::write(
        &f,
        "---\nname: Use fast profile\ndescription: speed\nmetadata:\n  type: feedback\n---\nBuild with --profile fast.\n",
    )
    .unwrap();
    let t = std::time::UNIX_EPOCH + Duration::from_secs(MTIME);
    std::fs::File::options()
        .write(true)
        .open(&f)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

fn import(addr: SocketAddr, dir: &std::path::Path, extra: &[&str]) -> Value {
    let out = Command::cargo_bin("kb")
        .unwrap()
        .env("HOME", "/home/alice")
        .args([
            "import",
            "claude-memory",
            "--json",
            "--kb",
            "memory-kb",
            "--daemon",
        ])
        .arg(format!("http://{addr}"))
        .arg("--dir")
        .arg(dir)
        .args(extra)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    serde_json::from_slice(&out).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_queues_once_dedupes_and_approval_keeps_the_source_mtime() {
    let (tmp, addr) = boot().await;
    let src = tmp.path().join("claude-projects");
    fixture(&src);
    let url = format!("http://{addr}");
    let client = reqwest::Client::new();

    // Dry-run with a reachable daemon: read-only, status is a real `new`.
    let dry = import(addr, &src, &[]);
    assert_eq!(dry["candidates"][0]["status"], "new", "{dry}");
    let queue: Value = client
        .get(format!("{url}/api/proposals"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        queue["items"].as_array().unwrap().len(),
        0,
        "dry-run must not queue"
    );

    // --apply queues exactly one proposal.
    let applied = import(addr, &src, &["--apply"]);
    assert_eq!(
        applied["submitted"].as_array().unwrap().len(),
        1,
        "{applied}"
    );
    let queue: Value = client
        .get(format!("{url}/api/proposals"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let items = queue["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{queue}");
    let item = &items[0];
    let body = item["body"].as_str().unwrap();
    assert!(body.contains("`kb/memory/x.md`"), "{body}");
    assert!(
        !body.contains("-home-alice"),
        "encoded home path leaked: {body}"
    );
    assert_eq!(item["memory_created_at"], MTIME, "{item}");

    // A second dry-run now SHOWS the duplicate; a second --apply queues nothing.
    let again = import(addr, &src, &[]);
    assert_eq!(again["candidates"][0]["status"], "duplicate", "{again}");
    let reapplied = import(addr, &src, &["--apply"]);
    assert_eq!(
        reapplied["submitted"].as_array().unwrap().len(),
        0,
        "{reapplied}"
    );

    // Approval writes the memory with the FILE's mtime as its creation date.
    let id = item["id"].as_str().unwrap();
    let resp = client
        .post(format!("{url}/api/kb/memory-kb/proposals/{id}/approve"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "{}", resp.status());
    let written: Vec<_> = std::fs::read_dir(tmp.path().join("memory-kb"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "html"))
        .collect();
    assert_eq!(written.len(), 1);
    let html = std::fs::read_to_string(written[0].path()).unwrap();
    assert!(
        html.contains(&format!("<meta name=\"kb-created\" content=\"{MTIME}\"")),
        "kb-created must be the source mtime, not now: {html}"
    );
}
