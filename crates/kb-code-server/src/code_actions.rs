//! `POST /api/code-actions` — S2-C (design-s2.md § S2-C + its Addendum).
//! LSP code actions (quick fixes) surfaced from a configured lip/1
//! provider, translated into the PINNED `code-actions/1` response shape
//! other Stage-2 units build against verbatim (the Addendum: "B1 emits, B4
//! consumes — EXACT"). Same honest-degrade posture `crate::lip::
//! diagnostics_route` already established: `available:false` + a CLOSED
//! `reason` vocabulary rather than an empty list standing in for "nothing
//! configured" — `"unknown_language"` (no detected language) ·
//! `"no_provider_configured"` (no `[[intel.providers]]` entry names this
//! `(repo, lang)`) · `"file_unreadable"` · `"provider_unavailable"`
//! (refused/timeout/unreachable/handshake-unusable) · `"blob_stale"` (the
//! CRITICAL blob-freshness re-check failed) · `"capability_absent"` — NEW
//! here: the provider IS configured and reachable, it just predates lip's
//! `"code-actions"` capability (`crate::lip::CodeActionsFailure`'s doc has
//! the exact split).
//!
//! NOTHING is persisted here — computed fresh, per request, same posture
//! every other lip overlay uses (`crate::lip`'s module doc, "Never
//! persisted; async-overlay architecture"). Converting an action into a
//! durable suggestion is a SEPARATE, CLIENT-driven step over the EXISTING
//! `POST /api/annotations/batch` `add_comment`(+suggestion) op
//! (`crate::routes::AnnotationBatchOp::AddComment`) — this route has no
//! mutation path of its own (design-s2.md § S2-C: "No new mutation
//! route").
//!
//! Ordinary `auth_bearer`-gated browsing-class read (`router.rs`) — a
//! caller-chosen RANGE instead of a caller-chosen position, same
//! sensitivity class as `/api/diagnostics`/`/api/hover`.

use crate::routes::{find_repo, read_repo_file, ApiError};
use crate::state::SharedState;
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CODE_ACTIONS_SCHEMA: &str = "code-actions/1";

/// `POST /api/code-actions` body (design-s2.md § S2-C's kb-code-server
/// pipeline). `end_line`/`end_col` default to `start_line`/`start_col`
/// server-side (a point range) when omitted; `kinds` is an optional LSP
/// `CodeActionKind` allowlist (`CodeActionContext.only`).
#[derive(Debug, Deserialize)]
pub struct CodeActionsBody {
    pub repo: String,
    pub path: String,
    pub start_line: u32,
    pub start_col: u32,
    #[serde(default)]
    pub end_line: Option<u32>,
    #[serde(default)]
    pub end_col: Option<u32>,
    #[serde(default)]
    pub kinds: Option<Vec<String>>,
}

/// One `TextEdit` — byte-col wire (1-based lines, 0-based byte columns),
/// the same codec every lip/1 endpoint uses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeActionEdit {
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
    pub new_text: String,
}

/// One file's worth of edits within a single code action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeActionFileEdit {
    pub path: String,
    pub edits: Vec<CodeActionEdit>,
}

/// One LSP code action, translated from a `WorkspaceEdit`'s `changes`/
/// `documentChanges` (`TextDocumentEdit` entries only — kb-lip drops
/// `Create`/`Rename`/`DeleteFile` and command-only actions into its own
/// `dropped_*` counters; see `crate::lip::LipCodeActionsAnswer`'s doc).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeAction {
    pub title: String,
    /// LSP `CodeActionKind` — empty string when the provider omitted it
    /// (optional per the LSP spec), never dropped for that alone.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub is_preferred: bool,
    pub edits: Vec<CodeActionFileEdit>,
}

/// `dropped_command_only`/`dropped_unsupported` relayed VERBATIM from the
/// lip/1 response — kb-lip's own counters. [`parse_code_actions`]'s own
/// defensive filtering is a SEPARATE pass over already-translated actions
/// and must never feed back into these (see that fn's doc).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct DroppedCounts {
    pub command_only: u32,
    pub unsupported: u32,
}

/// `POST /api/code-actions` response — the Addendum's PINNED
/// `code-actions/1` shape. `available:false` always carries
/// `actions: []` + `dropped: {0,0}` (never a partial/omitted body) so a
/// consumer can destructure this one shape unconditionally.
#[derive(Debug, Serialize)]
pub struct CodeActionsOut {
    pub schema: &'static str,
    pub available: bool,
    pub verified: bool,
    pub reason: Option<&'static str>,
    pub provider: Option<String>,
    pub actions: Vec<CodeAction>,
    pub dropped: DroppedCounts,
}

