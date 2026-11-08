//! Golden tests for the kb-term/1 wire contract: every golden JSON file
//! round-trips byte-identically (deserialize then re-serialize), unknown-field
//! strictness is pinned per side, and the binary frame codec is pinned.

use std::path::{Path, PathBuf};

use kb_term::wire::{
    decode_binary, decode_text_frame, encode_binary, encode_envelope, is_valid_term_id,
    parse_s2c_body, s2c_body, ClientFrame, ControlMessage, EnvelopeDecoder, Harness, Profile,
    Request, Response, ResumeInfo, ServerFrame, SessionInfo, TextFrame, WireError,
    MAX_ENVELOPE_LEN,
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
        "sframe_clipboard.json",
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

fn control_goldens() -> Vec<&'static str> {
    vec![
        "ctrl_viewer_open.json",
        "ctrl_viewer_close.json",
        "ctrl_typed.json",
        "ctrl_stats.json",
    ]
}

fn envelope_goldens() -> Vec<&'static str> {
    vec!["env_ssc_text.hex", "env_ssc_binary.hex", "env_control.hex"]
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
        "req_release_size.json",
        "req_grid.json",
        "req_viewer_open.json",
        "req_viewer_close.json",
        "req_media.json",
        "req_stream.json",
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
fn rpc_wire_goldens() {
    // The T9 request variants round-trip byte-identically against the
    // committed goldens, and every request rejects unknown fields.
    for name in [
        "req_paste.json",
        "req_enter.json",
        "req_keys.json",
        "req_take_size.json",
        "req_release_size.json",
        "req_grid.json",
        "req_viewer_open.json",
        "req_viewer_close.json",
    ] {
        let bytes = read_golden(name);
        let req: Request =
            serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("deserialize {name}: {e}"));
        let out = serde_json::to_vec(&req).unwrap_or_else(|e| panic!("serialize {name}: {e}"));
        assert_eq!(out, bytes, "runner request golden {name}");
    }
    for bad in [
        r#"{"op":"paste","id":"abcd1234","viewer":"web","text":"x","extra":1}"#,
        r#"{"op":"enter","id":"abcd1234","viewer":"web","blocked":false,"from_composer":true,"extra":1}"#,
        r#"{"op":"keys","id":"abcd1234","viewer":"web","input":{"kind":"named","named":["C-c"]},"extra":1}"#,
        r#"{"op":"keys","id":"abcd1234","viewer":"web","input":{"kind":"named","named":["C-c"],"extra":1}}"#,
        r#"{"op":"take_size","id":"abcd1234","cols":120,"rows":40,"owner":"a","force":false,"extra":1}"#,
        r#"{"op":"viewer_open","id":"abcd1234","viewer":"web","extra":1}"#,
        r#"{"op":"grid","id":"abcd1234","extra":1}"#,
    ] {
        let r: Result<Request, _> = serde_json::from_str(bad);
        assert!(r.is_err(), "unknown field must be rejected: {bad}");
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
    // 0x0002030405060708 < 2^53, so the decode bound does not fire.
    let enc = encode_binary(0x0002030405060708, b"hi");
    assert_eq!(enc, vec![0, 2, 3, 4, 5, 6, 7, 8, b'h', b'i']);
    let (off, payload) = decode_binary(&enc).expect("decode must succeed");
    assert_eq!(off, 0x0002030405060708);
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
fn decode_binary_rejects_offset_at_2pow53() {
    let mut frame = (1u64 << 53).to_be_bytes().to_vec();
    assert_eq!(decode_binary(&frame), Err(WireError::OffsetOutOfRange));
    frame.extend_from_slice(b"x");
    assert_eq!(decode_binary(&frame), Err(WireError::OffsetOutOfRange));
    // 2^53 - 1 is the largest legal offset.
    let ok = ((1u64 << 53) - 1).to_be_bytes();
    let (off, payload) = decode_binary(&ok).expect("2^53 - 1 must decode");
    assert_eq!(off, (1u64 << 53) - 1);
    assert!(payload.is_empty());
}

#[test]
fn control_messages_match_golden() {
    for name in control_goldens() {
        let bytes = read_golden(name);
        let msg: ControlMessage =
            serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("deserialize {name}: {e}"));
        let out = serde_json::to_vec(&msg).unwrap_or_else(|e| panic!("serialize {name}: {e}"));
        assert_eq!(out, bytes, "control message golden {name}");
    }
}

