//! SG-Q3 — `dedup_session=1` on `GET /api/memory/recall` and `GET /api/turn`.
//!
//! The contract under test: the exclusion runs AFTER scoring and BEFORE the
//! limit, so every survivor's score decomposition is byte-identical to the
//! same request without dedup (with and without `scoring_v2_relevance`), the
//! excluded hit is replaced by the next one, an absent parameter is today's
//! response, and a missing ledger degrades loudly instead of failing.
use crate::common;
use crate::slo::{boot_with_memory, get_json, url};
use kb_core::config::MemorySection;

const SID: &str = "sid-dedup-0001";

fn memory_doc(title: &str, salience: &str) -> String {
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>{title}</title>\
         <meta name=\"kb-category\" content=\"memory-user\">\
         <meta name=\"kb-salience\" content=\"{salience}\"></head>\
         <body><h1>{title}</h1><p>quokkaword is served here</p></body></html>"
    )
}

fn transcript(sid: &str) -> (String, String) {
    let now = chrono::Utc::now();
    let ts = now.format("%Y%m%dT%H%M%SZ").to_string();
    let iso = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let jsonl = format!(
        "{{\"type\":\"user\",\"sessionId\":\"{sid}\",\"timestamp\":\"{iso}\",\"promptSource\":\"typed\",\"message\":{{\"role\":\"user\",\"content\":\"hello\"}}}}\n"
    )
    .replace('&', "&amp;")
    .replace('<', "&lt;")
    .replace('>', "&gt;");
    let html = format!(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <title>Session transcript {ts}</title>\
         <meta name=\"kb-category\" content=\"memory-session\">\
         <meta name=\"kb-session\" content=\"{sid}\"></head><body>\
         <h1>Session transcript {ts}</h1><pre>{jsonl}</pre></body></html>"
    );
    (format!("session-{ts}-{sid}.html"), html)
}

async fn boot_sessions(relevance: bool) -> (tempfile::TempDir, std::net::SocketAddr) {
    let (tname, thtml) = transcript(SID);
    let memory = MemorySection {
        scoring_v2_relevance: relevance,
        ..Default::default()
    };
    boot_with_memory(
        "sessions",
        Some("global"),
        &[
            ("mem-a.html", memory_doc("Quokka alpha", "0.9")),
            ("mem-b.html", memory_doc("Quokka bravo", "0.8")),
            ("mem-c.html", memory_doc("Quokka charlie", "0.7")),
            (tname.as_str(), thtml),
        ],
        None,
        memory,
    )
    .await
}

