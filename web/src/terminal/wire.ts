// T14 — the kb-term/1 client-side wire codec.
//
// Wire names are copied from the merged Rust contract
// (`crates/kb-term/src/wire.rs`, `PROTO = "kb-term/1"`) and pinned by the
// golden lock-step test (`wire.test.ts`), which reads T1's golden JSON
// files and fails if a golden gains a field outside `KNOWN_SERVER_KEYS`.
// Both directions were verified against that source:
//
//   * Server→client binary: 8-byte BIG-ENDIAN u64 offset of payload byte
//     0, then payload. Shorter than 8 bytes is a protocol error. Offsets
//     are JS-safe integers (`< 2^53`, matching the Rust `MAX_OFFSET`).
//     The viewer-id prefix the tap adds to S→C bodies is stripped by
//     kb-server (`strip_s2c_id`); the browser only ever sees this shape.
//   * Server→client text: JSON `{"t": "<kind>", ...}` with unknown
//     fields tolerated on read (newer servers stay readable). Unknown
//     kinds are ignored by the session machine, never thrown.
//   * Client→server: JSON `{"t": "input", "data": "<string>"}` only.
//     Resume is the `?from=` query parameter on the WebSocket URL, not
//     a frame; the client never sends `resumefrom`, `ping` or
//     `takesize` frames (T12's relay pings the browser at the WS layer,
//     which browsers answer automatically).

export const PROTO = "kb-term/1";

/** Largest legal stream offset (offsets are JS-safe integers, `< 2^53`). */
export const MAX_OFFSET: number = 1 << 53;

export class WireProtocolError extends Error {
  readonly kind: "short-binary" | "offset-out-of-range" | "malformed-text";
  constructor(kind: "short-binary" | "offset-out-of-range" | "malformed-text", detail: string) {
    super(`${kind}: ${detail}`);
    this.name = "WireProtocolError";
    this.kind = kind;
  }
}

// ---------------------------------------------------------- binary frames

export interface DecodedBinaryFrame {
  offset: number;
  payload: Uint8Array;
}

