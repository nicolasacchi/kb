//! V70-A8 (D20 CLI hygiene) — the `--json` envelope shape and the process
//! exit-code table.
//!
//! # Scope: infrastructure + the NEW verbs this unit adds, not a retrofit
//!
//! D20 documents ONE envelope
//! (`{schema, ok, data, warnings, degraded, empty_reason}` on success,
//! `{ok:false, error:{code, message, hint}}` on failure) and ONE exit-code
//! table (2 usage, 3 conflict, 4 refused, 5 unreachable) meant to apply
//! "consistently across verbs." Retrofitting the ~150 pre-existing verbs'
//! OWN `--json` output onto this shape is out of scope for this unit — each
//! one currently pretty-prints its raw daemon response body verbatim, and
//! changing that is a wire-format change for every existing consumer
//! (scripts, the annotations hook, `plugins/kb-code`) that this unit's
//! throttle budget (one `cargo check`, no test run) can't safely risk. So:
//!
//! * [`print_ok`] is used by every BRAND NEW verb this unit adds (`tools`,
//!   `schema`, `token path`, `doctor`, `diff`, `commit`, `file-history`,
//!   `range-diff`, `scopes`, `doc-refs`, `review impact`, `review findings
//!   recurrence`, `pr reviews`) — a coherent, real demonstration of the
//!   documented shape, not a placeholder.
//! * [`exit_code_for`] is wired ONCE, centrally, in `main()` — see that
//!   function's doc — so it applies to EVERY verb (old and new alike)
//!   that goes through the shared [`crate::get_json`]/[`crate::post_json`]/
//!   `*_raw` HTTP helpers, with zero per-verb changes. This is the one
//!   piece of the table that IS universal, because it hooks the shared
//!   choke point rather than the ~150 individual call sites.
//!
//! # The exit-code table: one status -> exit function
//!
//! [`exit_for_status`] is the ONE status-to-exit mapping. Three producers
//! feed it: `review_agent::AgentError::from_http`, `store_cmd::failure`, and
//! [`exit_code_for`] (which reads either a `reqwest::Error` status or a
//! [`StatusError`] out of the `anyhow` chain). The `*_raw` helpers
//! (`post_json_raw`, ...) return `(StatusCode, Value)` and drop the
//! `reqwest::Error`, so the verbs that build their failure through
//! `annotation_api_error` / `loopback_or_api_error` carry a typed
//! [`StatusError`] instead; without it those exited 1 for every status.
//!
//! Loopback-only routes 404 a non-loopback caller (deliberately —
//! `router.rs`'s own doc). A 404 is therefore STRUCTURALLY ambiguous
//! between "this route is not yours" and "this resource does not exist", and
//! maps to [`EXIT_NOT_FOUND`] on the status alone. [`EXIT_REFUSED`] is the
//! unambiguous case: a 401/403.

use serde::Serialize;
use serde_json::json;

