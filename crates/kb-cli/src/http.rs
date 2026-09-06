//! Tiny HTTP client helpers — auto-detect daemon, build search URLs.

use anyhow::{Context, Result};
use std::time::Duration;

const DEFAULT_DAEMON: &str = "http://127.0.0.1:4000";

/// Try the configured (or default) daemon's `/api/identity` endpoint with a
/// short timeout. Returns the daemon URL if reachable, else None.
pub async fn detect_daemon(explicit: Option<&str>, bearer: Option<&str>) -> Option<String> {
    let candidate = explicit.unwrap_or(DEFAULT_DAEMON).to_string();
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
        .ok()?;
    let url = format!("{candidate}/api/identity");
    // Send the bearer: against an auth-on daemon reached over a non-loopback
    // hop (e.g. a published Docker port), an unauthenticated probe gets 401
    // and the caller would wrongly conclude the daemon is down.
    let mut req = client.get(&url);
    if let Some(token) = bearer {
        req = req.bearer_auth(token);
    }
    match req.send().await {
        Ok(r) if r.status().is_success() => Some(candidate),
        _ => None,
    }
}

pub fn client_with_timeout(secs: u64) -> Result<reqwest::Client> {
    client_with_timeout_and_bearer(secs, None)
}

/// Build a reqwest client with timeout + optional bearer auth header.
/// Centralised so every CLI verb that talks to the daemon authenticates
/// the same way; deep-review K6 surfaced that `search`, `status`, `atlas`,
/// and `comments` verbs were all sending bareheaded requests, breaking
/// `kb` against an auth-on remote daemon. Self-host docs claimed they
/// worked.
pub fn client_with_timeout_and_bearer(secs: u64, bearer: Option<&str>) -> Result<reqwest::Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    // M6: every kb-cli request advertises itself as a first-party
    // client so the daemon's Origin-allowlist middleware lets the
    // Origin-less POST through. Cross-site browser forms can't spoof
    // this header.
    headers.insert(
        "X-Requested-By",
        reqwest::header::HeaderValue::from_static("kb-cli"),
    );
    if let Some(token) = bearer {
        let value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
            .context("bearer token contained an invalid header byte")?;
        headers.insert(reqwest::header::AUTHORIZATION, value);
    }
    // MI test-hardening (2026-08) — every call site above hardcodes its own
    // timeout (5-600s), sized for a healthy daemon on an unloaded machine.
    // Under host I/O contention a daemon can be merely slow (e.g. its
    // single-writer storage actor queue backed up behind other tests'
    // indexing) rather than actually stuck, and a mutating integration-test
    // call like `kb notes new` can trip its call site's timeout before the
    // daemon ever gets to answer — `notes_cli_links_and_backlinks` did
    // exactly this (`error sending request … operation timed out`, not the
    // poll-loop timeout `KB_TEST_INDEX_TIMEOUT_SECS` already covers). There's
    // no knob for this that a test can already reach (each `secs` is a
    // literal at its call site), so this is a narrowly-scoped test-only
    // override: honoured ONLY when a test explicitly sets
    // `KB_TEST_HTTP_TIMEOUT_SECS` on the spawned `kb` subprocess's own env
    // (see `kb-cli/tests/common/mod.rs::http_timeout_secs`) — unset in every
    // real invocation, so production's snappy-failure defaults are
    // untouched.
    let secs = std::env::var("KB_TEST_HTTP_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(secs);
    reqwest::Client::builder()
        .timeout(Duration::from_secs(secs))
        .default_headers(headers)
        .build()
        .context("reqwest client")
}

/// Exchange OAuth2 `client_credentials` for an access token (RFC 6749
/// §4.4). `kb pull` uses this to authenticate to a kb instance fronted
/// by Authelia's OIDC bearer-authz gate: the returned `authelia_at_…`
/// access token is presented as `Authorization: Bearer …`, which
/// Authelia's forward-auth validates before Traefik rewrites the header
/// with the daemon's own token. Authenticates via `client_secret_basic`
/// (HTTP Basic) and requests `scope`.
///
/// `audience` is required: Authelia's bearer-authz denies a token whose
/// audience doesn't prefix-match the requested URL, and for the
/// `client_credentials` grant the audience is only granted when
/// explicitly requested. Pass the kb origin (e.g. `https://kb.example.com`).
pub async fn fetch_oauth_token(
    token_url: &str,
    client_id: &str,
    client_secret: &str,
    scope: &str,
    audience: &str,
) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("reqwest client (oauth)")?;
    let resp = client
        .post(token_url)
        .basic_auth(client_id, Some(client_secret))
        .form(&[
            ("grant_type", "client_credentials"),
            ("scope", scope),
            ("audience", audience),
        ])
        .send()
        .await
        .with_context(|| format!("POST {token_url}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("token endpoint {token_url}: HTTP {status} — {body}");
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .with_context(|| format!("parse token JSON from {token_url}"))?;
    body.get("access_token")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("token endpoint {token_url}: response had no access_token"))
}

