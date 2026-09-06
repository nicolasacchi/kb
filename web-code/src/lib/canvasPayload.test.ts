import { describe, expect, it } from "vitest";
import {
  canvasPayloadBytes,
  decodeCanvasPayload,
  encodeCanvasPayload,
  EMPTY_CANVAS_PAYLOAD,
  fragmentKey,
  resolveFragmentSymbol,
  type CanvasPayloadV1,
} from "./canvasPayload";

describe("decodeCanvasPayload / encodeCanvasPayload", () => {
  it("round-trips a full v1 payload", () => {
    const src: CanvasPayloadV1 = {
      version: 1,
      fragments: [
        { path: "lib.rs", symbol: "foo", line: 12, x: 40, y: 80, w: 360 },
        { path: "caller.rs", symbol: "bar", line: 3, x: 500, y: 80 },
      ],
      view: { x: -20, y: 10, zoom: 1.25 },
    };
    const encoded = encodeCanvasPayload(src);
    const decoded = decodeCanvasPayload(encoded);
    expect(decoded).toEqual(src);
  });

  it("round-trips through JSON (server-opaque path)", () => {
    const src: CanvasPayloadV1 = {
      version: 1,
      fragments: [{ path: "a.rs", symbol: "z", line: 1, x: 0, y: 0 }],
      view: { x: 0, y: 0, zoom: 1 },
    };
    const wire = JSON.parse(JSON.stringify(encodeCanvasPayload(src)));
    expect(decodeCanvasPayload(wire)).toEqual(src);
  });

  it("degrades malformed / foreign-version payloads to empty", () => {
    expect(decodeCanvasPayload(null)).toEqual({
      version: 1,
      fragments: [],
      view: { x: 0, y: 0, zoom: 1 },
    });
    expect(decodeCanvasPayload({ version: 99, fragments: [{ path: "x" }] }).fragments).toEqual([]);
    expect(decodeCanvasPayload({ version: 1, fragments: "nope" }).fragments).toEqual([]);
    expect(decodeCanvasPayload({ version: 1, fragments: [{ path: "", symbol: "a", line: 1, x: 0, y: 0 }] }).fragments).toEqual([]);
  });

  it("drops invalid fragments but keeps valid ones", () => {
    const decoded = decodeCanvasPayload({
      version: 1,
      fragments: [
        { path: "ok.rs", symbol: "ok", line: 2, x: 1, y: 2 },
        { path: "bad.rs", symbol: "x" }, // missing line/x/y
        null,
      ],
      view: { x: 5, y: 6, zoom: 0 }, // zoom 0 → reset to 1
    });
    expect(decoded.fragments).toEqual([{ path: "ok.rs", symbol: "ok", line: 2, x: 1, y: 2 }]);
    expect(decoded.view).toEqual({ x: 5, y: 6, zoom: 1 });
  });

  it("EMPTY_CANVAS_PAYLOAD encodes to a tiny stable JSON", () => {
    const enc = encodeCanvasPayload(EMPTY_CANVAS_PAYLOAD);
    expect(enc.version).toBe(1);
    expect(enc.fragments).toEqual([]);
    expect(canvasPayloadBytes(enc)).toBeLessThan(128);
  });
});

describe("fragmentKey / resolveFragmentSymbol", () => {
  it("keys by path+symbol+line", () => {
    expect(fragmentKey({ path: "a.rs", symbol: "f", line: 3 })).toBe("a.rs\0f\0" + "3");
  });

  it("resolves unique name, nearest line when overloaded, null when missing", () => {
    const symbols = [
      { name: "foo", line_start: 10, line_end: 20 },
      { name: "foo", line_start: 50, line_end: 60 },
      { name: "bar", line_start: 1, line_end: 2 },
    ];
    expect(resolveFragmentSymbol({ path: "x", symbol: "bar", line: 99 }, symbols)).toEqual({
      name: "bar",
      line_start: 1,
      line_end: 2,
    });
    expect(resolveFragmentSymbol({ path: "x", symbol: "foo", line: 48 }, symbols)?.line_start).toBe(50);
    expect(resolveFragmentSymbol({ path: "x", symbol: "missing", line: 1 }, symbols)).toBeNull();
  });
});