/// Never constructed (a clean exit is the ABSENCE of a call to
/// `std::process::exit`) — present so the full table is readable as code
/// in one place, not split across doc comments and tribal knowledge.
#[allow(dead_code)]
pub const EXIT_OK: i32 = 0;
/// Default/unclassified failure — byte-identical to every pre-V70-A8 exit
/// code (the `#[tokio::main] async fn main() -> Result<()>` default).
pub const EXIT_GENERIC: i32 = 1;
/// clap's OWN exit code for a parse/usage error (`--help`, unknown flag,
/// missing required arg) — `Cli::parse()` calls `std::process::exit` with
/// this before any code in this module ever runs. RS-U10a's review verbs
/// (`crate::review_agent`) reuse the slot for the same class of caller
/// mistake clap cannot see: a malformed `<id>`/`pr:<N>`/`<id>/ps<n>`
/// address, an ambiguous `pr:<N>`, or a daemon 400 (any verb, via
/// [`exit_for_status`]).
pub const EXIT_USAGE: i32 = 2;
/// The daemon's response was well-formed but reports a conflict with
/// current state (HTTP 409 — a drifted suggestion apply, a dirty-tree
/// checkout refusal, …).
pub const EXIT_CONFLICT: i32 = 3;
/// An unambiguous bearer-auth refusal (HTTP 401/403). See the module doc
/// for why an opaque loopback-only 404 is deliberately NOT folded in here.
pub const EXIT_REFUSED: i32 = 4;
/// The daemon could not be reached at all (connection refused, DNS
/// failure, timeout) — never got as far as an HTTP status.
pub const EXIT_UNREACHABLE: i32 = 5;
// ── RS-U3 (review store) — begin ──
/// The daemon was reached but an UPSTREAM it depends on failed (a forge
/// fetch/API call: offline, auth, vanished). Used by `store sync`.
pub const EXIT_UPSTREAM: i32 = 6;
/// The verb partly succeeded (e.g. `store sync` fetched some members but
/// not all). The JSON envelope carries `degraded: true` and the details.
pub const EXIT_PARTIAL: i32 = 7;
/// Every HTTP 404, decided on the STATUS ALONE — including a loopback-only
/// route's bodiless refusal. It does NOT mean "definitely absent": a
/// `loopback_only` route answers the same 404 for a route that exists but is
/// not yours, so 8 is structurally ambiguous between "no such review" and "you
/// cannot see this". Read the body's `code`/`hint` before concluding the thing
/// is missing — `docs/kb-code.md`'s exit-code table says the same, and this
/// constant is what that table is transcribed from.
///
/// Mapped by [`exit_for_status`], which every mapper shares: 404 -> 8 for the
/// review verbs (`AgentError::from_http`), the store verbs
/// (`store_cmd::failure`) and every other verb (`exit_code_for`, via the
/// `reqwest::Error` status or a [`StatusError`]).
pub const EXIT_NOT_FOUND: i32 = 8;
// ── RS-U3 (review store) — end ──

/// Print the success envelope: `{schema, ok:true, data, warnings, degraded,
/// empty_reason}`. `schema` should name the DATA's own shape (e.g.
/// `"diff/1"`, matching the `schema` field many daemon responses already
/// carry internally — reuse that string rather than inventing a second
/// name for the same shape where one already exists).
pub fn print_ok<T: Serialize>(
    schema: &str,
    data: T,
    warnings: Vec<String>,
    degraded: bool,
    empty_reason: Option<&str>,
) {
    let body = json!({
        "schema": schema,
        "ok": true,
        "data": data,
        "warnings": warnings,
        "degraded": degraded,
        "empty_reason": empty_reason,
    });
    match serde_json::to_string_pretty(&body) {
        Ok(s) => println!("{s}"),
        Err(_) => println!("{body}"),
    }
}

// ── RS-U10a (agent-facing review CLI, README §13) — begin ──

/// A suggested follow-up command, as an argv vector (`["kb-code", "review",
/// "diff", "12", "--patch"]`) — never a shell string, so an agent can
/// exec it without quoting rules.
pub type NextArgv = Vec<String>;

/// The success envelope as a VALUE: [`print_ok`]'s shape plus a top-level
/// `next` array of suggested follow-up argvs. The RS-U10a verbs build it
/// through this function so a test can validate the exact bytes a verb
/// would print without a daemon.
pub fn ok_value<T: Serialize>(
    schema: &str,
    data: T,
    warnings: Vec<String>,
    degraded: bool,
    empty_reason: Option<&str>,
    next: Vec<NextArgv>,
) -> serde_json::Value {
    json!({
        "schema": schema,
        "ok": true,
        "data": data,
        "warnings": warnings,
        "degraded": degraded,
        "empty_reason": empty_reason,
        "next": next,
    })
}

/// `code` as a URN: `"not-found"` → `"urn:kb:errors:not-found"`; an
/// already-URN code passes through unchanged.
pub fn error_urn(code: &str) -> String {
    if code.starts_with("urn:") {
        code.to_string()
    } else {
        format!("urn:kb:errors:{code}")
    }
}

