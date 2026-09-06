//! TEST-ONLY FIXTURE — a minimal stdio LSP server, driven exclusively by
//! kb-lip's own integration tests (`tests/`) via
//! `env!("CARGO_BIN_EXE_kb-lip-fake-lsp")`. Never shipped as part of a
//! real deployment's supervised child (that's always the operator's own
//! `command` in `config.toml` — a real `ruby-lsp`/`solargraph`/etc). No
//! real language server runs in CI (design-lip.md, Phase L1).
//!
//! Speaks just enough LSP over Content-Length-framed stdio JSON-RPC to
//! drive kb-lip's client through: initialize/initialized handshake,
//! hover (echoing the position it was asked about, so the test can assert
//! the byte->UTF-16 conversion landed correctly), definition, references
//! (count controlled by `FAKE_LSP_REF_COUNT`, default 5 — lets a test ask
//! for more than lip/1's 200 cap), documentSymbol, and shutdown/exit.
//! didChange/didClose are accepted and ignored (notifications, never
//! replied to). didOpen additionally PUSHES a canned
//! `textDocument/publishDiagnostics` notification (unless
//! `FAKE_LSP_DIAG_SUPPRESS=1`, simulating a server that never publishes —
//! exercises `/lip/diagnostics`'s empty-after-timeout path), optionally
//! after sleeping `FAKE_LSP_DIAG_DELAY_MS` first (simulating a slow
//! analysis pass — exercises the wait path catching a delayed publish).
//! `FAKE_LSP_DEFINITION_LOCATION_LINK=1` makes `textDocument/definition`
//! reply in `LocationLink[]` shape (`targetUri`/`targetRange`/
//! `targetSelectionRange`) instead of the default `Location[]` shape —
//! regression pin for the real-world finding that ruby-lsp replies with
//! LocationLink unconditionally regardless of client `linkSupport`
//! (PRR-L3 smoke, PRR-L4 fix). `FAKE_LSP_CWD_FILE=<path>`, if set, makes
//! this fixture write its own process cwd to that path at startup —
//! regression pin for `LspClient::start` spawning the child with
//! `workspace_root` as its cwd (PRR-L4 fix): a test sets `workspace_root`
//! to a tempdir, points this env var at a file inside it, and asserts the
//! written cwd matches — proving `Command::current_dir` was actually
//! applied rather than inheriting kb-lip's own process cwd.
//! `FAKE_LSP_PROGRESS_TOKEN=<token>`, if set, makes this fixture send a
//! `window/workDoneProgress/create` request + a `$/progress` `begin` for
//! that token right after receiving `initialized` — followed immediately
//! by a matching `end` UNLESS `FAKE_LSP_PROGRESS_NO_END=1`, in which case
//! the token is left open forever (simulates an in-progress cold-boot
//! index). Regression pin for `lsp::ProgressTracker`/`is_indexing`
//! (PRR-L4 fix) — mirrors the real `$/progress` begin-then-end lifecycle
//! confirmed live against ruby-lsp 0.26.11.
//! `FAKE_LSP_DIAG_PULL=1` switches this fixture from PUSH to LSP 3.17
//! PULL diagnostics mode — regression pin for PRR-L5's fix of the PRR-L4
//! finding that ruby-lsp 0.26.11 implements ONLY pull diagnostics and
//! never pushes `publishDiagnostics`. In this mode: `initialize`
//! additionally advertises `capabilities.diagnosticProvider`; `didOpen`
//! does NOT push a `publishDiagnostics` notification (mirroring a
//! pull-only real server exactly — implies `FAKE_LSP_DIAG_SUPPRESS`
//! regardless of its own setting); and `textDocument/diagnostic` requests
//! are answered directly: a request with no `previousResultId`, or one
//! that doesn't match `FAKE_PULL_RESULT_ID`, gets a FULL report
//! (`{"kind": "full", "resultId": FAKE_PULL_RESULT_ID, "items":
//! [canned_diagnostic()]}`); a request whose `previousResultId` DOES match
//! gets `{"kind": "unchanged", "resultId": FAKE_PULL_RESULT_ID}` (no
//! `items` — exercises the client's own cache-serve path, since an
//! unchanged reply carries no diagnostics to translate).
//!
//! `FAKE_LSP_CODE_ACTION_MODE=<mode>` (S2-C) makes this fixture advertise
//! `capabilities.codeActionProvider: {"resolveProvider": true}` at
//! `initialize` and answer `textDocument/codeAction`/`codeAction/resolve`
//! per `<mode>` — UNSET (the default) advertises no `codeActionProvider`
//! at all, exercising kb-lip's ordinary capability-absent
//! `refused: "unsupported"` path. Modes:
//! - `literal` — one `CodeAction` with an inline `.edit` (a single
//!   `changes` entry on the queried uri) — no resolve round trip needed.
//!   The returned `title` additionally echoes the `context.only` it
//!   received (`"... only=<json>"`) — a regression pin for kb-lip
//!   forwarding `kinds` into `CodeActionContext.only` unmodified, since
//!   there is no other way to observe the OUTGOING LSP request from an
//!   HTTP-only integration test.
//! - `resolve` — one `CodeAction` with `data` but NO `.edit`; the
//!   `data` carries the queried uri so `codeAction/resolve` (below) can
//!   build a same-file edit. `FAKE_LSP_CODE_ACTION_RESOLVE_DELAY_MS`, if
//!   set, sleeps that long before replying to `codeAction/resolve` —
//!   lets a test race a file mutation against the in-flight resolve (see
//!   `blob_mismatch_mid_resolve` below).
//! - `blob_mismatch_mid_resolve` — IDENTICAL server behavior to `resolve`
//!   (the mutation itself is the TEST's job, timed against
//!   `FAKE_LSP_CODE_ACTION_RESOLVE_DELAY_MS`); kept as a separate mode
//!   name purely for test-intent clarity.
//! - `command_only` — one `CodeAction` with neither `.edit` nor `data` —
//!   nothing for kb-lip to resolve, exercising `dropped_command_only`.
//! - `multi_file` — one `CodeAction` whose `.edit.documentChanges` touches
//!   BOTH the queried uri and a second file, `FAKE_LSP_CODE_ACTION_
//!   SECOND_URI` (a COMPLETE `file://` uri, supplied verbatim by the
//!   test — not reconstructed from this process's own cwd, so there is no
//!   dependency on this fixture's OS-reported cwd string matching the
//!   Rust-side `workspace_root` byte-for-byte).
//! - `out_of_root` — one `CodeAction` whose `.edit.changes` names a fixed
//!   `file:///etc/kb-lip-fixture-outside-workspace` uri, unconditionally
//!   outside any tempdir-based `workspace_root` a test could use —
//!   exercising `dropped_unsupported`.
//!
//! `codeAction/resolve` (any mode using it) replies by echoing the
//! request's own `params` back with an `.edit` spliced in: a single
//! `changes` entry on `params.data.uri`, one text edit.

