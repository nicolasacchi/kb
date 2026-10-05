//! Protocol-registry drift gate, kb-code side (v0.45 N8).
//!
//! `schemas/*.schema.json` are hand-written structural lints of kb-code's wire
//! types (docs/protocols.md). kb-cli's `schema_registry.rs` cannot link this
//! crate, so the kb-code contracts are checked HERE, from real values:
//!
//! * output types are serialized through the same serde path the routes use
//!   (`claim_out`, `DocFinding`, `render_findings_json`, `assemble`) or fetched
//!   from the route handler itself (the cmd/theme registries) and validated
//!   against the schema file, read at test time;
//! * input types (`FindingsImportBody`, `ClaimBody`) are compared two ways with
//!   their schema: the keys serde REQUIRES equal the schema's `required`, and
//!   every key serde accepts is a declared property;
//! * the json!-built route bodies (`kbc-github-export/1`, `unified-inbox/1`,
//!   the review-context route envelope, the identity Hello) are validated
//!   inside the route tests that fetch them (`tests/review/*`,
//!   `tests/boot_e2e/boot.rs`), NOT in this file. The one review-context check
//!   here (`review_context_bundle_from_the_real_assembler_conforms`) runs the
//!   real `assemble` output under a hand-stamped envelope, so the route's own
//!   envelope is covered only by its route test.
//!
//! A field renamed, or added as required, in Rust without the schema following
//! fails a named test below.

#[allow(unused)]
mod common;

use axum::response::IntoResponse;
use common::{assert_conforms, assert_keys_declared, assert_serde_matches_schema, conformance};
use kb_code_server::claims::{claim_out, ClaimBody};
use kb_code_server::review_context::{assemble, Inputs};
use kb_code_server::review_doc::DocFinding;
use kb_code_server::review_findings::{
    FindingEvidenceBody, FindingLocationBody, FindingsImportBody,
};
use kb_code_server::store::ClaimRow;
use serde_json::{json, Value};

async fn body_json(resp: axum::response::Response) -> Value {
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("body");
    assert!(status.is_success(), "{status}");
    serde_json::from_slice(&bytes).expect("the route serves JSON")
}

// --- kbc-cmd/1 + kbc-theme/1: served verbatim by the routes -------------------

#[tokio::test]
async fn kbc_cmd_registry_route_body_conforms() {
    let resp = kb_code_server::routes::commands(axum::http::HeaderMap::new()).await;
    let v = body_json(resp).await;
    assert_eq!(v["schema"], "kbc-cmd/1");
    assert_conforms("kbc-cmd/1", &v);
}

#[tokio::test]
async fn kbc_theme_registry_route_body_conforms() {
    let resp = kb_code_server::routes::themes().await.into_response();
    let v = body_json(resp).await;
    assert_eq!(v["schema"], "kbc-theme/1");
    assert_conforms("kbc-theme/1", &v);
}

// --- kbc-findings/1 (import payload) and /2 (sidecar) --------------------------

fn full_location() -> Value {
    json!({"path": "src/retry.rs", "kind": "range", "lines": [10, 12], "removed": false})
}

#[test]
fn findings_v1_import_payload_serde_and_schema_agree() {
    let item = json!({
        "slug": "f-retry-budget",
        "severity": "concern",
        "category": "correctness",
        "location": full_location(),
        "title": "Retry budget is per process",
        "rationale": "Two workers double the budget.",
        "recommendation": "Move the counter to the store.",
        "evidence": {"lang": "rust", "source": "let budget = 3;"}
    });
    let batch = json!({
        "schema": "kbc-findings/1",
        "mode": "additive",
        "ps_number": 1,
        "import_note": "first pass",
        "author": "claude",
        "findings": [item]
    });
    assert_conforms("kbc-findings/1", &batch);
    assert_serde_matches_schema::<FindingsImportBody>("kbc-findings/1", "", &batch);
    assert_serde_matches_schema::<kb_code_server::review_findings::FindingImportItem>(
        "kbc-findings/1",
        "/properties/findings/items",
        &item,
    );
    assert_serde_matches_schema::<FindingLocationBody>(
        "kbc-findings/1",
        "/$defs/location",
        &full_location(),
    );
}