/// The typed error envelope as a VALUE: `{ok:false, error:{code (a URN),
/// message, hint, next}}` (+ `candidates` when an address was ambiguous).
/// Printed to STDERR by the RS-U10a verbs — stdout stays reserved for the
/// success document, exactly like [`print_err`].
pub fn err_value(
    code: &str,
    message: &str,
    hint: Option<&str>,
    next: &[NextArgv],
    candidates: Option<&serde_json::Value>,
) -> serde_json::Value {
    let mut error = json!({
        "code": error_urn(code),
        "message": message,
        "hint": hint,
        "next": next,
    });
    if let Some(c) = candidates {
        error["candidates"] = c.clone();
    }
    json!({ "ok": false, "error": error })
}

/// Pretty-print a value to stdout (the RS-U10a success path).
pub fn print_value(v: &serde_json::Value) {
    match serde_json::to_string_pretty(v) {
        Ok(s) => println!("{s}"),
        Err(_) => println!("{v}"),
    }
}

// ── RS-U10a — end ──

/// Print the error envelope (`{ok:false, error:{code, message, hint}}`) to
/// STDERR. `code` is a short machine tag (e.g. `"unreachable"`,
/// `"not-found"`), never a bare number — an agent branching on this should
/// never have to keep this module's exit-code table memorized.
pub fn print_err(code: &str, message: &str, hint: Option<&str>) {
    let body = json!({
        "ok": false,
        "error": { "code": code, "message": message, "hint": hint },
    });
    match serde_json::to_string_pretty(&body) {
        Ok(s) => eprintln!("{s}"),
        Err(_) => eprintln!("{body}"),
    }
}

/// THE status-to-exit mapping every mapper shares (`AgentError::from_http`,
/// `store_cmd::failure`, [`exit_code_for`]): 400 usage, 401/403 refused, 404
/// not-found, 409/503 conflict, anything else generic.
pub fn exit_for_status(status: u16) -> i32 {
    match status {
        400 => EXIT_USAGE,
        401 | 403 => EXIT_REFUSED,
        404 => EXIT_NOT_FOUND,
        409 | 503 => EXIT_CONFLICT,
        _ => EXIT_GENERIC,
    }
}

/// A non-2xx daemon answer as a typed error. The `*_raw` helpers return
/// `(StatusCode, Value)` and drop the `reqwest::Error`, so their failure
/// builders (`annotation_api_error`, `loopback_or_api_error`) wrap this in
/// the `anyhow` chain for [`exit_code_for`] to downcast. `Display` is the
/// human message, byte-identical to the pre-typed `anyhow!` text.
#[derive(Debug)]
pub struct StatusError {
    pub status: u16,
    pub message: String,
    /// Set (to the daemon URL) when this is a LOOPBACK-ONLY route's bodiless
    /// 404. `main()` then asks `GET /api/identity` whether the caller is
    /// loopback; if not, the failure is `needs-daemon-host` (exit 4), not a
    /// confusing not-found (v0.44 F5).
    pub loopback_gate: Option<String>,
}

impl StatusError {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            loopback_gate: None,
        }
    }

    pub fn with_loopback_gate(mut self, daemon: impl Into<String>) -> Self {
        self.loopback_gate = Some(daemon.into());
        self
    }
}

/// The daemon URL of a loopback-gate 404 anywhere in the cause chain.
pub fn loopback_gate_daemon(err: &anyhow::Error) -> Option<String> {
    err.chain()
        .find_map(|c| c.downcast_ref::<StatusError>()?.loopback_gate.clone())
}

/// Read `caller_loopback` out of a `GET /api/identity` body. `None` when the
/// field is absent (older daemon) or not a bool — the caller then keeps the
/// plain not-found exit rather than guessing.
pub fn caller_loopback_from_identity(body: &serde_json::Value) -> Option<bool> {
    body.get("caller_loopback")?.as_bool()
}

impl std::fmt::Display for StatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StatusError {}

