//! OL IDEA5 route tests for `GET /api/changes`. Boots the in-process daemon
//! (the `slates.rs` convention) and drives the route end to end. The fold,
//! the window arithmetic and the wire-shape guarantees are unit-pinned in
//! `routes/changes.rs`; what these pin is the PLUMBING: the auth posture,
//! the 400s, the slate lane reading the real ledger on disk, and the
//! scrub floor on the wire.
use crate::slates::{body, boot, post, url, FIXTURE_TOKEN};

use serde_json::{json, Value};

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

async fn changes(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    query: &str,
) -> (u16, Value) {
    let resp = client
        .get(url(addr, &format!("/api/changes{query}")))
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or(Value::Null))
}

fn since_query(secs_ago: i64) -> String {
    format!("?since={}", now() - secs_ago)
}

#[tokio::test]
async fn changes_token_less_non_loopback_is_401() {
    let (_tmp, addr, _paths) = boot().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(url(addr, &format!("/api/changes{}", since_query(3600))))
        .header("X-Forwarded-For", "203.0.113.9")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 401);
}

#[tokio::test]
async fn changes_bearer_non_loopback_200_without_cwd_or_body() {
    let (_tmp, addr, _paths) = boot().await;
    let client = reqwest::Client::new();
    let b = json!({
        "kind": "now",
        "line": "wiring the changes route",
        "body": "BODY-MARKER with detail",
        "prov": {"harness": "claude", "session_id": "sess-a", "cwd": "/srv/abs/secret"},
    });
    assert_eq!(post(&client, addr, "orchard", b).await.0, 201);

    let resp = client
        .get(url(addr, &format!("/api/changes{}", since_query(3600))))
        .header("X-Forwarded-For", "203.0.113.9")
        .header("Authorization", format!("Bearer {FIXTURE_TOKEN}"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    assert_eq!(
        resp.headers()
            .get("cache-control")
            .unwrap()
            .to_str()
            .unwrap(),
        "no-store"
    );
    let text = resp.text().await.unwrap();
    assert!(
        text.contains("wiring the changes route"),
        "the line rides: {text}"
    );
    for needle in ["/srv/abs", "secret", "BODY-MARKER"] {
        assert!(!text.contains(needle), "wire leaked {needle:?}: {text}");
    }
}

#[tokio::test]
async fn changes_requires_since_400() {
    let (_tmp, addr, _paths) = boot().await;
    let client = reqwest::Client::new();
    let (status, v) = changes(&client, addr, "").await;
    assert_eq!(status, 400, "{v}");
    assert!(v["detail"].as_str().unwrap().contains("since"), "{v}");
}

#[tokio::test]
async fn changes_unparseable_since_400() {
    let (_tmp, addr, _paths) = boot().await;
    let client = reqwest::Client::new();
    let (status, v) = changes(&client, addr, "?since=yesterday").await;
    assert_eq!(status, 400, "{v}");
}

#[tokio::test]
async fn changes_future_since_400() {
    let (_tmp, addr, _paths) = boot().await;
    let client = reqwest::Client::new();
    let (status, v) = changes(&client, addr, &format!("?since={}", now() + 3600)).await;
    assert_eq!(status, 400, "{v}");
}

#[tokio::test]
async fn changes_small_skew_is_empty_200() {
    let (_tmp, addr, _paths) = boot().await;
    let client = reqwest::Client::new();
    let (status, v) = changes(&client, addr, &format!("?since={}", now() + 60)).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["projects"], json!([]));
    assert!(v["since_unix"].as_i64().unwrap() <= v["to_unix"].as_i64().unwrap() + 1);
}

#[tokio::test]
async fn changes_empty_window_shape() {
    let (_tmp, addr, _paths) = boot().await;
    let client = reqwest::Client::new();
    let (status, v) = changes(&client, addr, &since_query(3600)).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["projects"], json!([]));
    assert_eq!(v["projects_total"], 0);
    for flag in [
        "window_clamped",
        "projects_truncated",
        "sessions_scan_capped",
    ] {
        assert_eq!(v[flag], json!(false), "{flag}: {v}");
    }
    assert_eq!(v["trivial_sessions_hidden"], 0);
    assert!(v.get("degraded").is_none(), "{v}");
    assert!(v.get("slates_skipped").is_none(), "{v}");
}