#[test]
fn findings_v2_sidecar_from_real_serialization_conforms() {
    let finding = DocFinding {
        slug: Some("f-retry-budget".into()),
        act: "issue".into(),
        severity: "concern".into(),
        category: "correctness".into(),
        blocking: true,
        title: "Retry budget is per process".into(),
        rationale: "Two workers double the budget.".into(),
        recommendation: Some("Move the counter to the store.".into()),
        location: FindingLocationBody {
            path: "src/retry.rs".into(),
            kind: "range".into(),
            lines: Some(vec![10, 12]),
            removed: false,
        },
        cites: vec!["code:src/retry.rs:10".into()],
        supersedes: vec!["f-old".into()],
        evidence: Some(FindingEvidenceBody {
            lang: Some("rust".into()),
            source: Some("let budget = 3;".into()),
        }),
    };
    let item = serde_json::to_value(&finding).unwrap();
    // Everything the type emits is declared; what serde requires is what the
    // schema requires.
    assert_keys_declared("kbc-findings/2", "/properties/findings/items", &item);
    assert_serde_matches_schema::<DocFinding>(
        "kbc-findings/2",
        "/properties/findings/items",
        &item,
    );
    assert_serde_matches_schema::<FindingLocationBody>(
        "kbc-findings/2",
        "/$defs/location",
        &item["location"],
    );
    let sidecar = json!({"schema": "kbc-findings/2", "findings": [item]});
    assert_conforms("kbc-findings/2", &sidecar);

    // The pseudo-file render (`~review/findings.json`) is the same contract.
    let rendered = kb_code_server::review_pseudo::render_findings_json(7, 2, &[]);
    let v: Value = serde_json::from_str(rendered.content()).unwrap();
    assert_conforms("kbc-findings/2", &v);
    assert_keys_declared("kbc-findings/2", "", &v);
}

// --- kbc-claim/1 ----------------------------------------------------------------

#[test]
fn claim_write_body_and_answer_conform() {
    let full = json!({
        "schema": "kbc-claim/1",
        "repo": "demo",
        "subject_kind": "path",
        "subject": "order.rb",
        "kind": "explain",
        "body_md": "cents everywhere",
        "confidence": 0.7,
        "evidence": ["code:order.rb:3"],
        "session_id": "sess-1",
        "model": "claude",
        "blob_sha": "0000000000000000000000000000000000000000",
        "review_id": 1
    });
    assert_conforms("kbc-claim/1", &full);
    assert_serde_matches_schema::<ClaimBody>("kbc-claim/1", "", &full);

    // The answer: the real `claim_out`, serialized as the routes do.
    let row = ClaimRow {
        id: "clm_0123456789ab".into(),
        repo_id: 1,
        subject_kind: "path".into(),
        subject: "order.rb".into(),
        subject_path: Some("order.rb".into()),
        review_id: Some(1),
        kind: "explain".into(),
        body_md: "cents everywhere".into(),
        confidence: Some(0.7),
        evidence_json: r#"["code:order.rb:3"]"#.into(),
        session_id: Some("sess-1".into()),
        model: Some("claude".into()),
        blob_sha: Some("0000000000000000000000000000000000000000".into()),
        created_at: 1_767_214_800,
    };
    let out = serde_json::to_value(claim_out(&row, "demo", Some("1111"))).unwrap();
    assert_conforms("kbc-claim/1", &out);
    assert_keys_declared("kbc-claim/1", "", &out);
    // Anchorless claim: optional keys vanish, the answer still conforms.
    let bare = ClaimRow {
        subject_path: None,
        review_id: None,
        confidence: None,
        session_id: None,
        model: None,
        blob_sha: None,
        evidence_json: "[]".into(),
        ..row
    };
    let out = serde_json::to_value(claim_out(&bare, "demo", None)).unwrap();
    assert_conforms("kbc-claim/1", &out);
    // A value outside the closed vocabulary is not a claim.
    let mut bad = full.clone();
    bad["kind"] = json!("vibes");
    assert!(conformance("kbc-claim/1", &bad).is_err());
}

// --- kbc-review-context/1 ---------------------------------------------------------

#[test]
fn review_context_bundle_from_the_real_assembler_conforms() {
    let inputs = Inputs {
        header: json!({"title": "t"}),
        threads: vec![json!({"id": "a"})],
        findings: vec![json!({"slug": "f-1", "severity": "blocker"})],
        other_reviews: vec![],
        since: Some(Err("no-verdict".into())),
        reading_order: vec![json!({"path": "x.rs"})],
        files: vec![json!({"path": "x.rs"})],
        patches: vec![("x.rs".into(), "diff --git a/x.rs b/x.rs\n+x\n".into())],
        redacted: vec![json!({"path": ".env", "pattern": ".env"})],
    };
    let mut bundle = assemble(inputs, 1000);
    // The route stamps the envelope after `assemble` (review_context.rs).
    bundle["schema"] = json!(kb_code_server::review_context::CONTEXT_SCHEMA);
    bundle["review_id"] = json!(1);
    bundle["repo"] = json!("demo");
    bundle["ps"] = json!(1);
    assert_conforms("kbc-review-context/1", &bundle);
    assert_keys_declared("kbc-review-context/1", "", &bundle);
}