/// Resolve the process exit code for a top-level command failure. Walks
/// the WHOLE `anyhow` cause chain (not just the top frame) looking for a
/// [`StatusError`] (the `*_raw` helpers' failures) or the underlying
/// `reqwest::Error` (`get_json`/`post_json` wrap it via `.with_context(...)`,
/// which PRESERVES it as the chain's `source()`). Statuses map through
/// [`exit_for_status`].
pub fn exit_code_for(err: &anyhow::Error) -> i32 {
    for cause in err.chain() {
        if let Some(se) = cause.downcast_ref::<StatusError>() {
            return exit_for_status(se.status);
        }
        if let Some(re) = cause.downcast_ref::<reqwest::Error>() {
            if re.is_connect() || re.is_timeout() || (re.is_request() && re.status().is_none()) {
                return EXIT_UNREACHABLE;
            }
            if let Some(status) = re.status() {
                let exit = exit_for_status(status.as_u16());
                if exit != EXIT_GENERIC {
                    return exit;
                }
            }
        }
    }
    EXIT_GENERIC
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap(e: reqwest::Error) -> anyhow::Error {
        anyhow::Error::new(e).context("GET http://127.0.0.1:4747/api/whatever")
    }

    #[tokio::test]
    async fn connection_refused_is_unreachable() {
        // A port nothing listens on, guaranteed to refuse the connection
        // fast rather than hang — no real network access needed.
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(500))
            .build()
            .unwrap();
        let err = client
            .get("http://127.0.0.1:1")
            .send()
            .await
            .expect_err("nothing listens on port 1");
        assert_eq!(exit_code_for(&wrap(err)), EXIT_UNREACHABLE);
    }

    #[test]
    fn non_http_error_defaults_to_generic() {
        let err = anyhow::anyhow!("some unrelated failure");
        assert_eq!(exit_code_for(&err), EXIT_GENERIC);
    }

    /// A status `reqwest::Error` built the way `error_for_status` builds it,
    /// from a one-shot local HTTP listener answering `code`.
    async fn status_error(code: u16) -> reqwest::Error {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 2048];
            let _ = sock.read(&mut buf).await;
            let resp =
                format!("HTTP/1.1 {code} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            let _ = sock.write_all(resp.as_bytes()).await;
        });
        reqwest::get(format!("http://{addr}/"))
            .await
            .unwrap()
            .error_for_status()
            .expect_err("non-2xx is an error")
    }

    const STATUSES: [(u16, i32); 7] = [
        (400, EXIT_USAGE),
        (401, EXIT_REFUSED),
        (403, EXIT_REFUSED),
        (404, EXIT_NOT_FOUND),
        (409, EXIT_CONFLICT),
        (500, EXIT_GENERIC),
        (503, EXIT_CONFLICT),
    ];

    #[test]
    fn exit_for_status_table() {
        for (status, want) in STATUSES {
            assert_eq!(exit_for_status(status), want, "status {status}");
        }
    }

    #[tokio::test]
    async fn exit_code_for_reads_every_reqwest_status_arm() {
        for (status, want) in STATUSES {
            assert_eq!(
                exit_code_for(&wrap(status_error(status).await)),
                want,
                "reqwest status {status}"
            );
        }
    }

    #[test]
    fn exit_code_for_reads_typed_status_error_from_the_raw_helpers() {
        for (status, want) in STATUSES {
            let e = anyhow::Error::new(StatusError::new(status, "x")).context("outer");
            assert_eq!(exit_code_for(&e), want, "StatusError {status}");
        }
    }

    #[test]
    fn all_three_mappers_agree_on_every_status() {
        for (status, want) in STATUSES {
            let from_http =
                crate::review_agent::AgentError::from_http(status, &serde_json::Value::Null, "x");
            assert_eq!(from_http.exit, want, "from_http {status}");
            let (_, store) = crate::store_cmd::failure(
                reqwest::StatusCode::from_u16(status).unwrap(),
                &serde_json::Value::Null,
            );
            assert_eq!(store, want, "store_cmd::failure {status}");
        }
    }
}