/// L1 — `GET /api/kb/{kb}/lookup?q=<input>`. Returns the raw JSON
/// response so callers (`kb find`, the comments `--path` resolver)
/// can branch on the `kind` field without sharing a Rust type with
/// the server.
///
/// Errors surface transport/HTTP failures; a `200 not_found` is NOT
/// an error here — the caller inspects the JSON to decide.
pub async fn get_lookup(
    daemon: Option<&str>,
    kb: &str,
    q: &str,
    bearer: Option<&str>,
) -> Result<serde_json::Value> {
    let base = daemon.unwrap_or(DEFAULT_DAEMON).trim_end_matches('/');
    let url = format!("{base}/api/kb/{}/lookup", encode_path_segment(kb),);
    let client = client_with_timeout_and_bearer(5, bearer)?;
    let resp = client
        .get(&url)
        .query(&[("q", q)])
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("lookup {url}: HTTP {status} — {body}");
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .with_context(|| format!("parse JSON from {url}"))?;
    Ok(body)
}

/// L1 — resolve `--kb` to a concrete name. When `--kb` is supplied
/// returns it verbatim; otherwise hits `/api/kbs` and uses the only
/// configured kb (errors if 0 or 2+).
pub async fn resolve_default_kb(
    explicit: Option<&str>,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<String> {
    if let Some(k) = explicit {
        return Ok(k.to_string());
    }
    let base = daemon.unwrap_or(DEFAULT_DAEMON).trim_end_matches('/');
    let url = format!("{base}/api/kbs");
    let client = client_with_timeout_and_bearer(5, bearer)?;
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let body: serde_json::Value = resp
        .error_for_status()?
        .json()
        .await
        .with_context(|| format!("parse JSON from {url}"))?;
    let kbs = body
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("GET {url}: expected an array"))?;
    match kbs.len() {
        0 => anyhow::bail!("no kbs configured"),
        1 => Ok(kbs[0]["name"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("kb without `name`"))?
            .to_string()),
        n => anyhow::bail!("{n} kbs configured — pass --kb to pick one"),
    }
}

/// GC-B5 — record an agent-driven read (`kb cat`/`kb get`/`kb read`) into the same
/// `history` "open" row the SPA writes on artifact view (`POST
/// /api/kb/{kb}/history/open`), tagged `source: "cli"` so it's
/// distinguishable from a human scroll session (invariant #19: touched !=
/// read — a CLI dump is neither; it's an explicit pull, recorded via the
/// existing open-visit mechanism per the roadmap ask). `base` is an
/// already-detected reachable daemon URL (from `detect_daemon`), not
/// re-resolved here.
pub async fn record_open_cli(base: &str, kb: &str, id: &str, bearer: Option<&str>) -> Result<()> {
    let url = format!(
        "{}/api/kb/{}/history/open",
        base.trim_end_matches('/'),
        encode_path_segment(kb),
    );
    let client = client_with_timeout_and_bearer(5, bearer)?;
    let resp = client
        .post(&url)
        .json(&serde_json::json!({ "artifact_id": id, "source": "cli" }))
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("POST {url}: HTTP {status} — {body}");
    }
    Ok(())
}

/// True if `kb` contains a doc with this exact id. Probes
/// `GET /api/kb/{kb}/docs/{id}`: 200 → true, 404 → false, other → Err.
/// Doc-space fallback for the comments verbs' kb auto-resolve, used when
/// an id has no review file yet (e.g. `kb comments add`, or an id copied
/// from `kb search`). Comment-bearing ids resolve in review-space first.
pub async fn kb_has_doc(
    daemon: Option<&str>,
    kb: &str,
    id: &str,
    bearer: Option<&str>,
) -> Result<bool> {
    let base = daemon.unwrap_or(DEFAULT_DAEMON).trim_end_matches('/');
    let url = format!(
        "{base}/api/kb/{}/docs/{}",
        encode_path_segment(kb),
        encode_path_segment(id),
    );
    let client = client_with_timeout_and_bearer(5, bearer)?;
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    match resp.status().as_u16() {
        200 => Ok(true),
        404 => Ok(false),
        s => anyhow::bail!("GET {url}: HTTP {s}"),
    }
}

