import { describe, expect, it } from "vitest";
import { SseDecoder, readSseStream, type SseFrame } from "./stream";

const enc = new TextEncoder();

function feedAll(decoder: SseDecoder, ...chunks: string[]): SseFrame[] {
  const frames: SseFrame[] = [];
  for (const c of chunks) frames.push(...decoder.feed(enc.encode(c)));
  frames.push(...decoder.end());
  return frames;
}

describe("SseDecoder", () => {
  it("parses a complete typed frame with id", () => {
    const frames = feedAll(
      new SseDecoder(),
      'id: 42\nevent: artifact.indexed\ndata: {"v":1}\n\n',
    );
    expect(frames).toEqual([
      { event: "artifact.indexed", data: '{"v":1}', id: "42" },
    ]);
  });

  it("defaults the event type to message", () => {
    const frames = feedAll(new SseDecoder(), "data: hi\n\n");
    expect(frames).toEqual([{ event: "message", data: "hi", id: null }]);
  });

  it("joins multi-line data with newlines", () => {
    const frames = feedAll(new SseDecoder(), "data: a\ndata: b\ndata:\n\n");
    expect(frames).toEqual([{ event: "message", data: "a\nb\n", id: null }]);
  });

  it("strips exactly one leading space from values", () => {
    const frames = feedAll(new SseDecoder(), "data:  two spaces\ndata:none\n\n");
    expect(frames[0].data).toBe(" two spaces\nnone");
  });

  it("ignores comment lines (keep-alive) without dispatching", () => {
    const frames = feedAll(
      new SseDecoder(),
      ":keep-alive\n\ndata: x\n:keep-alive\n\n",
    );
    expect(frames).toEqual([{ event: "message", data: "x", id: null }]);
  });

  it("never dispatches a frame without data", () => {
    const frames = feedAll(new SseDecoder(), "event: ghost\n\n");
    expect(frames).toEqual([]);
  });

  it("resets the event type after a data-less blank line", () => {
    // Per spec the event-type buffer clears on every dispatch attempt,
    // even an empty one — "ghost" must not leak onto the next frame.
    const frames = feedAll(new SseDecoder(), "event: ghost\n\ndata: x\n\n");
    expect(frames).toEqual([{ event: "message", data: "x", id: null }]);
  });

  it("keeps the last event id sticky across frames", () => {
    const frames = feedAll(
      new SseDecoder(),
      "id: 7\ndata: a\n\ndata: b\n\nid: 9\ndata: c\n\n",
    );
    expect(frames.map((f) => f.id)).toEqual(["7", "7", "9"]);
  });

  it("ignores ids containing NUL", () => {
    const frames = feedAll(new SseDecoder(), "id: bad\0id\ndata: a\n\n");
    expect(frames[0].id).toBeNull();
  });

  it("handles CRLF and lone-CR line endings", () => {
    const frames = feedAll(
      new SseDecoder(),
      "event: a\r\ndata: 1\r\n\r\nevent: b\rdata: 2\r\r",
    );
    expect(frames).toEqual([
      { event: "a", data: "1", id: null },
      { event: "b", data: "2", id: null },
    ]);
  });

  it("handles a CRLF split across chunk boundaries as one terminator", () => {
    const frames = feedAll(new SseDecoder(), "data: x\r", "\n\r", "\n");
    expect(frames).toEqual([{ event: "message", data: "x", id: null }]);
  });

  it("handles frames split across chunks mid-line", () => {
    const frames = feedAll(
      new SseDecoder(),
      "event: artifact.in",
      "dexed\ndata: {",
      '"a":1}\n',
      "\n",
    );
    expect(frames).toEqual([
      { event: "artifact.indexed", data: '{"a":1}', id: null },
    ]);
  });

  it("handles multi-byte UTF-8 split across chunk boundaries", () => {
    const bytes = enc.encode("data: caffè\n\n");
    const cut = bytes.length - 4; // inside the two-byte è
    const decoder = new SseDecoder();
    const frames = [
      ...decoder.feed(bytes.slice(0, cut)),
      ...decoder.feed(bytes.slice(cut)),
      ...decoder.end(),
    ];
    expect(frames).toEqual([{ event: "message", data: "caffè", id: null }]);
  });

  it("discards an incomplete event at end of stream", () => {
    const frames = feedAll(new SseDecoder(), "data: dangling\n");
    expect(frames).toEqual([]);
  });

  it("treats a field line without a colon as an empty value", () => {
    const frames = feedAll(new SseDecoder(), "data\n\n");
    expect(frames).toEqual([{ event: "message", data: "", id: null }]);
  });
});

describe("readSseStream", () => {
  it("pumps frames in order and resolves when the stream ends", async () => {
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(enc.encode("event: a\ndata: 1\n\n"));
        controller.enqueue(enc.encode("event: b\ndata: 2\n\n"));
        controller.close();
      },
    });
    const seen: string[] = [];
    await readSseStream(body, (f) => seen.push(f.event));
    expect(seen).toEqual(["a", "b"]);
  });

  it("rejects when the reader errors (network drop / abort)", async () => {
    // Pull-based so the first chunk is READ before the error lands —
    // controller.error() discards anything still queued.
    let pulls = 0;
    const body = new ReadableStream<Uint8Array>({
      pull(controller) {
        if (pulls++ === 0) {
          controller.enqueue(enc.encode("event: a\ndata: 1\n\n"));
        } else {
          controller.error(new Error("boom"));
        }
      },
    });
    const seen: string[] = [];
    await expect(
      readSseStream(body, (f) => seen.push(f.event)),
    ).rejects.toThrow("boom");
    expect(seen).toEqual(["a"]);
  });
});