#[test]
fn envelope_goldens_match() {
    for name in envelope_goldens() {
        let raw = read_golden(name);
        let hex = std::str::from_utf8(&raw)
            .expect("envelope golden must be utf8 hex")
            .trim();
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex pair"))
            .collect();
        let mut dec = EnvelopeDecoder::new();
        let envs = dec.push(&bytes).expect("envelope golden must decode");
        assert_eq!(envs.len(), 1, "envelope golden {name} is one frame");
        let re = encode_envelope(envs[0].kind, &envs[0].body).expect("re-encode");
        assert_eq!(re, bytes, "envelope golden {name} round-trips");
    }
}

#[test]
fn envelope_codec_roundtrip_and_split_streaming() {
    let body = s2c_body("v1", b"{\"t\":\"ping\"}").expect("s2c body");
    let (id, frame) = parse_s2c_body(&body).expect("parse s2c body");
    assert_eq!(id, "v1");
    assert_eq!(frame, b"{\"t\":\"ping\"}");
    let mut buf = encode_envelope(0, &body).expect("encode");
    buf.extend_from_slice(&encode_envelope(2, b"{\"t\":\"stats\"}").expect("encode"));
    let mut dec = EnvelopeDecoder::new();
    assert!(dec.push(&buf[..3]).expect("partial prefix").is_empty());
    let rest = dec.push(&buf[3..]).expect("rest");
    assert_eq!(rest.len(), 2);
    assert_eq!(rest[0].kind, 0);
    assert_eq!(rest[1].kind, 2);
    assert_eq!(rest[1].body, b"{\"t\":\"stats\"}");
}

#[test]
fn envelope_negatives() {
    assert_eq!(encode_envelope(9, b"x"), Err(WireError::BadEnvelope));
    assert_eq!(
        s2c_body(&"a".to_string().repeat(300), b""),
        Err(WireError::BadEnvelope)
    );
    assert_eq!(parse_s2c_body(&[]), Err(WireError::BadEnvelope));
    assert_eq!(parse_s2c_body(&[5, b'a']), Err(WireError::BadEnvelope));
    let mut dec = EnvelopeDecoder::new();
    assert_eq!(dec.push(&[0, 0, 0, 0, 0]), Err(WireError::BadEnvelope));
    let mut dec2 = EnvelopeDecoder::new();
    assert_eq!(dec2.push(&[0, 0, 0, 2, 9, 65]), Err(WireError::BadEnvelope));
}