fn ids(v: &serde_json::Value) -> Vec<String> {
    v["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["id"].as_str().unwrap().to_string())
        .collect()
}

/// Serve the current top hit to `SID` and wait until the ledger shows it.
async fn seed_one_serve(client: &reqwest::Client, addr: std::net::SocketAddr) -> String {
    let top = get_json(client, addr, "/api/memory/recall?q=quokkaword&limit=3").await;
    assert_eq!(ids(&top).len(), 3, "three memories indexed: {top}");
    let first = ids(&top)[0].clone();
    common::poll_until("the three memories to rank stably", || async {
        let b = get_json(client, addr, "/api/memory/recall?q=quokkaword&limit=3").await;
        (ids(&b).first() == Some(&first)).then_some(())
    })
    .await;
    let _ = client
        .get(url(
            addr,
            &format!("/api/memory/recall?q=quokkaword&limit=1&session={SID}"),
        ))
        .send()
        .await
        .unwrap();
    common::poll_until("the served row in the ledger", || async {
        let r = client
            .get(url(addr, &format!("/api/sessions/{SID}/recalls")))
            .send()
            .await
            .ok()?;
        if !r.status().is_success() {
            return None;
        }
        let b: serde_json::Value = r.json().await.ok()?;
        (!b["recalls"].as_array()?.is_empty()).then_some(())
    })
    .await;
    first
}

async fn survivors_match_baseline(relevance: bool) {
    let (_tmp, addr) = boot_sessions(relevance).await;
    let client = reqwest::Client::new();
    let first = seed_one_serve(&client, addr).await;

    let baseline = get_json(&client, addr, "/api/memory/recall?q=quokkaword&limit=3").await;
    let base_hits = baseline["hits"].as_array().unwrap().clone();
    assert_eq!(base_hits[0]["id"], first.as_str());
    assert!(
        baseline.get("dedup_removed").is_none(),
        "parameter absent: no new field: {baseline}"
    );

    // limit=2 with dedup: the served top hit is excluded and the next one
    // takes its place; each survivor equals its baseline hit byte for byte.
    let deduped = get_json(
        &client,
        addr,
        &format!("/api/memory/recall?q=quokkaword&limit=2&session={SID}&dedup_session=1"),
    )
    .await;
    assert_eq!(deduped["dedup_removed"], 1, "{deduped}");
    let got = deduped["hits"].as_array().unwrap();
    assert_eq!(got.len(), 2, "replaced, not shortened: {deduped}");
    assert_eq!(got[0], base_hits[1], "relevance={relevance}");
    assert_eq!(got[1], base_hits[2], "relevance={relevance}");
    assert!(!ids(&deduped).contains(&first));
}

#[tokio::test]
async fn dedup_survivors_are_byte_identical_with_relevance_on() {
    survivors_match_baseline(true).await;
}

#[tokio::test]
async fn dedup_survivors_are_byte_identical_with_relevance_off() {
    survivors_match_baseline(false).await;
}

#[tokio::test]
async fn dedup_needs_session_and_served_after_resets_the_window() {
    let (_tmp, addr) = boot_sessions(true).await;
    let client = reqwest::Client::new();
    let first = seed_one_serve(&client, addr).await;

    // dedup_session without session= is not meaningful: today's response.
    let no_session = get_json(
        &client,
        addr,
        "/api/memory/recall?q=quokkaword&limit=3&dedup_session=1",
    )
    .await;
    assert!(ids(&no_session).contains(&first));
    assert!(no_session.get("dedup_removed").is_none());

    // served_after in the future: nothing counts as served any more (the
    // compact reset), so nothing is removed and the top hit is back.
    let future = chrono::Utc::now().timestamp() + 3600;
    let reset = get_json(
        &client,
        addr,
        &format!(
            "/api/memory/recall?q=quokkaword&limit=3&session={SID}&dedup_session=1&served_after={future}"
        ),
    )
    .await;
    assert!(ids(&reset).contains(&first), "{reset}");
    assert!(reset.get("dedup_removed").is_none(), "{reset}");
}

#[tokio::test]
async fn turn_route_drops_served_hits_too() {
    let (_tmp, addr) = boot_sessions(true).await;
    let client = reqwest::Client::new();
    let first = seed_one_serve(&client, addr).await;

    let plain = get_json(
        &client,
        addr,
        &format!("/api/turn?q=quokkaword&lanes=recall&session={SID}x"),
    )
    .await;
    assert!(plain.get("dedup_removed").is_none());
    assert_eq!(plain["recalled"].as_array().unwrap().len(), 3, "{plain}");

    let deduped = get_json(
        &client,
        addr,
        &format!("/api/turn?q=quokkaword&lanes=recall&session={SID}&dedup_session=1"),
    )
    .await;
    assert_eq!(deduped["dedup_removed"], 1, "{deduped}");
    let rec = deduped["recalled"].as_array().unwrap();
    assert_eq!(rec.len(), 2, "{deduped}");
    assert!(rec.iter().all(|h| h["id"] != first.as_str()));
}

#[tokio::test]
async fn missing_ledger_degrades_loudly_and_dedups_nothing() {
    // A kb that is neither `sessions` nor `memory-session`-categorised: the
    // route has no ledger to read.
    let (_tmp, addr) = boot_with_memory(
        "smoke",
        Some("global"),
        &[
            ("mem-a.html", memory_doc("Quokka alpha", "0.9")),
            ("mem-b.html", memory_doc("Quokka bravo", "0.8")),
        ],
        None,
        MemorySection::default(),
    )
    .await;
    let client = reqwest::Client::new();
    let body = common::poll_until("memories indexed", || async {
        let b = get_json(
            &client,
            addr,
            "/api/memory/recall?q=quokkaword&limit=5&session=s1&dedup_session=1",
        )
        .await;
        (b["hits"].as_array().is_some_and(|h| h.len() == 2)).then_some(b)
    })
    .await;
    assert!(body.get("dedup_removed").is_none(), "{body}");
    let degraded = body["degraded"].as_array().expect("degraded[] names it");
    assert!(
        degraded.iter().any(|d| d["lane"] == "recall.dedup"),
        "{body}"
    );
}
