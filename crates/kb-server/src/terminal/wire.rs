//! A serde **mirror** of the runner RPC types (`kb-term`'s `wire` module,
//! crate `kb-term`).
//!
//! kb-server talks to `kb-termd` over the unix runner socket; it must NOT
//! depend on the `kb-term` crate (the lance graph stays out of kb-term,
//! and kb-server must not grow a kb-term edge for below-the-API types), so
//! the JSON shapes are spelled out here and pinned against the upstream
//! goldens by [`terms_runner_wire_matches_golden`].
//!
//! The merged upstream contract (`crates/kb-term/src/wire.rs`, pinned by
//! `crates/kb-term/tests/golden/*.json`) wins over any proposed
//! spelling: requests are `{"op": <snake_case>, ...}` with unknown
//! fields rejected (`deny_unknown_fields`), replies are
//! `{"status":"ok","data":...}` / `{"status":"err","code","message"}` with
//! unknown fields tolerated (additive-reader rule), and every field
//! serializes `None` as an explicit `null` (no `skip_serializing_if`).

use serde::{Deserialize, Serialize};

/// A harness known to the terminal substrate (closed set). Mirrors
/// `kb_core::sessions::HARNESSES` (`sessions/constants.rs`, `["claude",
/// "codex", "opencode", "grok", "kimi", "omp"]`) without a dependency
/// edge; the runner validates the name against the same constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Harness {
    /// Claude Code.
    Claude,
    /// Codex.
    Codex,
    /// OpenCode.
    Opencode,
    /// Grok.
    Grok,
    /// Kimi.
    Kimi,
    /// Omp.
    Omp,
}

impl Harness {
    /// Every harness in wire order.
    pub const ALL: [Harness; 6] = [
        Harness::Claude,
        Harness::Codex,
        Harness::Opencode,
        Harness::Grok,
        Harness::Kimi,
        Harness::Omp,
    ];

    /// The lowercase wire name of this harness.
    pub fn as_str(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Codex => "codex",
            Harness::Opencode => "opencode",
            Harness::Grok => "grok",
            Harness::Kimi => "kimi",
            Harness::Omp => "omp",
        }
    }
}

/// Runner RPC request over the runner socket (tag `op`, snake_case).
/// Unknown fields are rejected so a misspelled field never silently
/// drops.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunnerRequest {
    /// List sessions.
    List {},
    /// Launch a new terminal session.
    Launch {
        /// Harness to run.
        harness: Harness,
        /// Extra argv, if any.
        argv: Option<Vec<String>>,
        /// Resume session id, if any.
        resume_sid: Option<String>,
        /// Working directory.
        cwd: String,
        /// Grid columns.
        cols: u16,
        /// Grid rows.
        rows: u16,
    },
    /// Kill a session.
    Kill {
        /// Session id.
        id: String,
    },
    /// Paste text into a session.
    Paste {
        /// Session id.
        id: String,
        /// Text to paste.
        text: String,
    },
    /// Mark a session as entered (caller supplies the beat-derived flag).
    Enter {
        /// Session id.
        id: String,
        /// Currently blocked.
        blocked: bool,
    },
    /// Send keys to a session.
    Keys {
        /// Session id.
        id: String,
        /// Key names.
        keys: Vec<String>,
    },
    /// Read session history.
    History {
        /// Session id.
        id: String,
    },
    /// Read a session.
    Read {
        /// Session id.
        id: String,
    },
    /// Take (or report) the grid size for a session.
    TakeSize {
        /// Session id.
        id: String,
        /// Grid columns.
        cols: u16,
        /// Grid rows.
        rows: u16,
        /// Who is taking the grid.
        by: String,
    },
}

/// Runner RPC response over the runner socket (tag `status`, lowercase).
/// Unknown fields are tolerated on read so a newer runner stays
/// readable by an older kb-server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum RunnerReply {
    /// Success with a payload. `List` puts a `SessionInfo` array in
    /// `data`.
    Ok {
        /// Response payload.
        data: serde_json::Value,
    },
    /// Failure.
    Err {
        /// Machine-readable code.
        code: String,
        /// Human-readable message.
        message: String,
    },
}

