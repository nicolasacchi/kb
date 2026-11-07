//! Golden tests for the kb-term/1 wire contract: every golden JSON file
//! round-trips byte-identically (deserialize then re-serialize), unknown-field
//! strictness is pinned per side, and the binary frame codec is pinned.

use std::path::{Path, PathBuf};

use kb_term::wire::{
    decode_binary, encode_binary, is_valid_term_id, ClientFrame, Harness, Request, Response,
    ServerFrame, SessionInfo, WireError,
};

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
}

fn read_golden(name: &str) -> Vec<u8> {
    std::fs::read(golden_dir().join(name)).unwrap_or_else(|e| panic!("read golden {name}: {e}"))
}

fn server_goldens() -> Vec<&'static str> {
    vec![
        "sframe_hello.json",
        "sframe_profile.json",
        "sframe_imagedef.json",
        "sframe_seed.json",
        "sframe_resize.json",
        "sframe_sizechanged.json",
        "sframe_gfxreset.json",
        "sframe_exit.json",
        "sframe_error.json",
    ]
}

fn client_goldens() -> Vec<&'static str> {
    vec![
        "cframe_resumefrom.json",
        "cframe_takesize.json",
        "cframe_input.json",
        "cframe_ping.json",
    ]
}

fn request_goldens() -> Vec<&'static str> {
    vec![
        "req_list.json",
        "req_launch.json",
        "req_kill.json",
        "req_resize.json",
        "req_ping.json",
        "req_paste.json",
        "req_enter.json",
        "req_keys.json",
        "req_history.json",
        "req_read.json",
        "req_take_size.json",
        "req_media.json",
    ]
}

#[test]
fn server_frames_match_golden() {
    for name in server_goldens() {
        let bytes = read_golden(name);
        let frame: ServerFrame =
            serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("deserialize {name}: {e}"));
        let out = serde_json::to_vec(&frame).unwrap_or_else(|e| panic!("serialize {name}: {e}"));
        assert_eq!(out, bytes, "server frame golden {name}");
    }
}

#[test]
fn client_frames_match_golden() {
    for name in client_goldens() {
        let bytes = read_golden(name);
        let frame: ClientFrame =
            serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("deserialize {name}: {e}"));
        let out = serde_json::to_vec(&frame).unwrap_or_else(|e| panic!("serialize {name}: {e}"));
        assert_eq!(out, bytes, "client frame golden {name}");
    }
}

#[test]
fn client_frame_rejects_unknown_field() {
    let r: Result<ClientFrame, _> = serde_json::from_str(r#"{"t":"ping","extra":1}"#);
    assert!(r.is_err(), "ping with an extra field must be rejected");
}

#[test]
fn server_frame_ignores_unknown_field() {
    let frame: ServerFrame =
        serde_json::from_str(r#"{"t":"seed","offset":1,"len":2,"future":true}"#)
            .expect("server frame must tolerate unknown fields");
    assert_eq!(frame, ServerFrame::Seed { offset: 1, len: 2 });
}

#[test]
fn runner_requests_match_golden() {
    for name in request_goldens() {
        let bytes = read_golden(name);
        let req: Request =
            serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("deserialize {name}: {e}"));
        let out = serde_json::to_vec(&req).unwrap_or_else(|e| panic!("serialize {name}: {e}"));
        assert_eq!(out, bytes, "runner request golden {name}");
    }
}

#[test]
fn runner_request_rejects_unknown_op() {
    let r: Result<Request, _> = serde_json::from_str(r#"{"op":"bogus"}"#);
    assert!(r.is_err(), "unknown op must be rejected");
}

#[test]
fn runner_request_rejects_unknown_field() {
    let r: Result<Request, _> = serde_json::from_str(r#"{"op":"list","x":1}"#);
    assert!(r.is_err(), "list with an extra field must be rejected");
}

#[test]
fn runner_response_and_session_info_match_golden() {
    for name in ["resp_ok.json", "resp_err.json"] {
        let bytes = read_golden(name);
        let resp: Response =
            serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("deserialize {name}: {e}"));
        let out = serde_json::to_vec(&resp).unwrap_or_else(|e| panic!("serialize {name}: {e}"));
        assert_eq!(out, bytes, "runner response golden {name}");
    }
    let bytes = read_golden("session_info.json");
    let info: SessionInfo = serde_json::from_slice(&bytes).expect("deserialize session_info.json");
    let out = serde_json::to_vec(&info).expect("serialize session_info.json");
    assert_eq!(out, bytes, "session_info golden");
}

#[test]
fn binary_frame_roundtrip() {
    let enc = encode_binary(0x0102030405060708, b"hi");
    assert_eq!(enc, vec![1, 2, 3, 4, 5, 6, 7, 8, b'h', b'i']);
    let (off, payload) = decode_binary(&enc).expect("decode must succeed");
    assert_eq!(off, 0x0102030405060708);
    assert_eq!(payload, b"hi");
    let empty = encode_binary(7, b"");
    let (off, payload) = decode_binary(&empty).expect("empty payload must decode");
    assert_eq!(off, 7);
    assert!(payload.is_empty());
    assert_eq!(
        decode_binary(&[1, 2, 3, 4, 5, 6, 7]),
        Err(WireError::ShortBinaryFrame)
    );
}

#[test]
fn harness_set_is_closed_and_ordered() {
    let names: Vec<&str> = Harness::ALL.iter().map(|h| h.as_str()).collect();
    assert_eq!(
        names,
        vec!["claude", "codex", "opencode", "grok", "kimi", "omp"]
    );
    let r: Result<Harness, _> = serde_json::from_str(r#""glee""#);
    assert!(
        r.is_err(),
        "a seventh harness name must fail to deserialize"
    );
}

#[test]
fn term_id_charset() {
    assert!(is_valid_term_id("abcd1234"));
    assert!(is_valid_term_id("abcd1234abcd"));
    assert!(!is_valid_term_id("abcd123"));
    assert!(!is_valid_term_id("abcd1234abcd1"));
    assert!(!is_valid_term_id("ABCD1234"));
    assert!(!is_valid_term_id("abcd-123"));
    assert!(!is_valid_term_id("abcd.123"));
    assert!(!is_valid_term_id("abcd/123"));
    assert!(!is_valid_term_id(""));
}

#[test]
fn golden_files_are_complete() {
    let mut expected: Vec<String> = server_goldens()
        .into_iter()
        .chain(client_goldens())
        .chain(request_goldens())
        .chain(["resp_ok.json", "resp_err.json", "session_info.json"])
        .map(str::to_string)
        .collect();
    expected.sort();
    let mut actual: Vec<String> = std::fs::read_dir(golden_dir())
        .unwrap_or_else(|e| panic!("list golden dir: {e}"))
        .map(|e| {
            e.unwrap_or_else(|e| panic!("golden dir entry: {e}"))
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    actual.sort();
    assert_eq!(
        actual, expected,
        "golden directory must match the pinned set"
    );
}
