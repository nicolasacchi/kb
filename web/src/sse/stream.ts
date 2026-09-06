// Incremental SSE (text/event-stream) decoder over fetch ReadableStream
// chunks. Replaces EventSource because the daemon names EVERY frame with
// `event:` (kb-server routes/events.rs frame_to_sse), and EventSource only
// delivers types you addEventListener for — which forced hand-maintained
// type lists (MAIN_EVENT_TYPES, KNOWN_EVENT_KINDS) and silently dropped
// types subscribed after the source opened. A generic decoder delivers
// all frames; filtering becomes the consumer's business.
//
// Spec: https://html.spec.whatwg.org/multipage/server-sent-events.html
// — lines split on CRLF | LF | CR (CRLF possibly straddling chunks),
// `:` comment lines ignored (the daemon's 15s `:keep-alive`), one leading
// space stripped from field values, multi-line `data:` joined with \n,
// `id:` sticky across frames (ignored when it contains NUL), `retry:`
// ignored (the core does its own backoff), empty line dispatches.

export type SseFrame = {
  /// `event:` field, defaulting to "message" like EventSource.
  event: string;
  /// Joined `data:` lines. Frames without data are never dispatched.
  data: string;
  /// Sticky last-event-id at dispatch time (EventSource semantics);
  /// null until the first `id:` line on the connection.
  id: string | null;
};

export class SseDecoder {
  private dec = new TextDecoder("utf-8");
  private buf = "";
  private dataLines: string[] = [];
  private eventType = "";
  private lastId: string | null = null;

  /// Feed one network chunk; returns the frames it completed. Partial
  /// lines (and a trailing lone CR that may be half a CRLF) are carried
  /// over to the next feed.
  feed(chunk: Uint8Array): SseFrame[] {
    this.buf += this.dec.decode(chunk, { stream: true });
    return this.drain(false);
  }

  /// Flush at end-of-stream. A final unterminated line is processed, but
  /// per spec an event not followed by a blank line is never dispatched.
  end(): SseFrame[] {
    this.buf += this.dec.decode();
    return this.drain(true);
  }

  private drain(eof: boolean): SseFrame[] {
    const frames: SseFrame[] = [];
    let start = 0;
    for (let i = 0; i < this.buf.length; i++) {
      const c = this.buf[i];
      if (c === "\n") {
        this.line(this.buf.slice(start, i), frames);
        start = i + 1;
      } else if (c === "\r") {
        // A CR as the very last char may be the first half of a CRLF
        // split across chunks — hold it until the next feed (or EOF).
        if (i + 1 === this.buf.length && !eof) break;
        this.line(this.buf.slice(start, i), frames);
        if (this.buf[i + 1] === "\n") i++;
        start = i + 1;
      }
    }
    this.buf = this.buf.slice(start);
    if (eof && this.buf.length > 0) {
      this.line(this.buf, frames);
      this.buf = "";
    }
    return frames;
  }

  private line(s: string, frames: SseFrame[]) {
    if (s === "") {
      // Blank line — dispatch the buffered event, if it has data.
      if (this.dataLines.length === 0) {
        this.eventType = "";
        return;
      }
      frames.push({
        event: this.eventType || "message",
        data: this.dataLines.join("\n"),
        id: this.lastId,
      });
      this.dataLines = [];
      this.eventType = "";
      return;
    }
    if (s.startsWith(":")) return; // comment (keep-alive)
    const idx = s.indexOf(":");
    let field = s;
    let value = "";
    if (idx !== -1) {
      field = s.slice(0, idx);
      value = s.slice(idx + 1);
      if (value.startsWith(" ")) value = value.slice(1);
    }
    switch (field) {
      case "data":
        this.dataLines.push(value);
        break;
      case "event":
        this.eventType = value;
        break;
      case "id":
        if (!value.includes("\0")) this.lastId = value;
        break;
      // "retry" and unknown fields: ignored.
    }
  }
}

/// Pump a fetch body through the decoder until the stream ends or the
/// reader throws (abort / network error). `onFrame` fires per complete
/// frame, in order. Returns when the SERVER ends the stream (daemon
/// shutdown closes SSE via take_until) — callers treat both return and
/// throw as a disconnect.
export async function readSseStream(
  body: ReadableStream<Uint8Array>,
  onFrame: (frame: SseFrame) => void,
): Promise<void> {
  const reader = body.getReader();
  const decoder = new SseDecoder();
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      for (const frame of decoder.feed(value)) onFrame(frame);
    }
    for (const frame of decoder.end()) onFrame(frame);
  } finally {
    reader.releaseLock();
  }
}