#[tokio::test]
async fn changes_slate_post_lands_in_slate_group() {
    let (_tmp, addr, _paths) = boot().await;
    let client = reqwest::Client::new();
    let (status, _) = post(
        &client,
        addr,
        "orchard",
        body("warn", "the pump is flaky", Some("sess-a")),
    )
    .await;
    assert_eq!(status, 201);
    let (status, v) = changes(&client, addr, &since_query(3600)).await;
    assert_eq!(status, 200, "{v}");
    let g = &v["projects"][0];
    assert_eq!(g["project"], "orchard");
    assert_eq!(g["source"], "slate");
    let p = &g["slate"]["posts"][0];
    assert_eq!(p["kind"], "warn");
    assert_eq!(p["line"], "the pump is flaky");
    assert_eq!(p["who"], "claude/sess");
    assert_eq!(p["closed"], false);
    assert_eq!(g["slate"]["posts_total"], 1);
}

#[tokio::test]
async fn changes_slate_drop_is_retracted_not_listed() {
    let (_tmp, addr, _paths) = boot().await;
    let client = reqwest::Client::new();
    assert_eq!(
        post(
            &client,
            addr,
            "orchard",
            body("warn", "wrong alarm", Some("sess-a"))
        )
        .await
        .0,
        201
    );
    let mut drop = body("drop", "that warn was wrong", Some("sess-a"));
    drop["re"] = json!(1);
    let (status, v) = post(&client, addr, "orchard", drop).await;
    assert_eq!(status, 201, "{v}");
    let (_, v) = changes(&client, addr, &since_query(3600)).await;
    let s = &v["projects"][0]["slate"];
    assert_eq!(s["posts"], json!([]), "{v}");
    assert_eq!(s["retracted_total"], 1);
    assert_eq!(s["other_total"], 1, "the drop itself");
}

#[tokio::test]
async fn changes_slate_done_lists_done_and_marks_target_closed() {
    let (_tmp, addr, _paths) = boot().await;
    let client = reqwest::Client::new();
    assert_eq!(
        post(
            &client,
            addr,
            "orchard",
            body("warn", "disk is full", Some("sess-a"))
        )
        .await
        .0,
        201
    );
    let mut done = body("done", "freed the disk", Some("sess-a"));
    done["re"] = json!(1);
    let (status, v) = post(&client, addr, "orchard", done).await;
    assert_eq!(status, 201, "{v}");
    let (_, v) = changes(&client, addr, &since_query(3600)).await;
    let posts = v["projects"][0]["slate"]["posts"]
        .as_array()
        .unwrap()
        .clone();
    let kinds: Vec<(&str, bool)> = posts
        .iter()
        .map(|p| (p["kind"].as_str().unwrap(), p["closed"].as_bool().unwrap()))
        .collect();
    assert_eq!(kinds, [("done", false), ("warn", true)], "{v}");
}

#[tokio::test]
async fn changes_slate_rotate_flags_archived_in_window() {
    let (_tmp, addr, _paths) = boot().await;
    let client = reqwest::Client::new();
    let mut found = body("found", "a finding", Some("sess-a"));
    found["refs"] = json!(["path:crates/kb-server/src/routes/slates.rs"]);
    assert_eq!(post(&client, addr, "orchard", found).await.0, 201);
    let resp = client
        .post(url(addr, "/api/slates/orchard/rotate"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let (_, v) = changes(&client, addr, &since_query(3600)).await;
    let s = &v["projects"][0]["slate"];
    assert_eq!(s["archived_in_window"], true, "{v}");
    assert_eq!(s["posts"], json!([]), "archived posts are named, not read");
}

#[tokio::test]
async fn changes_project_filter_matches_label() {
    let (_tmp, addr, _paths) = boot().await;
    let client = reqwest::Client::new();
    for slug in ["orchard", "meadow"] {
        assert_eq!(
            post(
                &client,
                addr,
                slug,
                body("warn", "something", Some("sess-a"))
            )
            .await
            .0,
            201
        );
    }
    let (_, all) = changes(&client, addr, &since_query(3600)).await;
    assert_eq!(all["projects_total"], 2, "{all}");
    let (status, v) = changes(
        &client,
        addr,
        &format!("{}&project=meadow", since_query(3600)),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(v["project"], "meadow");
    assert_eq!(v["projects"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["projects"][0]["label"], "meadow");
    let (_, none) = changes(
        &client,
        addr,
        &format!("{}&project=nope", since_query(3600)),
    )
    .await;
    assert_eq!(none["projects"], json!([]));
}

#[tokio::test]
async fn changes_window_older_than_31_days_is_clamped() {
    let (_tmp, addr, _paths) = boot().await;
    let client = reqwest::Client::new();
    let (status, v) = changes(&client, addr, "?since=1").await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["window_clamped"], true);
    let span = v["to_unix"].as_i64().unwrap() - v["since_unix"].as_i64().unwrap();
    assert_eq!(span, 31 * 86_400);
}
