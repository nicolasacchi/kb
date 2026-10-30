//! OL TIME2 — `?at=<unix>` on an artifact's own origin serves the BODY as it
//! stood (nearest-prior through the versions façade), pushed through today's
//! outbound pipeline. Boots the in-process daemon (the `docs_asof.rs`
//! convention) and requests the artifact subdomain via a `Host` header.
use crate::common;
use crate::docs_asof::{boot_with, now, page, session_doc, url};

use kb_core::config::OutboundSection;
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

const V1: &str = "version-one-marker";
const V2: &str = "version-two-marker";
const PROMPT: &str = "secret-prompt-marker";

fn body_of(marker: &str) -> String {
    format!(
        "<!doctype html><html><head><title>T</title></head><body><h1>T</h1>\
         <template id=\"kb-prompt\">{PROMPT}</template><p>{marker}</p></body></html>"
    )
}

struct Fx {
    _tmp: tempfile::TempDir,
    addr: std::net::SocketAddr,
    client: reqwest::Client,
    id: String,
    /// `ts_unix` of the older index snapshot (version one).
    ts_old: i64,
    /// `ts_unix` of the newer index snapshot (version two).
    ts_new: i64,
}

async fn versions(c: &reqwest::Client, addr: std::net::SocketAddr, id: &str) -> Vec<Value> {
    let v: Value = c
        .get(url(addr, &format!("/api/kb/smoke/artifacts/{id}/versions")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    v["versions"].as_array().cloned().unwrap_or_default()
}

fn index_ts(vs: &[Value]) -> Vec<i64> {
    vs.iter()
        .filter(|v| v["source"] == "index")
        .map(|v| v["ts_unix"].as_i64().unwrap())
        .collect()
}

/// An artifact with TWO index snapshots (version one, then version two a few
/// seconds later) and the working tree holding version two.
async fn fixture(outbound: Option<OutboundSection>) -> Fx {
    let (tmp, addr, source): (_, _, PathBuf) =
        boot_with(&[("a.html", body_of(V1))], Some("index"), outbound).await;
    let client = reqwest::Client::new();
    let id = common::poll_until("doc indexed", || async {
        let docs: Vec<Value> = client
            .get(url(addr, "/api/kb/smoke/docs?limit=10"))
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        docs.first().map(|d| d["id"].as_str().unwrap().to_string())
    })
    .await;
    common::poll_until("first snapshot", || async {
        (index_ts(&versions(&client, addr, &id).await).len() == 1).then_some(())
    })
    .await;
    // Snapshot `captured_at` is whole seconds: leave a clear gap.
    tokio::time::sleep(Duration::from_secs(3)).await;
    std::fs::write(source.join("a.html"), body_of(V2)).unwrap();
    let ts = common::poll_until("second snapshot", || async {
        let t = index_ts(&versions(&client, addr, &id).await);
        (t.len() == 2).then_some(t)
    })
    .await;
    let (ts_new, ts_old) = (ts[0].max(ts[1]), ts[0].min(ts[1]));
    assert!(
        ts_new >= ts_old + 2,
        "fixture needs a gap: {ts_old} {ts_new}"
    );
    Fx {
        _tmp: tmp,
        addr,
        client,
        id,
        ts_old,
        ts_new,
    }
}

impl Fx {
    async fn get(&self, query: &str, xff: Option<&str>) -> reqwest::Response {
        let mut req = self
            .client
            .get(url(self.addr, &format!("/{query}")))
            .header("Host", format!("{}.artifacts.localhost", self.id));
        if let Some(x) = xff {
            req = req.header("X-Forwarded-For", x);
        }
        req.send().await.unwrap()
    }
}

#[tokio::test]
async fn at_between_two_snapshots_serves_the_older_body_nearest_prior() {
    let fx = fixture(None).await;
    // Today's body is version two.
    let today = fx.get("", None).await.text().await.unwrap();
    assert!(today.contains(V2) && !today.contains(V1));

    // An instant between the snapshots lands on version one, nearest-prior.
    let resp = fx.get(&format!("?at={}", fx.ts_old + 1), None).await;
    assert_eq!(resp.status(), 200);
    let h = resp.headers().clone();
    assert_eq!(h["x-kb-as-of-relation"], "nearest-prior");
    assert!(h.contains_key("memento-datetime"));
    assert!(h["cache-control"].to_str().unwrap().contains("no-store"));
    let body = resp.text().await.unwrap();
    assert!(body.contains(V1) && !body.contains(V2), "{body}");
    // Today's pipeline still ran over it: the probe and runtime are injected.
    assert!(body.contains("/_kb/probe.js") && body.contains("/_kb/runtime.js"));

    // The same second as the older snapshot is an exact hit.
    let resp = fx.get(&format!("?at={}", fx.ts_old), None).await;
    assert_eq!(resp.headers()["x-kb-as-of-relation"], "exact");
    assert!(resp.text().await.unwrap().contains(V1));

    // At or after the newer snapshot it is version two again.
    let body = fx
        .get(&format!("?at={}", fx.ts_new), None)
        .await
        .text()
        .await
        .unwrap();
    assert!(body.contains(V2) && !body.contains(V1));
}

#[tokio::test]
async fn at_after_the_working_tree_serves_todays_file_unchanged() {
    let fx = fixture(None).await;
    let today = fx.get("", None).await;
    assert!(!today.headers().contains_key("memento-datetime"));
    let today = today.text().await.unwrap();
    // Any instant is answered; a far-future one is the working tree.
    let far = now() + 400 * 86_400;
    let resp = fx.get(&format!("?at={far}"), None).await;
    assert_eq!(resp.status(), 200);
    assert!(!resp.headers().contains_key("memento-datetime"));
    assert_eq!(resp.text().await.unwrap(), today);
}

#[tokio::test]
async fn at_below_the_floor_is_an_honest_404_naming_the_floor() {
    let fx = fixture(None).await;
    let resp = fx.get("?at=1000", None).await;
    assert_eq!(resp.status(), 404);
    let ct = resp.headers()["content-type"].to_str().unwrap().to_string();
    assert!(ct.contains("problem+json"), "{ct}");
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("that old") && body.contains("oldest known version"),
        "{body}"
    );
    // Never a silent degrade to the oldest (or the current) body.
    assert!(!body.contains(V1) && !body.contains(V2));
}

#[tokio::test]
async fn malformed_at_is_a_400_not_todays_body() {
    let fx = fixture(None).await;
    for q in ["?at=yesterday", "?at=", "?at=1.5"] {
        let resp = fx.get(q, None).await;
        assert_eq!(resp.status(), 400, "{q}");
    }
}

#[tokio::test]
async fn a_historical_render_writes_no_history_row() {
    let fx = fixture(None).await;
    let history = |c: reqwest::Client, addr| async move {
        let v: Value = c
            .get(url(addr, "/api/kb/smoke/history?limit=50"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        v["entries"].as_array().unwrap().len()
    };
    let before = history(fx.client.clone(), fx.addr).await;
    let resp = fx.get(&format!("?at={}", fx.ts_old + 1), None).await;
    assert_eq!(resp.status(), 200);
    let _ = resp.text().await.unwrap();
    assert_eq!(history(fx.client.clone(), fx.addr).await, before);
    assert_eq!(before, 0, "nothing in this fixture opens the artifact");
}

#[tokio::test]
async fn a_historical_render_never_appends_the_annotator() {
    let fx = fixture(None).await;
    let live = fx.get("?cm=on", None).await.text().await.unwrap();
    assert!(
        live.contains("/_kb/annotate.js"),
        "control: cm=on injects it"
    );
    let then = fx
        .get(&format!("?cm=on&at={}", fx.ts_old + 1), None)
        .await
        .text()
        .await
        .unwrap();
    assert!(then.contains(V1));
    assert!(!then.contains("/_kb/annotate.js"));
}

#[tokio::test]
async fn kb_prompt_is_stripped_from_a_historical_body_on_non_loopback() {
    let fx = fixture(Some(OutboundSection {
        strip_kb_prompt: true,
        redactions: Vec::new(),
    }))
    .await;
    let at = format!("?at={}", fx.ts_old + 1);
    // Control: a loopback reader still sees the prompt in the OLD body.
    let local = fx.get(&at, None).await.text().await.unwrap();
    assert!(local.contains(V1) && local.contains(PROMPT));
    // A request that looks non-loopback gets today's scrub over the old body.
    let remote = fx.get(&at, Some("8.8.8.8")).await.text().await.unwrap();
    assert!(remote.contains(V1), "still the older body");
    assert!(!remote.contains(PROMPT), "kb-prompt must be stripped");
}

#[tokio::test]
async fn transcripts_and_versions_off_are_refused_not_faked() {
    let (_t, addr, _src) = boot_with(&[("s.html", session_doc("S"))], Some("index"), None).await;
    let c = reqwest::Client::new();
    let id = common::poll_until("indexed", || async {
        let docs: Vec<Value> = c
            .get(url(addr, "/api/kb/smoke/docs?limit=10"))
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        docs.first().map(|d| d["id"].as_str().unwrap().to_string())
    })
    .await;
    let r = c
        .get(url(addr, &format!("/?at={}", now())))
        .header("Host", format!("{id}.artifacts.localhost"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);

    let (_t2, addr2, _src2) = boot_with(&[("a.html", page("A"))], Some("off"), None).await;
    let id2 = common::poll_until("indexed", || async {
        let docs: Vec<Value> = c
            .get(url(addr2, "/api/kb/smoke/docs?limit=10"))
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        docs.first().map(|d| d["id"].as_str().unwrap().to_string())
    })
    .await;
    let r = c
        .get(url(addr2, &format!("/?at={}", now())))
        .header("Host", format!("{id2}.artifacts.localhost"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    assert!(r.text().await.unwrap().contains("versions are off"));
}