impl CodeActionsOut {
    fn unavailable(reason: &'static str) -> Self {
        Self {
            schema: CODE_ACTIONS_SCHEMA,
            available: false,
            verified: false,
            reason: Some(reason),
            provider: None,
            actions: Vec::new(),
            dropped: DroppedCounts::default(),
        }
    }
}

/// `POST /api/code-actions` — see the module doc.
pub async fn code_actions_route(
    State(state): State<SharedState>,
    Json(body): Json<CodeActionsBody>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, _repo_id) = find_repo(&state, &body.repo)?;
    let out = code_actions_for(&state, repo, &body).await;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(out)))
}

async fn code_actions_for(
    state: &SharedState,
    repo: &crate::config::RepoEntry,
    body: &CodeActionsBody,
) -> CodeActionsOut {
    let Some(lang) = crate::lang::detect(&body.path, None) else {
        return CodeActionsOut::unavailable("unknown_language");
    };
    let Some(client) = state.lip.client_for(&repo.name, lang.id) else {
        return CodeActionsOut::unavailable("no_provider_configured");
    };
    let provider_name = client.name().to_string();
    let Ok(read) = read_repo_file(repo, &body.path, None) else {
        return CodeActionsOut {
            provider: Some(provider_name),
            ..CodeActionsOut::unavailable("file_unreadable")
        };
    };
    let answer = client
        .code_actions(
            &body.path,
            &read.blob_hash,
            body.start_line,
            body.start_col,
            body.end_line,
            body.end_col,
            body.kinds.as_deref(),
        )
        .await;
    let answer = match answer {
        Ok(a) => a,
        Err(crate::lip::CodeActionsFailure::CapabilityAbsent) => {
            return CodeActionsOut {
                provider: Some(provider_name),
                ..CodeActionsOut::unavailable("capability_absent")
            };
        }
        Err(crate::lip::CodeActionsFailure::Unavailable) => {
            return CodeActionsOut {
                provider: Some(provider_name),
                ..CodeActionsOut::unavailable("provider_unavailable")
            };
        }
    };
    // The CRITICAL trust rule — see `crate::lip`'s module doc. A mismatch
    // DISCARDS the whole response; never a downgrade.
    if !crate::lip::verify_blob_freshness_sha(repo, &body.path, &answer.verified_blob_sha) {
        return CodeActionsOut {
            provider: Some(provider_name),
            ..CodeActionsOut::unavailable("blob_stale")
        };
    }
    let actions = parse_code_actions(&answer.results);
    CodeActionsOut {
        schema: CODE_ACTIONS_SCHEMA,
        available: true,
        verified: true,
        reason: None,
        provider: Some(provider_name),
        actions,
        dropped: DroppedCounts {
            command_only: answer.dropped_command_only,
            unsupported: answer.dropped_unsupported,
        },
    }
}

// --- defensive parsing of kb-lip's translated code actions -----------------

fn parse_edit(v: &Value) -> Option<CodeActionEdit> {
    Some(CodeActionEdit {
        start_line: v.get("start_line")?.as_u64()? as u32,
        start_col: v.get("start_col")?.as_u64()? as u32,
        end_line: v.get("end_line")?.as_u64()? as u32,
        end_col: v.get("end_col")?.as_u64()? as u32,
        new_text: v.get("new_text")?.as_str()?.to_string(),
    })
}

fn parse_file_edit(v: &Value) -> Option<CodeActionFileEdit> {
    let path = v.get("path")?.as_str()?.to_string();
    let edits: Vec<CodeActionEdit> = v
        .get("edits")?
        .as_array()?
        .iter()
        .filter_map(parse_edit)
        .collect();
    if edits.is_empty() {
        return None;
    }
    Some(CodeActionFileEdit { path, edits })
}

