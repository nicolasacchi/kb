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
//!
//! Post-merge reconciliation (FXT8): this mirror deliberately crosses the
//! kb-server boundary that T8's brief kept closed — the relay needs the
//! shared wire shapes to type its JSON, so the mirror is justified, kept
//! in lock-step by the golden round-trip below (not a second contract).

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
        /// Grid columns (`None` means the runner's default).
        #[serde(default)]
        cols: Option<u16>,
        /// Grid rows (`None` means the runner's default).
        #[serde(default)]
        rows: Option<u16>,
        /// Extra environment for the harness pane.
        #[serde(default)]
        env: Option<std::collections::BTreeMap<String, String>>,
        /// Exit keys used by the kill RPC.
        #[serde(default)]
        exit_keys: Option<Vec<String>>,
    },
    /// Kill a session.
    Kill {
        /// Session id.
        id: String,
        /// Grace in seconds before the forced path runs.
        #[serde(default)]
        grace_secs: Option<u64>,
    },
    /// Paste text into a session.
    Paste {
        /// Session id.
        id: String,
        /// Viewer the paste is attributed to.
        viewer: String,
        /// Text to paste.
        text: String,
    },
    /// Enter on a viewer's hidden client (caller supplies beat state).
    Enter {
        /// Session id.
        id: String,
        /// Viewer.
        viewer: String,
        /// Currently blocked.
        blocked: bool,
        /// The Enter comes from the composer.
        from_composer: bool,
    },
    /// Send input to a viewer's hidden client.
    Keys {
        /// Session id.
        id: String,
        /// Viewer.
        viewer: String,
        /// Input payload.
        input: KeyInput,
    },
    /// Resize the session grid.
    Resize {
        /// Session id.
        id: String,
        /// Grid columns.
        cols: u16,
        /// Grid rows.
        rows: u16,
    },
    /// Liveness probe.
    Ping {},
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
    /// Take the grid size for a session.
    TakeSize {
        /// Session id.
        id: String,
        /// Grid columns.
        cols: u16,
        /// Grid rows.
        rows: u16,
        /// Who is taking the grid.
        owner: String,
        /// Override a foreign owner.
        #[serde(default)]
        force: bool,
    },
    /// Release the grid size for a session.
    ReleaseSize {
        /// Session id.
        id: String,
        /// The owner releasing the grid.
        owner: String,
    },
    /// Report the grid size and owner for a session.
    Grid {
        /// Session id.
        id: String,
    },
    /// Open a hidden input client for a viewer.
    ViewerOpen {
        /// Session id.
        id: String,
        /// Viewer id.
        viewer: String,
    },
    /// Close a viewer's hidden client.
    ViewerClose {
        /// Session id.
        id: String,
        /// Viewer id.
        viewer: String,
    },
    /// Fetch media by sha256.
    Media {
        /// sha256 of the media bytes.
        sha256: String,
    },
    /// Open a byte-stream relay to a session's tap (the WS upstream).
    /// The server answers the request line with `RunnerReply::Ok` and
    /// then splices tap.sock envelopes on the same connection.
    Stream {
        /// Session id.
        id: String,
        /// Resume offset, if any.
        #[serde(default)]
        from: Option<u64>,
    },
}