use kb_lip::rpc;
use serde_json::{json, Value};
use std::io::{self, Write};
use std::time::Duration;

fn ref_count() -> usize {
    std::env::var("FAKE_LSP_REF_COUNT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5)
}

/// The fixed `resultId` this fixture always replies with in pull mode —
/// see the module doc comment. One constant is enough: each test spawns
/// its own fresh fixture PROCESS (no state to collide across tests).
const FAKE_PULL_RESULT_ID: &str = "fake-pull-result-1";

fn pull_mode() -> bool {
    std::env::var("FAKE_LSP_DIAG_PULL").ok().as_deref() == Some("1")
}

/// S2-C code-actions fixture mode — see the module doc comment. Empty
/// string when unset, matched against below (never `None`, so a typo'd
/// env var value falls into the `_ => json!([])` default branch rather
/// than panicking).
fn code_action_mode() -> String {
    std::env::var("FAKE_LSP_CODE_ACTION_MODE").unwrap_or_default()
}

fn reply<W: Write>(w: &mut W, id: &Value, result: Value) {
    let msg = json!({"jsonrpc": "2.0", "id": id, "result": result});
    rpc::blocking::write_message(w, &msg).expect("write reply");
}

fn reply_error<W: Write>(w: &mut W, id: &Value, code: i64, message: &str) {
    let msg = json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}});
    rpc::blocking::write_message(w, &msg).expect("write error reply");
}

fn notify<W: Write>(w: &mut W, method: &str, params: Value) {
    let msg = json!({"jsonrpc": "2.0", "method": method, "params": params});
    rpc::blocking::write_message(w, &msg).expect("write notification");
}

/// One canned diagnostic — content is arbitrary, tests assert on its
/// exact shape (`translate_diagnostics`'s golden test uses the same
/// fields for that reason).
fn canned_diagnostic() -> Value {
    json!({
        "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 3}},
        "severity": 1,
        "code": "FAKE001",
        "source": "fake-lsp",
        "message": "fake diagnostic"
    })
}