#[test]
fn decode_text_frame_handles_unknown_and_malformed() {
    let raw = r#"{"t":"future-kind","x":[1,2,3]}"#;
    match decode_text_frame(raw).expect("unknown must decode") {
        TextFrame::Unknown(s) => assert_eq!(s, raw),
        other => panic!("unknown t must be Unknown, got {other:?}"),
    }
    assert_eq!(decode_text_frame("{"), Err(WireError::MalformedJson));
    assert_eq!(decode_text_frame("not json"), Err(WireError::MalformedJson));
    assert_eq!(decode_text_frame("[1,2,3]"), Err(WireError::MalformedJson));
    assert_eq!(
        decode_text_frame(r#"{"x":1}"#),
        Err(WireError::MalformedJson)
    );
    // Known variants route to their enums.
    match decode_text_frame(r#"{"t":"ping"}"#).expect("ping") {
        TextFrame::Client(ClientFrame::Ping {}) => {}
        other => panic!("ping must be ClientFrame::Ping, got {other:?}"),
    }
    let open = serde_json::to_value(ControlMessage::ViewerOpen {
        viewer: "v1".to_string(),
        profile: Profile::Desktop,
        resume: Some(ResumeInfo {
            offset: 5,
            mseq: 2,
            epoch: "ab".to_string(),
        }),
    })
    .expect("to value");
    match decode_text_frame(&serde_json::to_string(&open).expect("to string")).expect("open") {
        TextFrame::Control(ControlMessage::ViewerOpen { viewer, resume, .. }) => {
            assert_eq!(viewer, "v1");
            assert_eq!(resume.unwrap().offset, 5);
        }
        other => panic!("open must route, got {other:?}"),
    }
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
        .chain(control_goldens())
        .chain(envelope_goldens())
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

#[test]
fn envelope_length_capped_at_max_envelope_len() {
    // Boundary: a body of MAX_ENVELOPE_LEN - 1 bytes (length == the cap) is fine.
    let ok = vec![0u8; MAX_ENVELOPE_LEN - 1];
    let env = encode_envelope(0, &ok).expect("boundary envelope must encode");
    assert_eq!(env.len(), 4 + MAX_ENVELOPE_LEN);
    // One byte more fails the encoder.
    let too_big = vec![0u8; MAX_ENVELOPE_LEN];
    assert_eq!(encode_envelope(0, &too_big), Err(WireError::BadEnvelope));
    // A decoder fed only a header claiming MAX+1 fails immediately,
    // without waiting for the body.
    let mut dec = EnvelopeDecoder::new();
    let hdr = (MAX_ENVELOPE_LEN as u32 + 1).to_be_bytes();
    assert_eq!(dec.push(&hdr), Err(WireError::BadEnvelope));
    // The boundary envelope split across pushes still decodes.
    let mut dec2 = EnvelopeDecoder::new();
    assert!(dec2.push(&env[..10]).expect("partial").is_empty());
    let done = dec2.push(&env[10..]).expect("rest");
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].body.len(), MAX_ENVELOPE_LEN - 1);
}

#[test]
fn decode_text_frame_rejects_missing_required_fields() {
    // Lock-step with the TS decoder: a known `t` without its required
    // fields is MalformedJson, never a half-populated frame.
    assert_eq!(
        decode_text_frame(r#"{"t":"seed","offset":1000}"#),
        Err(WireError::MalformedJson)
    );
    assert_eq!(
        decode_text_frame(r#"{"t":"hello","proto":"kb-term/1","session":"s"}"#),
        Err(WireError::MalformedJson)
    );
    assert_eq!(
        decode_text_frame(r#"{"t":"resumefrom","offset":1000,"epoch":"e"}"#),
        Err(WireError::MalformedJson)
    );
}

#[test]
fn decode_text_frame_rejects_offsets_at_2pow53() {
    // Lock-step with the TS `reqUint` cap (Number.MAX_SAFE_INTEGER): text
    // u64 offsets/seqs at or above 2^53 are OffsetOutOfRange on both sides.
    // 2^53 - 1 is the largest legal value and still decodes.
    assert!(
        decode_text_frame(r#"{"t":"seed","offset":9007199254740991,"len":1}"#).is_ok(),
        "2^53 - 1 must decode"
    );
    assert_eq!(
        decode_text_frame(r#"{"t":"seed","offset":9007199254740992,"len":1}"#),
        Err(WireError::OffsetOutOfRange)
    );
    assert_eq!(
        decode_text_frame(r#"{"t":"seed","offset":0,"len":9007199254740992}"#),
        Err(WireError::OffsetOutOfRange)
    );
    assert_eq!(
        decode_text_frame(r#"{"t":"resumefrom","offset":0,"mseq":9007199254740992,"epoch":"e"}"#),
        Err(WireError::OffsetOutOfRange)
    );
    assert_eq!(
        decode_text_frame(
            r#"{"t":"viewer-open","viewer":"v1","profile":"desktop","resume":{"offset":9007199254740992,"mseq":0,"epoch":"e"}}"#
        ),
        Err(WireError::OffsetOutOfRange)
    );
    assert_eq!(
        decode_text_frame(r#"{"t":"clipboard","offset":9007199254740992,"data_b64":""}"#),
        Err(WireError::OffsetOutOfRange)
    );
}