/// Key input for the `keys` RPC (kind-tagged; unknown fields rejected).
/// Mirrors `kb-term`'s `KeyInput`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum KeyInput {
    /// Raw bytes, base64-encoded (written verbatim).
    Bytes {
        /// Base64 of the bytes.
        bytes_b64: String,
    },
    /// Named keys from the closed table.
    Named {
        /// Key names.
        named: Vec<String>,
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
///
/// Every fact is `Option` and serializes `None` as an explicit `null`,
/// exactly like kb-term's `wire::SessionInfo`: a stale socket appears
/// with all facts null and `stale: true`. One type covers both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    /// Session id.
    pub id: String,
    /// Harness running in the session.
    #[serde(default)]
    pub harness: Option<Harness>,
    /// Working directory.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Start time as unix seconds.
    #[serde(default)]
    pub started_unix: Option<i64>,
    /// Resume session id, if any.
    pub resume_sid: Option<String>,
    /// Pane pid.
    #[serde(default)]
    pub pane_pid: Option<u32>,
    /// Grid columns.
    #[serde(default)]
    pub cols: Option<u16>,
    /// Grid rows.
    #[serde(default)]
    pub rows: Option<u16>,
    /// Whether bracketed paste is enabled in the pane.
    pub bracket_paste_flag: bool,
    /// Whether the pane is dead.
    pub pane_dead: bool,
    /// Exit code, if any.
    pub exit_code: Option<i32>,
    /// Grid owner, if any.
    pub grid_owner: Option<String>,
    /// `kb-term.conf` version the holder loaded.
    #[serde(default)]
    pub conf_version: u32,
    /// The anchor client pid, if any.
    #[serde(default)]
    pub anchor_pid: Option<u32>,
    /// Whether the holder socket did not answer (stale).
    #[serde(default)]
    pub stale: bool,
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
                "req_stream",
                include_str!("../../../kb-term/tests/golden/req_stream.json"),
            ),
            (
                "req_take_size",
                include_str!("../../../kb-term/tests/golden/req_take_size.json"),
            ),
            (
                "req_viewer_open",
                include_str!("../../../kb-term/tests/golden/req_viewer_open.json"),
            ),
            (
                "req_viewer_close",
                include_str!("../../../kb-term/tests/golden/req_viewer_close.json"),
            ),
            (
                "req_release_size",
                include_str!("../../../kb-term/tests/golden/req_release_size.json"),
            ),
            (
                "req_resize",
                include_str!("../../../kb-term/tests/golden/req_resize.json"),
            ),
            (
                "req_grid",
                include_str!("../../../kb-term/tests/golden/req_grid.json"),
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
    /// A launch request that omits `cols`/`rows` parses as `None`
    /// (the runner's 120x40 default), and serializes both back as
    /// explicit `null` — the mirror shape of the upstream
    /// `Option<u16>` + `#[serde(default)]`. The golden
    /// (`req_launch.json`, 120/40) still round-trips as `Some`.
    #[test]
    fn launch_size_may_be_omitted() {
        let req: RunnerRequest =
            serde_json::from_str("{\"op\":\"launch\",\"harness\":\"omp\",\"cwd\":\"/work/proj\"}")
                .unwrap();
        match req {
            RunnerRequest::Launch {
                cols,
                rows,
                argv,
                resume_sid,
                env,
                exit_keys,
                ..
            } => {
                assert_eq!(cols, None);
                assert_eq!(rows, None);
                assert_eq!(argv, None);
                assert_eq!(resume_sid, None);
                assert_eq!(env, None);
                assert_eq!(exit_keys, None);
            }
            other => panic!("must be launch, got {other:?}"),
        }
        let out = serde_json::to_string(&RunnerRequest::Launch {
            harness: Harness::Omp,
            argv: None,
            resume_sid: None,
            cwd: "/work/proj".to_string(),
            cols: None,
            rows: None,
            env: None,
            exit_keys: None,
        })
        .unwrap();
        assert!(out.contains(r#""cols":null"#), "{out}");
        assert!(out.contains(r#""rows":null"#), "{out}");
        let expected = RunnerRequest::Launch {
            harness: Harness::Omp,
            argv: None,
            resume_sid: None,
            cwd: "/work/proj".to_string(),
            cols: None,
            rows: None,
            env: None,
            exit_keys: None,
        };
        let back: RunnerRequest = serde_json::from_str(&out).unwrap();
        assert_eq!(back, expected);
    }

    /// The runner's `list` reply `data` is an array of `SessionInfo`s
    /// (stale sockets included with null facts and `stale: true`), and
    /// this mirror must deserialize that array.
    #[test]
    fn list_data_array_parses_as_session_infos() {
        let golden = include_str!("../../../kb-term/tests/golden/session_info.json").trim_end();
        let reply: RunnerReply =
            serde_json::from_str(&format!("{{\"status\":\"ok\",\"data\":[{}]}}", golden)).unwrap();
        match &reply {
            RunnerReply::Ok { data } => {
                let infos: Vec<SessionInfo> = serde_json::from_value(data.clone()).unwrap();
                assert_eq!(infos.len(), 1);
                assert_eq!(infos[0].id, "abcd1234");
                assert_eq!(infos[0].harness, Some(Harness::Omp));
            }
            RunnerReply::Err { .. } => panic!("must not err"),
        }
        let stale_json = r#"{"id":"deadbeef","harness":null,"cwd":null,"started_unix":null,"resume_sid":null,"pane_pid":null,"cols":null,"rows":null,"bracket_paste_flag":false,"pane_dead":false,"exit_code":null,"grid_owner":null,"conf_version":0,"anchor_pid":null,"stale":true}"#;
        let stale: SessionInfo = serde_json::from_str(stale_json).unwrap();
        assert!(stale.stale);
        assert!(stale.harness.is_none() && stale.pane_pid.is_none());
        assert_eq!(
            serde_json::to_string(&stale).unwrap(),
            stale_json,
            "stale SessionInfo must round-trip byte-identically (explicit nulls)"
        );
    }
}
