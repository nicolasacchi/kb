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
//! # The exit-code table's one honest gap
//!
//! Loopback-only routes 404 a non-loopback caller (deliberately —
//! `router.rs`'s own doc: hiding the route's existence is the point, the
//! same posture as an ordinary "no such id"). That means a 404 is
//! STRUCTURALLY ambiguous between "this route doesn't exist for you" and
//! "this resource doesn't exist" — the daemon does not, and per that
//! design must not, tell the two apart in the response. So
//! [`exit_code_for`] does NOT special-case 404 into [`EXIT_REFUSED`]; doing
//! so would need a signal the daemon deliberately withholds, and guessing
//! would print a confident-looking but wrong diagnosis. [`EXIT_REFUSED`]
//! is reserved for the UNAMBIGUOUS case — a 401/403 bearer-auth failure.

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
/// this before any code in this module ever runs. Never constructed HERE
/// (same reason as [`EXIT_OK`]) — documented so the full table reads in
/// one place.
#[allow(dead_code)]
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

/// Resolve the process exit code for a top-level command failure. Walks
/// the WHOLE `anyhow` cause chain (not just the top frame) looking for the
/// underlying `reqwest::Error` — every verb's failure still funnels through
/// [`crate::get_json`]/[`crate::post_json`]/the `*_raw` siblings, each of
/// which wraps the original `reqwest::Error` via `.with_context(...)`
/// (which PRESERVES it as the chain's `source()`, never replaces it), so
/// this needs no per-verb cooperation to work universally.
pub fn exit_code_for(err: &anyhow::Error) -> i32 {
    for cause in err.chain() {
        if let Some(re) = cause.downcast_ref::<reqwest::Error>() {
            if re.is_connect() || re.is_timeout() || (re.is_request() && re.status().is_none()) {
                return EXIT_UNREACHABLE;
            }
            if let Some(status) = re.status() {
                match status.as_u16() {
                    401 | 403 => return EXIT_REFUSED,
                    409 => return EXIT_CONFLICT,
                    _ => {}
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
}
