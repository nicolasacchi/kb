//! `GET /api/events.schema.json` — enum of this daemon's SSE event kinds.
//! `GET /api/events/schema/{type}/v1.json` — per-type payload-fields list.
//!
//! Mirrors kb-server's own `routes/schema.rs` (`crates/kb-server/src/routes/
//! schema.rs`) at kb-code's own scale: kb-code-server runs its OWN
//! `kb_core::events::EventBus` (`AppState::bus`, `sink.rs`'s live-mirror
//! sink + the various route mutation handlers publish onto it — see
//! `routes::events`'s own doc, which undersold the vocabulary as "exactly
//! two types" until this module's drift audit below). Kept minimal like
//! kb-server's v0.0.1 shape — name + payload fields, not a full JSON Schema
//! document.
//!
//! `V1_TYPES` is the source of truth; `drift::` (test-only) audits it
//! against every `EventBus::emit` call site under this crate's own `src/`
//! and fails if one is missing. **Update protocol**: when you add a new
//! `bus.emit("some.kind", ...)` call site anywhere in this crate, add
//! `"some.kind"` to `V1_TYPES` below (and a matching arm in `per_type`) in
//! the SAME commit — `cargo test -p kb-code-server schema::drift` catches an
//! omission, but the fastest fix is to never let it drift in the first
//! place.