/// Defensively parses kb-lip's `translate_code_actions`-shaped `results`
/// array (design-s2.md § S2-C's Response block): `[{title, kind,
/// is_preferred, edits: [{path, edits: [...]}]}]`. An entry missing
/// `title`, missing `edits` entirely, or left with ZERO usable file edits
/// after filtering (every nested edit malformed) is dropped WHOLESALE — an
/// upstream quirk must degrade this ONE action, never the whole response
/// (the same `filter_map`-over-panic posture `crate::lip::parse_locations`/
/// `parse_diagnostics` already use). This is a SEPARATE pass from kb-lip's
/// own `dropped_command_only`/`dropped_unsupported` counters
/// (`crate::lip::LipCodeActionsAnswer`) — those are relayed verbatim by
/// `code_actions_for`, never recomputed from what this fn drops.
pub(crate) fn parse_code_actions(results: &Value) -> Vec<CodeAction> {
    results
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let title = a.get("title")?.as_str()?.to_string();
            let kind = a
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let is_preferred = a
                .get("is_preferred")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let raw_edits = a.get("edits")?.as_array()?;
            let edits: Vec<CodeActionFileEdit> =
                raw_edits.iter().filter_map(parse_file_edit).collect();
            if edits.is_empty() {
                return None;
            }
            Some(CodeAction {
                title,
                kind,
                is_preferred,
                edits,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{IntelProviderEntry, IntelSection, RepoEntry};
    use crate::join::kb_client::test_support::mock_kb_server;
    use axum::routing::{get, post};
    use axum::{Json as AxumJson, Router};
    use serde_json::json;

    fn write_file(root: &std::path::Path, path: &str, content: &str) {
        let abs = root.join(path);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(abs, content).unwrap();
    }

    fn fixture_repo(name: &str) -> (tempfile::TempDir, RepoEntry) {
        let root = tempfile::tempdir().unwrap();
        let entry = RepoEntry {
            name: name.to_string(),
            path: root.path().to_path_buf(),
        };
        (root, entry)
    }

    fn provider_entry(name: &str, url: String) -> IntelProviderEntry {
        IntelProviderEntry {
            name: name.to_string(),
            url,
            langs: vec!["rust".to_string()],
            repos: vec!["fixture".to_string()],
        }
    }

    /// Same fixture convention `crate::lip`'s own test module documents —
    /// boots a real `SharedState` via `crate::build_state_for_test`.
    async fn test_state(
        providers: Vec<IntelProviderEntry>,
        repo_root: &std::path::Path,
        repo_name: &str,
    ) -> SharedState {
        let cfg = crate::config::KbCodeConfig {
            repos: vec![RepoEntry {
                name: repo_name.to_string(),
                path: repo_root.to_path_buf(),
            }],
            kb_daemon: crate::config::KbDaemonSection {
                enabled: false,
                url: "http://127.0.0.1:0".to_string(),
                token_file: None,
                public_url: None,
            },
            intel: IntelSection { providers },
            ..crate::config::KbCodeConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let paths = kb_core::paths::KbPaths::rooted_at(tmp.path(), "kb-code");
        crate::build_state_for_test(cfg, paths).await.unwrap()
    }

    // --- code_actions_for: honest-degrade ladder ----------------------

    #[tokio::test]
    async fn reports_unknown_language_when_no_lang_detected() {
        let (root, repo) = fixture_repo("fixture");
        write_file(root.path(), "README", "hello\n");
        let state = test_state(vec![], root.path(), "fixture").await;
        let body = CodeActionsBody {
            repo: "fixture".to_string(),
            path: "README".to_string(),
            start_line: 1,
            start_col: 0,
            end_line: None,
            end_col: None,
            kinds: None,
        };
        let out = code_actions_for(&state, &repo, &body).await;
        assert!(!out.available);
        assert_eq!(out.reason, Some("unknown_language"));
        assert!(out.provider.is_none());
        assert!(out.actions.is_empty());
    }

    #[tokio::test]
    async fn reports_no_provider_configured() {
        let (root, repo) = fixture_repo("fixture");
        write_file(root.path(), "a.rs", "fn main() {}\n");
        let state = test_state(vec![], root.path(), "fixture").await;
        let body = CodeActionsBody {
            repo: "fixture".to_string(),
            path: "a.rs".to_string(),
            start_line: 1,
            start_col: 0,
            end_line: None,
            end_col: None,
            kinds: None,
        };
        let out = code_actions_for(&state, &repo, &body).await;
        assert!(!out.available);
        assert_eq!(out.reason, Some("no_provider_configured"));
    }

    #[tokio::test]
    async fn reports_file_unreadable_but_still_names_the_provider() {
        let (root, repo) = fixture_repo("fixture");
        // Never write "a.rs" — the read must fail.
        let state = test_state(
            vec![provider_entry(
                "rust-analyzer",
                "http://127.0.0.1:0".to_string(),
            )],
            root.path(),
            "fixture",
        )
        .await;
        let body = CodeActionsBody {
            repo: "fixture".to_string(),
            path: "a.rs".to_string(),
            start_line: 1,
            start_col: 0,
            end_line: None,
            end_col: None,
            kinds: None,
        };
        let out = code_actions_for(&state, &repo, &body).await;
        assert!(!out.available);
        assert_eq!(out.reason, Some("file_unreadable"));
        assert_eq!(out.provider.as_deref(), Some("rust-analyzer"));
    }

    #[tokio::test]
    async fn reports_capability_absent_when_the_provider_predates_it() {
        let (root, repo) = fixture_repo("fixture");
        write_file(root.path(), "a.rs", "fn main() {}\n");
        let router = Router::new().route(
            "/lip/identity",
            get(|| async {
                AxumJson(json!({"protocol": "lip/1", "lip_major": 1, "capabilities": ["hover"]}))
            }),
        );
        let (addr, _server) = mock_kb_server(router).await;
        let state = test_state(
            vec![provider_entry("rust-analyzer", format!("http://{addr}"))],
            root.path(),
            "fixture",
        )
        .await;
        let body = CodeActionsBody {
            repo: "fixture".to_string(),
            path: "a.rs".to_string(),
            start_line: 1,
            start_col: 0,
            end_line: None,
            end_col: None,
            kinds: None,
        };
        let out = code_actions_for(&state, &repo, &body).await;
        assert!(!out.available);
        assert_eq!(out.reason, Some("capability_absent"));
        assert_eq!(out.provider.as_deref(), Some("rust-analyzer"));
    }

    #[tokio::test]
    async fn reports_provider_unavailable_on_refusal() {
        let (root, repo) = fixture_repo("fixture");
        write_file(root.path(), "a.rs", "fn main() {}\n");
        let router = Router::new()
            .route(
                "/lip/identity",
                get(|| async {
                    AxumJson(
                        json!({"protocol": "lip/1", "lip_major": 1, "capabilities": ["code-actions"]}),
                    )
                }),
            )
            .route(
                "/lip/code-actions",
                post(|| async { AxumJson(json!({"refused": "server_down"})) }),
            );
        let (addr, _server) = mock_kb_server(router).await;
        let state = test_state(
            vec![provider_entry("rust-analyzer", format!("http://{addr}"))],
            root.path(),
            "fixture",
        )
        .await;
        let body = CodeActionsBody {
            repo: "fixture".to_string(),
            path: "a.rs".to_string(),
            start_line: 1,
            start_col: 0,
            end_line: None,
            end_col: None,
            kinds: None,
        };
        let out = code_actions_for(&state, &repo, &body).await;
        assert!(!out.available);
        assert_eq!(out.reason, Some("provider_unavailable"));
    }

    #[tokio::test]
    async fn reports_blob_stale_and_discards_rather_than_downgrades() {
        let (root, repo) = fixture_repo("fixture");
        write_file(root.path(), "a.rs", "fn main() {}\n");
        let router = Router::new()
            .route(
                "/lip/identity",
                get(|| async {
                    AxumJson(
                        json!({"protocol": "lip/1", "lip_major": 1, "capabilities": ["code-actions"]}),
                    )
                }),
            )
            .route(
                "/lip/code-actions",
                post(|| async {
                    AxumJson(json!({
                        "verified_blob_sha": "not-the-real-hash",
                        "results": [{"title": "Add import", "kind": "quickfix",
                            "is_preferred": false, "edits": []}],
                    }))
                }),
            );
        let (addr, _server) = mock_kb_server(router).await;
        let state = test_state(
            vec![provider_entry("rust-analyzer", format!("http://{addr}"))],
            root.path(),
            "fixture",
        )
        .await;
        let body = CodeActionsBody {
            repo: "fixture".to_string(),
            path: "a.rs".to_string(),
            start_line: 1,
            start_col: 0,
            end_line: None,
            end_col: None,
            kinds: None,
        };
        let out = code_actions_for(&state, &repo, &body).await;
        assert!(!out.available);
        assert_eq!(out.reason, Some("blob_stale"));
        assert!(
            out.actions.is_empty(),
            "a blob mismatch must discard, never downgrade"
        );
    }

    #[tokio::test]
    async fn success_returns_actions_and_relays_dropped_counters_verbatim() {
        let (root, repo) = fixture_repo("fixture");
        let src = "fn main() {}\n";
        write_file(root.path(), "a.rs", src);
        let hash = crate::ingest::git_blob_hash(src.as_bytes());
        let router = Router::new()
            .route(
                "/lip/identity",
                get(|| async {
                    AxumJson(
                        json!({"protocol": "lip/1", "lip_major": 1, "capabilities": ["code-actions"]}),
                    )
                }),
            )
            .route(
                "/lip/code-actions",
                post(move || {
                    let hash = hash.clone();
                    async move {
                        AxumJson(json!({
                            "verified_blob_sha": hash,
                            "results": [
                                {"title": "Add missing import", "kind": "quickfix",
                                 "is_preferred": true, "edits": [{"path": "a.rs", "edits": [
                                    {"start_line": 1, "start_col": 0, "end_line": 1,
                                     "end_col": 0, "new_text": "use foo;\n"}]}]},
                                // A malformed entry (no "edits") — must be dropped, not panic.
                                {"title": "broken", "kind": "quickfix"},
                            ],
                            "dropped_command_only": 3,
                            "dropped_unsupported": 2,
                        }))
                    }
                }),
            );
        let (addr, _server) = mock_kb_server(router).await;
        let state = test_state(
            vec![provider_entry("rust-analyzer", format!("http://{addr}"))],
            root.path(),
            "fixture",
        )
        .await;
        let body = CodeActionsBody {
            repo: "fixture".to_string(),
            path: "a.rs".to_string(),
            start_line: 1,
            start_col: 0,
            end_line: None,
            end_col: None,
            kinds: None,
        };
        let out = code_actions_for(&state, &repo, &body).await;
        assert!(out.available);
        assert!(out.verified);
        assert!(out.reason.is_none());
        assert_eq!(out.provider.as_deref(), Some("rust-analyzer"));
        assert_eq!(out.actions.len(), 1, "the malformed entry must be dropped");
        assert_eq!(out.actions[0].title, "Add missing import");
        assert!(out.actions[0].is_preferred);
        assert_eq!(out.actions[0].edits[0].path, "a.rs");
        assert_eq!(out.dropped.command_only, 3);
        assert_eq!(out.dropped.unsupported, 2);
    }

    // --- parse_code_actions: defensive parsing --------------------------

    #[test]
    fn parse_code_actions_reads_the_translated_shape() {
        let results = json!([
            {"title": "Fix", "kind": "quickfix", "is_preferred": false,
             "edits": [{"path": "a.rs", "edits": [
                {"start_line": 3, "start_col": 4, "end_line": 3, "end_col": 9, "new_text": "x"}
             ]}]}
        ]);
        let actions = parse_code_actions(&results);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].title, "Fix");
        assert_eq!(actions[0].kind, "quickfix");
        assert_eq!(actions[0].edits[0].path, "a.rs");
        assert_eq!(actions[0].edits[0].edits[0].new_text, "x");
    }

    #[test]
    fn parse_code_actions_drops_an_entry_with_no_title() {
        let results = json!([{"kind": "quickfix", "edits": []}]);
        assert!(parse_code_actions(&results).is_empty());
    }

    #[test]
    fn parse_code_actions_drops_an_entry_with_no_edits_key() {
        let results = json!([{"title": "command only", "kind": "quickfix"}]);
        assert!(parse_code_actions(&results).is_empty());
    }

    #[test]
    fn parse_code_actions_drops_an_entry_whose_edits_are_all_malformed() {
        let results = json!([{"title": "Fix", "edits": [{"path": "a.rs", "edits": [
            {"start_line": 3} // missing every other required field
        ]}]}]);
        assert!(parse_code_actions(&results).is_empty());
    }

    #[test]
    fn parse_code_actions_defaults_a_missing_kind_to_empty_and_is_preferred_to_false() {
        let results = json!([{"title": "Fix", "edits": [{"path": "a.rs", "edits": [
            {"start_line": 1, "start_col": 0, "end_line": 1, "end_col": 1, "new_text": "y"}
        ]}]}]);
        let actions = parse_code_actions(&results);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].kind, "");
        assert!(!actions[0].is_preferred);
    }

    #[test]
    fn parse_code_actions_empty_results_is_empty_not_missing() {
        assert!(parse_code_actions(&json!([])).is_empty());
        assert!(parse_code_actions(&json!(null)).is_empty());
    }
}