/// Session facts returned by `list` inside `RunnerReply::Ok` `data`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    /// Session id.
    pub id: String,
    /// Harness running in the session.
    pub harness: Harness,
    /// Working directory.
    pub cwd: String,
    /// Start time as unix seconds.
    pub started_unix: i64,
    /// Resume session id, if any.
    pub resume_sid: Option<String>,
    /// Pane pid.
    pub pane_pid: u32,
    /// Grid columns.
    pub cols: u16,
    /// Grid rows.
    pub rows: u16,
    /// Whether bracketed paste is enabled in the pane.
    pub bracket_paste_flag: bool,
    /// Whether the pane is dead.
    pub pane_dead: bool,
    /// Exit code, if any.
    pub exit_code: Option<i32>,
    /// Grid owner, if any.
    pub grid_owner: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The upstream T1 goldens, read from `crates/kb-term/tests/golden/`
    /// with `include_str!` so a drift in the merged contract
    /// (`crates/kb-term/src/wire.rs` / the golden files) fails this
    /// mirror test. `include_str!` resolves relative to this file, so
    /// `../../../kb-term/...` reaches it.
    fn golden_samples() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "req_list",
                include_str!("../../../kb-term/tests/golden/req_list.json"),
            ),
            (
                "req_launch",
                include_str!("../../../kb-term/tests/golden/req_launch.json"),
            ),
            (
                "req_kill",
                include_str!("../../../kb-term/tests/golden/req_kill.json"),
            ),
            (
                "req_paste",
                include_str!("../../../kb-term/tests/golden/req_paste.json"),
            ),
            (
                "req_enter",
                include_str!("../../../kb-term/tests/golden/req_enter.json"),
            ),
            (
                "req_keys",
                include_str!("../../../kb-term/tests/golden/req_keys.json"),
            ),
            (
                "req_read",
                include_str!("../../../kb-term/tests/golden/req_read.json"),
            ),
            (
                "req_history",
                include_str!("../../../kb-term/tests/golden/req_history.json"),
            ),
            (
                "req_take_size",
                include_str!("../../../kb-term/tests/golden/req_take_size.json"),
            ),
            (
                "resp_ok",
                include_str!("../../../kb-term/tests/golden/resp_ok.json"),
            ),
            (
                "resp_err",
                include_str!("../../../kb-term/tests/golden/resp_err.json"),
            ),
            (
                "session_info",
                include_str!("../../../kb-term/tests/golden/session_info.json"),
            ),
        ]
    }

    #[test]
    fn terms_runner_wire_matches_golden() {
        for (name, golden) in golden_samples() {
            // Drop a final newline the file may carry; the comparison
            // is against the JSON body itself (the upstream files
            // today carry none).
            let golden = golden.trim_end();
            // Byte-identical round-trip: deserialize, re-serialize, and
            // compare with the golden bytes (the upstream contract
            // serializes `None` as an explicit `null` and keeps field
            // order).
            if name.starts_with("req_") {
                let req: RunnerRequest =
                    serde_json::from_str(golden).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert_eq!(&serde_json::to_string(&req).unwrap(), golden, "{name}");
            } else if name.starts_with("resp_") {
                let resp: RunnerReply =
                    serde_json::from_str(golden).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert_eq!(&serde_json::to_string(&resp).unwrap(), golden, "{name}");
            } else {
                let info: SessionInfo =
                    serde_json::from_str(golden).unwrap_or_else(|e| panic!("{name}: {e}"));
                assert_eq!(&serde_json::to_string(&info).unwrap(), golden, "{name}");
            }
        }
        // The guard against a silent schema drift: this mirror must
        // accept exactly the upstream golden shapes above.
        assert_eq!(
            Harness::ALL.len(),
            kb_core::sessions::HARNESSES.len(),
            "this mirror's closed harness set must track kb_core's constant"
        );
        for (i, h) in Harness::ALL.iter().enumerate() {
            assert_eq!(
                h.as_str(),
                kb_core::sessions::HARNESSES[i],
                "harness {i} must line up with kb_core::sessions::HARNESSES"
            );
        }
    }

    #[test]
    fn unknown_reply_fields_are_tolerated() {
        // The additive-reader rule: a newer runner may add fields to a
        // reply and this mirror must still parse it.
        let reply: RunnerReply =
            serde_json::from_str("{\"status\":\"ok\",\"data\":{\"n\":1},\"future_field\":true}")
                .unwrap();
        match reply {
            RunnerReply::Ok { data } => assert_eq!(data["n"], 1),
            RunnerReply::Err { .. } => panic!("must not err"),
        }
        // Unknown request fields are NOT tolerated (deny_unknown_fields),
        // so a typo never silently drops.
        assert!(serde_json::from_str::<RunnerRequest>(
            "{\"op\":\"kill\",\"id\":\"abcd1234\",\"future_field\":true}"
        )
        .is_err());
    }
}