use axum::{
    body::Body,
    extract::Path,
    http::{Response, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Serialize;
use serde_json::json;

const V1_TYPES: &[&str] = &[
    // W1.4/W1.6 — the live-mirror sink (`sink.rs`). `mirror.updated` fires
    // per touched-path batch (initial index walk, live watcher events, and
    // the reconciler's full-reconcile pass all funnel through
    // `emit_mirror_updated`); `repo.head_moved` fires once per detected
    // `HEAD` move (checkout, branch switch, rebase/reset), old sha `None`
    // for a repo's first-ever observed HEAD.
    "mirror.updated",
    "repo.head_moved",
    // W4.6 — code annotations (`routes.rs`). One event per mutation; the
    // V4.C2 batch route reuses the same kind with an additive `batch: true`
    // + `paths` (plural) instead of `path`.
    "annotation.changed",
    // Phase E3 — reading sets (`reading_sets.rs`). Per-repo scoped, fired
    // on every create/patch/delete/append-span mutation.
    "set.changed",
    // Phase N — bookmarks (`bookmarks.rs`). Per-repo scoped, fired on
    // create/update/delete.
    "bookmark.changed",
    // V3.R1 — local review sessions (`reviews.rs`). `reason` distinguishes
    // the mutation kind (`patchset` | `meta` | `deleted` | `verdict` |
    // `pr_bound` | `report` [PRR-R2] | `findings_import` | `disposition`
    // [PRR-R3]); `deleted: true` is additive when the review itself was
    // removed. PRR-R3's `findings_import`/`disposition` reasons carry an
    // additive `finding_slug` (absent for a batch import, present for a
    // manual create or a disposition set/clear — see `review_findings.rs`).
    "review.changed",
    // V4.S1 — suggestion apply, the daemon's second sanctioned
    // working-tree mutation (`suggestions.rs`).
    "suggestion.applied",
    // DCB W2.A/D-A — the doc-lens pin lifecycle (`doclens/pins.rs`); `repo`
    // is JSON `null` on the delete path (the pin no longer resolves to one).
    "doclens.pin.changed",
    // DCB W3.A — the doc_refs reverse-index sync pass, per synced kb
    // (`doclens/sync.rs`), not once per whole pass — mirrors `set.changed`'s
    // per-repo (not per-pass) scoping precedent (see that route's own
    // comment).
    "doc_refs.synced",
];

#[derive(Debug, Serialize)]
pub struct SchemaEnum {
    pub envelope: serde_json::Value,
    pub types: Vec<&'static str>,
}

pub async fn enum_get() -> Json<SchemaEnum> {
    Json(SchemaEnum {
        envelope: json!({
            "fields": ["v", "id", "type", "ts", "payload"],
            "v": 1,
        }),
        types: V1_TYPES.to_vec(),
    })
}

pub async fn per_type(Path((kind, version)): Path<(String, String)>) -> Response<Body> {
    if version != "v1.json" && version != "v1" {
        return (StatusCode::NOT_FOUND, "only v1 schemas today").into_response();
    }
    if !V1_TYPES.contains(&kind.as_str()) {
        return (StatusCode::NOT_FOUND, format!("unknown event type: {kind}")).into_response();
    }
    let schema = match kind.as_str() {
        "mirror.updated" => json!({"type": kind, "payload": ["repo", "paths"]}),
        "repo.head_moved" => json!({"type": kind, "payload": ["repo", "old", "new"]}),
        "annotation.changed" => {
            json!({"type": kind, "payload": ["repo", "path?", "paths?", "batch?", "review_id?"]})
        }
        "set.changed" => json!({"type": kind, "payload": ["repo"]}),
        "bookmark.changed" => json!({"type": kind, "payload": ["repo"]}),
        "review.changed" => {
            json!({"type": kind, "payload": ["review_id", "repo", "reason", "deleted?", "finding_slug?"]})
        }
        "suggestion.applied" => {
            json!({"type": kind, "payload": ["repo", "path", "annotation_id", "review_id?"]})
        }
        "doclens.pin.changed" => json!({"type": kind, "payload": ["kb", "doc_id", "repo"]}),
        "doc_refs.synced" => json!({"type": kind, "payload": ["kb", "resolved"]}),
        _ => unreachable!("contains-check above"),
    };
    Json(schema).into_response()
}

#[cfg(test)]
mod drift {
    //! Drift guard: regex-scans this crate's own `src/` for every
    //! `EventBus::emit` call site (the SAME shapes the survey behind
    //! `V1_TYPES` above was built from — `bus.emit("kind", ...)` /
    //! `.bus.emit("kind", ...)` on either a single line or split across
    //! lines) and asserts every literal it finds is declared in
    //! `V1_TYPES`. Anchored on the precise call shape (a `.emit(` method
    //! call whose first argument is a string literal — never a bare dotted
    //! string anywhere in the file) to avoid false positives from payload
    //! JSON keys (`"kb"`, `"repo"`, …) or prose. The local, non-`EventBus`
    //! `emit` helpers in `keypath.rs`/`yaml.rs` (YAML/keypath tree walkers)
    //! take a non-string-literal first argument, so this pattern does not
    //! match them.

    use super::V1_TYPES;
    use std::path::Path;

    /// Every `.emit("literal.kind"` occurrence in `src`, allowing the
    /// call's opening paren and the string literal to be separated by
    /// whitespace/newlines (the common multi-line `bus.emit(\n    "kind",`
    /// formatting `cargo fmt` produces once a call has a payload argument).
    fn emit_literals_in(src: &str) -> Vec<String> {
        let bytes = src.as_bytes();
        let mut out = Vec::new();
        let needle = b".emit(";
        let mut i = 0;
        while let Some(pos) = find(bytes, needle, i) {
            let mut j = pos + needle.len();
            // Skip whitespace/newlines between `(` and the argument.
            while j < bytes.len() && (bytes[j] as char).is_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'"' {
                let start = j + 1;
                if let Some(end_rel) = src[start..].find('"') {
                    out.push(src[start..start + end_rel].to_string());
                }
            }
            i = pos + needle.len();
        }
        out
    }

    fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
        if from >= haystack.len() || needle.is_empty() {
            return None;
        }
        haystack[from..]
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|p| p + from)
    }

    /// A literal is a plausible event kind (not a payload-key/prose false
    /// positive) if it is lowercase, dotted-or-bare, and free of spaces —
    /// every real kind in this crate is `word(.word)+` or a bare `word`.
    /// This is a filter on top of the precise `.emit(` anchor above, not a
    /// replacement for it.
    fn looks_like_kind(s: &str) -> bool {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_')
    }

    fn collect_rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rs_files(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn every_emitted_kind_is_declared_in_v1_types() {
        let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect_rs_files(&src_dir, &mut files);
        assert!(
            files.len() > 50,
            "sanity: expected to walk this crate's whole src/ tree, got {} files",
            files.len()
        );
        // This file's own doc comments (module doc + the fn docs below)
        // deliberately show example `.emit("kind", …)` call shapes —
        // excluded so the scan doesn't flag its own documentation prose as
        // an undeclared kind.
        let self_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/schema.rs");

        let mut missing = Vec::new();
        for file in &files {
            if file == &self_path {
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(file) else {
                continue;
            };
            for lit in emit_literals_in(&contents) {
                if !looks_like_kind(&lit) {
                    continue; // not a plausible event-kind literal
                }
                // `.emit(evt, …)`/`.emit(kind, …)` (a variable, not a
                // literal) never matches `emit_literals_in` in the first
                // place — it only captures a literal `"` right after the
                // call's opening paren. `keypath.rs`/`yaml.rs`'s own
                // `emit(pair, …)`/`emit(table, …)` helpers pass idents, not
                // string literals, so they never appear here either.
                if !V1_TYPES.contains(&lit.as_str()) && !missing.contains(&lit) {
                    missing.push(format!("{lit} ({})", file.display()));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "event kind(s) emitted but missing from schema::V1_TYPES: {missing:#?}"
        );
    }
}