/** Decode one binary frame: 8-byte BE u64 offset + payload. */
export function decodeBinaryFrame(frame: ArrayBuffer | Uint8Array): DecodedBinaryFrame {
  const bytes = frame instanceof Uint8Array ? frame : new Uint8Array(frame);
  if (bytes.byteLength < 8) {
    throw new WireProtocolError("short-binary", `frame is ${bytes.byteLength} bytes, need >= 8`);
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const offsetBig = view.getBigUint64(0, false);
  if (offsetBig > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new WireProtocolError("offset-out-of-range", `offset ${offsetBig.toString()} >= 2^53`);
  }
  return { offset: Number(offsetBig), payload: bytes.subarray(8) };
}

/** Encode one binary frame (used by tests and the e2e fake tap). */
export function encodeBinaryFrame(offset: number, payload: Uint8Array): Uint8Array {
  if (!Number.isSafeInteger(offset) || offset < 0) {
    throw new WireProtocolError("offset-out-of-range", `offset ${offset} is not a safe integer`);
  }
  const out = new Uint8Array(8 + payload.length);
  new DataView(out.buffer).setBigUint64(0, BigInt(offset), false);
  out.set(payload, 8);
  return out;
}

// ------------------------------------------------------------ text frames

export interface HelloFrame { t: "hello"; proto: string; session: string; epoch: string; harness: string; bound_sid: string | null }
export interface ProfileFrame {
  t: "profile"; xterm: string; serialize: string; width: string; image: string;
  cols: number; rows: number; scrollback: number; scroll_on_erase_in_display: boolean;
  cell_px_w: number; cell_px_h: number; theme: unknown;
}
export interface ImageDefFrame {
  t: "imagedef"; seq: number; id: number; gen: number; sha256: string; mime: string;
  w: number; h: number; cols: number; rows: number; offset: number;
}
export interface SeedFrame { t: "seed"; offset: number; len: number }
export interface ResizeFrame { t: "resize"; seq: number; offset: number; cols: number; rows: number }
export interface SizechangedFrame { t: "sizechanged"; seq: number; cols: number; rows: number; by: string; offset: number }
export interface GfxresetFrame { t: "gfxreset"; seq: number; gen: number; offset: number }
export interface ExitFrame { t: "exit"; seq: number; code: number | null; offset: number }
export interface ClipboardFrame { t: "clipboard"; offset: number; data_b64: string }
export interface ErrorFrame { t: "error"; code: string; msg: string }
export interface UnknownFrame { t: "unknown"; raw: string }

export type ServerTextFrame =
  | HelloFrame | ProfileFrame | ImageDefFrame | SeedFrame | ResizeFrame
  | SizechangedFrame | GfxresetFrame | ExitFrame | ClipboardFrame | ErrorFrame | UnknownFrame;

/**
 * Parse one server→client text frame. Malformed JSON or a missing string
 * `t` is a protocol error; an unknown kind yields `{t: "unknown"}` and is
 * ignored by the session machine (never thrown).
 */
export function parseTextFrame(raw: string): ServerTextFrame {
  let obj: unknown;
  try {
    obj = JSON.parse(raw);
  } catch {
    throw new WireProtocolError("malformed-text", "frame is not JSON");
  }
  if (typeof obj !== "object" || obj === null) {
    throw new WireProtocolError("malformed-text", "frame is not an object");
  }
  // Narrow the `t` discriminant before naming the shape (no inline-cast
  // member access): `t` is a string here, so the switch below dispatches
  // on a checked value and the cast only renames the object.
  if (!("t" in obj) || typeof obj.t !== "string") {
    throw new WireProtocolError("malformed-text", "frame has no string `t`");
  }
  switch (obj.t) {
    case "hello": return obj as unknown as HelloFrame;
    case "profile": return obj as unknown as ProfileFrame;
    case "imagedef": return obj as unknown as ImageDefFrame;
    case "seed": return obj as unknown as SeedFrame;
    case "resize": return obj as unknown as ResizeFrame;
    case "sizechanged": return obj as unknown as SizechangedFrame;
    case "gfxreset": return obj as unknown as GfxresetFrame;
    case "exit": return obj as unknown as ExitFrame;
    case "clipboard": return obj as unknown as ClipboardFrame;
    case "error": return obj as unknown as ErrorFrame;
    default: return { t: "unknown", raw };
  }
}

// ------------------------------------------------------------ known keys
//
// The lock-step test asserts every golden's key set is contained in the
// entry for its kind, so a new golden field fails the test until this
// table is extended alongside the TS type.

export const KNOWN_SERVER_KEYS: Record<string, readonly string[]> = {
  hello: ["t", "proto", "session", "epoch", "harness", "bound_sid"],
  profile: ["t", "xterm", "serialize", "width", "image", "cols", "rows", "scrollback", "scroll_on_erase_in_display", "cell_px_w", "cell_px_h", "theme"],
  imagedef: ["t", "seq", "id", "gen", "sha256", "mime", "w", "h", "cols", "rows", "offset"],
  seed: ["t", "offset", "len"],
  resize: ["t", "seq", "offset", "cols", "rows"],
  sizechanged: ["t", "seq", "cols", "rows", "by", "offset"],
  gfxreset: ["t", "seq", "gen", "offset"],
  exit: ["t", "seq", "code", "offset"],
  clipboard: ["t", "offset", "data_b64"],
  error: ["t", "code", "msg"],
};

export const KNOWN_CLIENT_KEYS: Record<string, readonly string[]> = {
  input: ["t", "data"],
  resumefrom: ["t", "offset", "mseq", "epoch"],
  ping: ["t"],
  takesize: ["t", "cols", "rows"],
};

// ------------------------------------------------------------- client out

export interface InputFrame { t: "input"; data: string }

/** Encode keystrokes as `{"t":"input","data":"..."}` (field order pinned). */
export function encodeInput(data: string): string {
  const frame: InputFrame = { t: "input", data };
  return JSON.stringify(frame);
}
