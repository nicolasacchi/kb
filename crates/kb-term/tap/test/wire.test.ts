// Wire goldens (T7 test 2): every golden in the SHARED directory round-trips
// byte-identically (decode -> re-encode). The Rust side iterates the same
// directory in tests/wire_golden.rs, so both languages agree byte for byte.
import { test, expect } from 'bun:test';
import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import {
  EnvelopeDecoder,
  decodeBinaryFrame,
  decodeTextFrame,
  encodeBinaryFrame,
  encodeClientFrame,
  encodeControlMessage,
  encodeEnvelope,
  encodeServerFrame,
  encodeStatsResponse,
  ENV_BINARY,
  ENV_CONTROL,
  ENV_TEXT,
  FrameError,
  type ServerFrame,
} from '../src/wire.ts';

const GOLDEN = join(import.meta.dir, '../../tests/golden');
const enc = (s: string) => new TextEncoder().encode(s);

function goldenBytes(name: string): Uint8Array {
  return readFileSync(join(GOLDEN, name));
}

const SERVER = ['sframe_hello', 'sframe_profile', 'sframe_imagedef', 'sframe_seed', 'sframe_resize', 'sframe_sizechanged', 'sframe_gfxreset', 'sframe_exit', 'sframe_clipboard', 'sframe_error'];
const CLIENT = ['cframe_resumefrom', 'cframe_takesize', 'cframe_input', 'cframe_ping'];
const ENVELOPE = ['env_ssc_text', 'env_ssc_binary', 'env_control'];
// Control + request/response/session goldens land with merged units; the
// shared dir is asserted in full so both language mirrors pin the same set.
const CONTROL = ['ctrl_viewer_open', 'ctrl_viewer_close', 'ctrl_typed', 'ctrl_stats'];
const REQUEST = ['req_list', 'req_launch', 'req_kill', 'req_resize', 'req_ping', 'req_paste', 'req_enter', 'req_keys', 'req_history', 'req_read', 'req_take_size', 'req_media'];

test('wire_server_goldens_roundtrip_byte_identical', () => {
  for (const name of SERVER) {
    const bytes = goldenBytes(`${name}.json`);
    const text = bytes.toString('utf-8');
    const decoded = decodeTextFrame(text);
    expect(decoded.kind, name).toBe('known');
    const re = encodeServerFrame(decoded.frame as ServerFrame);
    expect(re, name).toBe(text);
    expect(Buffer.from(re, 'utf-8').equals(Buffer.from(bytes)), name).toBe(true);
  }
});

test('wire_client_goldens_roundtrip_byte_identical', () => {
  for (const name of CLIENT) {
    const text = goldenBytes(`${name}.json`).toString('utf-8');
    const decoded = decodeTextFrame(text);
    expect(decoded.kind, name).toBe('known');
    expect(encodeClientFrame(decoded.frame as never), name).toBe(text);
  }
});

test('wire_envelope_goldens_roundtrip_byte_identical', () => {
  for (const name of ENVELOPE) {
    const hex = goldenBytes(`${name}.hex`).toString('utf-8').trim();
    const bytes = Buffer.from(hex, 'hex');
    const dec = new EnvelopeDecoder();
    const envs = dec.push(new Uint8Array(bytes));
    expect(envs.length, name).toBe(1);
    const re = encodeEnvelope(envs[0].kind, envs[0].body);
    expect(Buffer.from(re).toString('hex'), name).toBe(hex);
  }
});

test('wire_envelope_split_frame_needs_all_bytes', () => {
  const env = encodeEnvelope(ENV_TEXT, enc('{"t":"ping"}'));
  const dec = new EnvelopeDecoder();
  expect(dec.push(env.subarray(0, 3))).toEqual([]);
  expect(dec.push(env.subarray(3, 6))).toEqual([]);
  const done = dec.push(env.subarray(6));
  expect(done.length).toBe(1);
  expect(done[0].body).toEqual(enc('{"t":"ping"}'));
});