fn handle<W: Write>(msg: &Value, w: &mut W) -> bool {
    let method = msg.get("method").and_then(Value::as_str);
    let id = msg.get("id").cloned();

    match (method, id) {
        (Some("initialize"), Some(id)) => {
            let mut capabilities = json!({
                "hoverProvider": true,
                "definitionProvider": true,
                "referencesProvider": true,
                "documentSymbolProvider": true,
                "textDocumentSync": 1
            });
            if pull_mode() {
                capabilities["diagnosticProvider"] = json!({
                    "interFileDependencies": false,
                    "workspaceDiagnostics": false
                });
            }
            if !code_action_mode().is_empty() {
                capabilities["codeActionProvider"] = json!({"resolveProvider": true});
            }
            reply(
                w,
                &id,
                json!({
                    "capabilities": capabilities,
                    "serverInfo": {
                        "name": "fake-lsp",
                        "version": "0.1.0-test"
                    }
                }),
            );
        }
        (Some("shutdown"), Some(id)) => {
            reply(w, &id, Value::Null);
        }
        (Some("exit"), None) => {
            return true; // stop the loop; process exits normally below.
        }
        (Some("textDocument/hover"), Some(id)) => {
            let pos = msg["params"]["position"].clone();
            let line = pos["line"].as_u64().unwrap_or(0);
            let character = pos["character"].as_u64().unwrap_or(0);
            reply(
                w,
                &id,
                json!({
                    "contents": {
                        "kind": "markdown",
                        "value": format!("hover at line={line} character={character}")
                    },
                    "range": {
                        "start": {"line": line, "character": character},
                        "end": {"line": line, "character": character + 5}
                    }
                }),
            );
        }
        (Some("textDocument/definition"), Some(id)) => {
            let uri = msg["params"]["textDocument"]["uri"]
                .as_str()
                .unwrap_or("file:///unknown")
                .to_string();
            let as_location_link = std::env::var("FAKE_LSP_DEFINITION_LOCATION_LINK")
                .ok()
                .as_deref()
                == Some("1");
            let result = if as_location_link {
                json!([{
                    "targetUri": uri,
                    "targetRange": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 2, "character": 3}
                    },
                    "targetSelectionRange": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 3}
                    }
                }])
            } else {
                json!([{
                    "uri": uri,
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 3}
                    }
                }])
            };
            reply(w, &id, result);
        }
        (Some("textDocument/references"), Some(id)) => {
            let uri = msg["params"]["textDocument"]["uri"]
                .as_str()
                .unwrap_or("file:///unknown")
                .to_string();
            let n = ref_count();
            let locations: Vec<Value> = (0..n)
                .map(|i| {
                    json!({
                        "uri": uri,
                        "range": {
                            "start": {"line": i as u64, "character": 0},
                            "end": {"line": i as u64, "character": 1}
                        }
                    })
                })
                .collect();
            reply(w, &id, Value::Array(locations));
        }
        (Some("textDocument/documentSymbol"), Some(id)) => {
            reply(
                w,
                &id,
                json!([{
                    "name": "fake_symbol",
                    "kind": 12,
                    "range": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 10}
                    },
                    "selectionRange": {
                        "start": {"line": 0, "character": 0},
                        "end": {"line": 0, "character": 10}
                    }
                }]),
            );
        }
        (Some("textDocument/diagnostic"), Some(id)) => {
            let previous_result_id = msg["params"]["previousResultId"].as_str();
            if previous_result_id == Some(FAKE_PULL_RESULT_ID) {
                reply(
                    w,
                    &id,
                    json!({"kind": "unchanged", "resultId": FAKE_PULL_RESULT_ID}),
                );
            } else {
                reply(
                    w,
                    &id,
                    json!({
                        "kind": "full",
                        "resultId": FAKE_PULL_RESULT_ID,
                        "items": [canned_diagnostic()]
                    }),
                );
            }
        }
        (Some("textDocument/codeAction"), Some(id)) => {
            let uri = msg["params"]["textDocument"]["uri"]
                .as_str()
                .unwrap_or("file:///unknown")
                .to_string();
            let only = msg["params"]["context"]["only"].clone();
            let result = match code_action_mode().as_str() {
                "literal" => json!([{
                    "title": format!("Add missing semicolon only={only}"),
                    "kind": "quickfix",
                    "isPreferred": true,
                    "edit": {
                        "changes": {
                            uri.clone(): [{
                                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}},
                                "newText": "// fixed\n"
                            }]
                        }
                    }
                }]),
                "resolve" | "blob_mismatch_mid_resolve" => json!([{
                    "title": "Extract variable",
                    "kind": "refactor.extract",
                    "data": {"uri": uri}
                }]),
                "command_only" => json!([{
                    "title": "Organize imports (command only)",
                    "kind": "source.organizeImports"
                }]),
                "multi_file" => {
                    let second_uri = std::env::var("FAKE_LSP_CODE_ACTION_SECOND_URI")
                        .unwrap_or_else(|_| "file:///unknown-second-file".to_string());
                    json!([{
                        "title": "Apply fix across two files",
                        "kind": "quickfix",
                        "edit": {
                            "documentChanges": [
                                {
                                    "textDocument": {"uri": uri, "version": Value::Null},
                                    "edits": [{
                                        "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}},
                                        "newText": "// primary\n"
                                    }]
                                },
                                {
                                    "textDocument": {"uri": second_uri, "version": Value::Null},
                                    "edits": [{
                                        "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}},
                                        "newText": "// second\n"
                                    }]
                                }
                            ]
                        }
                    }])
                }
                "out_of_root" => json!([{
                    "title": "Touches a file outside the workspace",
                    "kind": "quickfix",
                    "edit": {
                        "changes": {
                            "file:///etc/kb-lip-fixture-outside-workspace": [{
                                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}},
                                "newText": "nope"
                            }]
                        }
                    }
                }]),
                _ => json!([]),
            };
            reply(w, &id, result);
        }
        (Some("codeAction/resolve"), Some(id)) => {
            let delay_ms: u64 = std::env::var("FAKE_LSP_CODE_ACTION_RESOLVE_DELAY_MS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            if delay_ms > 0 {
                // Blocking sleep is fine — see the same rationale on
                // FAKE_LSP_DIAG_DELAY_MS above (single-threaded, one
                // message at a time; the real adapter's own timeout, if
                // any, is bounded and timeout-driven on ITS side).
                std::thread::sleep(Duration::from_millis(delay_ms));
            }
            let data_uri = msg["params"]["data"]["uri"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let mut resolved = msg["params"].clone();
            resolved["edit"] = json!({
                "changes": {
                    data_uri: [{
                        "range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 0}},
                        "newText": "extracted = true\n"
                    }]
                }
            });
            reply(w, &id, resolved);
        }
        (Some("textDocument/didOpen"), None) => {
            let uri = msg["params"]["textDocument"]["uri"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            // In pull mode this fixture mirrors a real pull-only server
            // (ruby-lsp 0.26.11, PRR-L4's finding) exactly: it NEVER pushes
            // publishDiagnostics, regardless of FAKE_LSP_DIAG_SUPPRESS.
            let suppress =
                pull_mode() || std::env::var("FAKE_LSP_DIAG_SUPPRESS").ok().as_deref() == Some("1");
            if !suppress {
                let delay_ms: u64 = std::env::var("FAKE_LSP_DIAG_DELAY_MS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                if delay_ms > 0 {
                    // A blocking sleep is fine here: this fixture handles
                    // one message at a time on a single thread, and the
                    // real adapter's wait is a bounded, timeout-driven
                    // poll on its own side regardless of how slow we are.
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                }
                notify(
                    w,
                    "textDocument/publishDiagnostics",
                    json!({"uri": uri, "diagnostics": [canned_diagnostic()]}),
                );
            }
        }
        (Some("initialized"), _) => {
            if let Ok(token) = std::env::var("FAKE_LSP_PROGRESS_TOKEN") {
                let create_req = json!({
                    "jsonrpc": "2.0",
                    "id": "fake-wdp-create",
                    "method": "window/workDoneProgress/create",
                    "params": {"token": token}
                });
                rpc::blocking::write_message(w, &create_req).expect("write wdp create");
                notify(
                    w,
                    "$/progress",
                    json!({"token": token, "value": {"kind": "begin", "title": "fake indexing"}}),
                );
                if std::env::var("FAKE_LSP_PROGRESS_NO_END").ok().as_deref() != Some("1") {
                    notify(
                        w,
                        "$/progress",
                        json!({"token": token, "value": {"kind": "end"}}),
                    );
                }
            }
        }
        // Notifications this fixture accepts and silently ignores.
        (Some("textDocument/didChange" | "textDocument/didClose" | "$/cancelRequest"), _) => {}
        // Any other REQUEST (has an id) gets a proper JSON-RPC error so
        // the client never hangs waiting on a reply we'll never send.
        (Some(other), Some(id)) => {
            reply_error(w, &id, -32601, &format!("method not found: {other}"));
        }
        // Any other notification: ignore.
        (Some(_), None) => {}
        (None, _) => {}
    }
    false
}

fn main() {
    // Written BEFORE the read loop starts, i.e. strictly before this
    // process can send the `initialize` reply — a caller that has
    // received the initialize response is guaranteed this file already
    // exists (see the module doc comment for what this pins).
    if let Ok(path) = std::env::var("FAKE_LSP_CWD_FILE") {
        let cwd = std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let _ = std::fs::write(path, cwd);
    }

    let stdin = io::stdin();
    let mut reader = io::BufReader::new(stdin.lock());
    let stdout = io::stdout();
    let mut writer = stdout.lock();

    loop {
        match rpc::blocking::read_message(&mut reader) {
            Ok(Some(msg)) => {
                if handle(&msg, &mut writer) {
                    break;
                }
            }
            Ok(None) => break,
            Err(e) => {
                eprintln!("fake-lsp: read error: {e}");
                break;
            }
        }
    }
}
