//! Low-rate `term.*` SSE kinds (T12).
//!
//! Three kinds, all daemon-wide through the `EventBus` like
//! `session.state`: `term.created` (launch confirmed), `term.ended`
//! (exactly once per id, from the kill handler or from an `exit` text
//! frame passing through the WS relay), `term.state` (only on
//! `sizechanged` and on `ended` — never per data frame).
//!
//! The dedup set for `term.ended` lives in memory (`TerminalState`)
//! behind a sync lock never held across an await (invariant #15); a
//! lost entry only risks an idempotent duplicate, which the capped
//! clear accepts.
//!
//! The drift scanner (`routes::schema::drift`) requires every
//! `.emit("<literal>", …)` first argument to be declared in
//! `V0_0_1_TYPES` with a matching `per_type` arm — the literals below
//! are that declaration's counterpart, in the same commit.

use std::collections::HashSet;
use std::sync::Mutex;

use crate::state::KbHandles;

/// Cap on the ended-id dedup set. Over the cap the oldest entries are
/// dropped wholesale (a lost entry only risks an idempotent
/// duplicate); ids are 8..=12 chars, so 1024 entries is a few KiB.
pub(crate) const ENDED_SET_CAP: usize = 1024;

/// The `term.ended` dedup set (id -> ended). Logic: `true` when the
/// caller is the first to report this id ended (and must emit).
pub(crate) fn ended_claim(state: &KbHandles, id: &str) -> bool {
    let mut guard = state
        .terminals
        .ended
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if guard.contains(id) {
        return false;
    }
    if guard.len() >= ENDED_SET_CAP {
        guard.clear();
    }
    guard.insert(id.to_string());
    true
}

/// Convenience for `HashSet`-typed state fields (kept tiny: the set is
/// just the dedup table).
pub(crate) type EndedSet = Mutex<HashSet<String>>;

/// The `term.created` payload `{id, harness, cols, rows}`.
pub(crate) fn term_created_payload(
    id: &str,
    harness: &str,
    cols: u16,
    rows: u16,
) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "harness": harness,
        "cols": cols,
        "rows": rows,
    })
}

/// The `term.ended` payload `{id, exit_code?}` — `exit_code` is
/// present only when the runner/tap supplied one.
pub(crate) fn term_ended_payload(id: &str, exit_code: Option<i32>) -> serde_json::Value {
    let mut v = serde_json::json!({ "id": id });
    if let Some(code) = exit_code {
        v["exit_code"] = serde_json::json!(code);
    }
    v
}

/// The `term.state` payload `{id, state, cols?, rows?, grid_owner?}` —
/// `state` is `"live"` or `"ended"`. Emitted only when a `sizechanged`
/// text frame passes through, or on `ended`.
pub(crate) fn term_state_payload(
    id: &str,
    state: &str,
    cols: Option<u16>,
    rows: Option<u16>,
    grid_owner: Option<&str>,
) -> serde_json::Value {
    let mut v = serde_json::json!({ "id": id, "state": state });
    if let Some(c) = cols {
        v["cols"] = serde_json::json!(c);
    }
    if let Some(r) = rows {
        v["rows"] = serde_json::json!(r);
    }
    if let Some(owner) = grid_owner {
        v["grid_owner"] = serde_json::json!(owner);
    }
    v
}

/// Emit `term.created` for a launch the runner confirmed.
pub(crate) fn emit_created(state: &KbHandles, id: &str, harness: &str, cols: u16, rows: u16) {
    state.bus.emit(
        "term.created",
        term_created_payload(id, harness, cols, rows),
    );
}

/// Emit `term.ended` exactly once per id (the dedup claim is the
/// gate: a duplicate claim emits nothing). Returns whether it emitted.
pub(crate) fn emit_ended(state: &KbHandles, id: &str, exit_code: Option<i32>) -> bool {
    if !ended_claim(state, id) {
        return false;
    }
    state
        .bus
        .emit("term.ended", term_ended_payload(id, exit_code));
    true
}

/// Emit `term.state` (live on `sizechanged`, ended on `term.ended`).
pub(crate) fn emit_state(
    state: &KbHandles,
    id: &str,
    live: bool,
    cols: Option<u16>,
    rows: Option<u16>,
    grid_owner: Option<&str>,
) {
    let state = if live { "live" } else { "ended" };
    state.bus.emit(
        "term.state",
        term_state_payload(id, state, cols, rows, grid_owner),
    );
}

#[cfg(test)]
mod payload_tests {
    //! The payload builders are pure and pinned against the key consts
    //! in `routes::schema` (the `session_state_payload` tie test's
    //! shape): keys == consts modulo `?`.
    use super::*;
    use crate::routes::schema::{TERM_CREATED_KEYS, TERM_ENDED_KEYS, TERM_STATE_KEYS};

    fn keys_of(v: &serde_json::Value) -> Vec<String> {
        v.as_object()
            .expect("payload is an object")
            .keys()
            .cloned()
            .collect()
    }

    fn declared(consts: &[&str]) -> Vec<String> {
        consts
            .iter()
            .map(|s| s.trim_end_matches('?').to_string())
            .collect()
    }

    #[test]
    fn term_event_keys_match_emitters() {
        // term.created: every key always present.
        let created = term_created_payload("abcd1234", "omp", 120, 40);
        let mut got = keys_of(&created);
        let mut want = declared(&TERM_CREATED_KEYS);
        got.sort();
        want.sort();
        assert_eq!(got, want, "term.created keys must match the schema consts");

        // term.ended: exit_code is optional (`?`).
        let ended_with = term_ended_payload("abcd1234", Some(1));
        let ended_without = term_ended_payload("abcd1234", None);
        let mut got_with = keys_of(&ended_with);
        let mut got_without = keys_of(&ended_without);
        let mut want = declared(&TERM_ENDED_KEYS);
        got_with.sort();
        got_without.sort();
        want.sort();
        assert_eq!(got_with, want, "term.ended with exit_code");
        assert_eq!(
            got_without,
            vec!["id".to_string()],
            "term.ended without exit_code carries only id"
        );

        // term.state: cols/rows/grid_owner optional.
        let live = term_state_payload("abcd1234", "live", Some(120), Some(40), Some("web"));
        let mut got = keys_of(&live);
        let mut want = declared(&TERM_STATE_KEYS);
        got.sort();
        want.sort();
        assert_eq!(got, want, "term.state keys must match the schema consts");
        assert_eq!(live["state"], "live");
        let ended = term_state_payload("abcd1234", "ended", None, None, None);
        assert_eq!(keys_of(&ended), vec!["id".to_string(), "state".to_string()]);
        assert_eq!(ended["state"], "ended");
    }
}