test('wire_golden_directory_is_complete', () => {
  const actual = readdirSync(GOLDEN).sort();
  const expected = [...SERVER.map((n) => `${n}.json`), ...CLIENT.map((n) => `${n}.json`), ...CONTROL.map((n) => `${n}.json`), ...REQUEST.map((n) => `${n}.json`), 'resp_ok.json', 'resp_err.json', 'session_info.json', ...ENVELOPE.map((n) => `${n}.hex`)].sort();
  expect(actual).toEqual(expected);
});

test('wire_binary_frame_roundtrip_and_negatives', () => {
  const ff = encodeBinaryFrame(0x0102, enc('hi'));
  expect(Array.from(ff)).toEqual([0, 0, 0, 0, 0, 0, 1, 2, 104, 105]);
  const back = decodeBinaryFrame(ff);
  expect(back.offset).toBe(0x0102);
  expect(back.payload).toEqual(enc('hi'));
  expect(() => decodeBinaryFrame(new Uint8Array(7))).toThrow(FrameError);
  const tooBig = new Uint8Array(8);
  new DataView(tooBig.buffer).setBigUint64(0, 1n << 53n);
  expect(() => decodeBinaryFrame(tooBig)).toThrow(FrameError);
  const max = new Uint8Array(8);
  new DataView(max.buffer).setBigUint64(0, (1n << 53n) - 1n);
  expect(decodeBinaryFrame(max).offset).toBe(Number((1n << 53n) - 1n));
  expect(() => encodeBinaryFrame(2 ** 53, enc('x'))).toThrow(FrameError);
});

test('wire_unknown_t_preserved_raw_not_error', () => {
  const raw = '{"t":"future-kind","x":[1,2,3]}';
  const decoded = decodeTextFrame(raw);
  expect(decoded.kind).toBe('unknown');
  expect(decoded.kind === 'unknown' && decoded.raw).toBe(raw);
});

test('wire_malformed_json_and_missing_t_are_errors', () => {
  expect(() => decodeTextFrame('{"t":"ping"')).toThrow(FrameError);
  expect(() => decodeTextFrame('not json')).toThrow(FrameError);
  expect(() => decodeTextFrame('[1,2,3]')).toThrow(FrameError);
  expect(() => decodeTextFrame('{"x":1}')).toThrow(FrameError);
});

test('wire_envelope_negatives', () => {
  expect(() => encodeEnvelope(9, enc('x'))).toThrow(FrameError);
  const dec = new EnvelopeDecoder();
  expect(() => dec.push(new Uint8Array([0, 0, 0, 0, 0]))).toThrow(FrameError);
  const dec2 = new EnvelopeDecoder();
  expect(() => dec2.push(new Uint8Array([0, 0, 0, 2, 9, 65]))).toThrow(FrameError);
});

test('wire_control_and_stats_encode_shapes', () => {
  expect(encodeControlMessage({ t: 'viewer-open', viewer: 'v1', profile: 'desktop', resume: null })).toBe(
    '{"t":"viewer-open","viewer":"v1","profile":"desktop","resume":null}',
  );
  expect(encodeControlMessage({ t: 'viewer-open', viewer: 'v1', profile: 'phone', resume: { offset: 5, mseq: 2, epoch: 'ab' } })).toBe(
    '{"t":"viewer-open","viewer":"v1","profile":"phone","resume":{"offset":5,"mseq":2,"epoch":"ab"}}',
  );
  expect(encodeControlMessage({ t: 'viewer-close', viewer: 'v1' })).toBe('{"t":"viewer-close","viewer":"v1"}');
  expect(encodeControlMessage({ t: 'typed', viewer: 'v1' })).toBe('{"t":"typed","viewer":"v1"}');
  expect(encodeControlMessage({ t: 'stats' })).toBe('{"t":"stats"}');
  expect(encodeStatsResponse({ t: 'stats', joins: 2, degradedJoins: 1, viewers: 3 })).toBe(
    '{"t":"stats","joins":2,"degradedJoins":1,"viewers":3}',
  );
});

test('wire_environment_envelope_kinds_numbered', () => {
  expect([ENV_TEXT, ENV_BINARY, ENV_CONTROL]).toEqual([0, 1, 2]);
});