/// Percent-encode a URL path segment. CLI verbs build URLs by
/// interpolating user-supplied `kb` / `artifact_id` / `src` into
/// `format!("{base}/api/kb/{kb}/...")`. A kb name or id containing `/`,
/// `#`, `?`, or non-ASCII characters used to produce a malformed URL —
/// deep-review K8. This helper escapes the standard "non-segment"
/// charset (RFC 3986 unreserved + sub-delims minus the path delimiters).
pub fn encode_path_segment(seg: &str) -> String {
    // percent-encode anything outside the unreserved set + a small
    // safe subset of sub-delims that segments accept (`-._~`). Stays
    // conservative: anything ambiguous gets encoded.
    let mut out = String::with_capacity(seg.len());
    for b in seg.as_bytes() {
        let c = *b;
        if c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.' | b'_' | b'~') {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{c:02X}"));
        }
    }
    out
}

/// Resolve `(kb?, target)` to `(kb_name, artifact_id)` via the daemon's
/// `/lookup`. `target` is either a 12-hex id or a source-relative
/// path / unique filename. Single-positional shape shared by the verbs
/// that take exactly one artifact (`kb list add`, formerly
/// `kb bookmark`); `commands::comments` keeps its id+--path pair.
pub async fn resolve_artifact_target(
    kb: Option<&str>,
    target: &str,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<(String, String)> {
    let resolved_kb = resolve_default_kb(kb, daemon, bearer).await?;
    // Heuristic: 12 hex chars with no '/' or '.' is an artifact id.
    let looks_like_id = target.len() == 12 && target.chars().all(|c| c.is_ascii_hexdigit());
    if looks_like_id {
        return Ok((resolved_kb, target.to_string()));
    }
    let body = get_lookup(daemon, &resolved_kb, target, bearer).await?;
    let kind = body
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    match kind {
        "exact" | "unique_suffix" => {
            let id = body
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("lookup response missing `id`"))?
                .to_string();
            Ok((resolved_kb, id))
        }
        "ambiguous" => {
            let candidates = body
                .get("candidates")
                .and_then(|v| v.as_array())
                .map(|a| a.as_slice())
                .unwrap_or(&[]);
            let mut msg = format!(
                "{target:?} matched {} artifacts in {resolved_kb}; pick one:\n",
                candidates.len()
            );
            for c in candidates {
                let id = c.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                let rel = c
                    .get("source_relative")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                msg.push_str(&format!("  {id}  {rel}\n"));
            }
            anyhow::bail!(msg)
        }
        "not_found" => anyhow::bail!("{target:?} matched no artifact in {resolved_kb}"),
        other => anyhow::bail!("lookup returned unknown kind {other:?}: {body}"),
    }
}

/// Send a built request, mapping non-2xx to a problem+json-aware error
/// (`detail` surfaced when present). Returns the parsed JSON body
/// (`Null` for empty 204-style responses).
pub async fn send_json(req: reqwest::RequestBuilder, what: &str) -> Result<serde_json::Value> {
    let resp = req.send().await.with_context(|| what.to_string())?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        let detail = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("detail").and_then(|d| d.as_str()).map(str::to_string))
            .unwrap_or(text);
        anyhow::bail!("{what} failed: HTTP {status} — {detail}");
    }
    Ok(serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_path_segment_passes_unreserved() {
        assert_eq!(encode_path_segment("abc123-_.~"), "abc123-_.~");
    }

    #[test]
    fn encode_path_segment_escapes_slash_hash_question() {
        assert_eq!(encode_path_segment("a/b"), "a%2Fb");
        assert_eq!(encode_path_segment("a#b"), "a%23b");
        assert_eq!(encode_path_segment("a?b"), "a%3Fb");
        assert_eq!(encode_path_segment("a b"), "a%20b");
    }

    #[test]
    fn encode_path_segment_handles_utf8() {
        // 漢 = 0xE6 0xBC 0xA2 → each byte percent-encoded.
        assert_eq!(encode_path_segment("漢"), "%E6%BC%A2");
    }

    #[test]
    fn client_builder_accepts_bearer() {
        let c = client_with_timeout_and_bearer(5, Some("secret"));
        assert!(c.is_ok());
    }

    #[test]
    fn client_builder_rejects_bearer_with_control_bytes() {
        let c = client_with_timeout_and_bearer(5, Some("bad\ntoken"));
        assert!(c.is_err());
    }
}
