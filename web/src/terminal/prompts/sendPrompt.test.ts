import { describe, expect, it, vi } from "vitest";
import type { Drive, SendTextResult } from "../phone/driveApi";
import { DriveError } from "../phone/driveApi";
import { PromptsApiError, type PromptList } from "./promptsApi";
import { sendPrompt, type SendPromptDeps } from "./sendPrompt";

const drive: Drive = { termId: "t1", harness: "omp", viewer: null, confirm: vi.fn(async () => true) };
const stale = () => new PromptsApiError(412, "urn:kb:errors:prompts-stale", "changed", 5);
const listWith = (done: number | null, present = true): PromptList => ({
  project: "demo",
  rev: 5,
  items: present ? [{ id: "q_1", text: "x", created_at: 1, done_at: done, order: 0 }] : [],
});

function deps(over: Partial<SendPromptDeps> & { text?: string } = {}) {
  const sent: { text: string; enter: boolean }[] = [];
  const send = vi.fn(async (_d: Drive, text: string, o: { enter: boolean }): Promise<SendTextResult> => {
    sent.push({ text, enter: o.enter });
    return { kind: "sent" };
  });
  const markDone = vi.fn(async () => ({}));
  const refetch = vi.fn(async () => listWith(null));
  const d: SendPromptDeps = { drive, item: { id: "q_1", text: over.text ?? "hello" }, markDone, refetch, send, ...over };
  return { d, sent, send, markDone, refetch };
}

describe("sendPrompt", () => {
  it("pastes a multi-line prompt as ONE line, WITHOUT Enter, then marks done", async () => {
    const t = deps({ text: "line one\n  line two\r\nline three" });
    const r = await sendPrompt(t.d);
    expect(t.sent).toEqual([{ text: "line one line two line three", enter: false }]);
    expect(t.sent[0].text).not.toMatch(/[\r\n]/);
    expect(r).toEqual({ kind: "sent", flattened: true, marked: true });
    expect(t.markDone).toHaveBeenCalledTimes(1);
  });

  it("flags flattening only when the text changed", async () => {
    const r = await sendPrompt(deps({ text: "single line" }).d);
    expect(r).toEqual({ kind: "sent", flattened: false, marked: true });
  });

  it("never presses Enter: the real sendText posts one /paste without `enter` and no /enter request", async () => {
    const urls: string[] = [];
    const bodies: Record<string, unknown>[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init: RequestInit) => {
        urls.push(url);
        bodies.push(JSON.parse(String(init.body)) as Record<string, unknown>);
        return new Response("{}", { status: 200, headers: { "Content-Type": "application/json" } });
      }),
    );
    try {
      const t = deps({ text: "a\nb" });
      delete t.d.send;
      await sendPrompt(t.d);
      expect(urls).toHaveLength(1);
      expect(urls[0]).toMatch(/\/api\/terms\/t1\/paste$/);
      expect(bodies[0]).toEqual({ text: "a b" });
      expect("enter" in bodies[0]).toBe(false);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("a declined guard pastes and marks nothing", async () => {
    const t = deps({ send: vi.fn(async () => ({ kind: "declined" }) as SendTextResult) });
    expect(await sendPrompt(t.d)).toEqual({ kind: "declined" });
    expect(t.markDone).not.toHaveBeenCalled();
  });

  it("a paste error marks nothing and carries the detail", async () => {
    const t = deps({ send: vi.fn(async () => Promise.reject(new DriveError(409, null, "pane is busy"))) });
    expect(await sendPrompt(t.d)).toEqual({ kind: "error", detail: "pane is busy" });
    expect(t.markDone).not.toHaveBeenCalled();
  });

  it("a blank prompt sends nothing", async () => {
    const t = deps({ text: " \n " });
    expect((await sendPrompt(t.d)).kind).toBe("error");
    expect(t.send).not.toHaveBeenCalled();
  });

  it("a 412 on done-marking refetches once and retries once, never pasting twice", async () => {
    const t = deps();
    t.markDone.mockRejectedValueOnce(stale());
    const r = await sendPrompt(t.d);
    expect(r).toEqual({ kind: "sent", flattened: false, marked: true });
    expect(t.send).toHaveBeenCalledTimes(1);
    expect(t.refetch).toHaveBeenCalledTimes(1);
    expect(t.markDone).toHaveBeenCalledTimes(2);
  });

  it("gives up after the one retry (still pasted once) and says it could not mark done", async () => {
    const t = deps();
    t.markDone.mockRejectedValue(stale());
    const r = await sendPrompt(t.d);
    expect(r).toEqual({ kind: "sent", flattened: false, marked: false });
    expect(t.send).toHaveBeenCalledTimes(1);
    expect(t.markDone).toHaveBeenCalledTimes(2);
  });

  it("does not retry when the refetch shows the item already done or gone", async () => {
    const done = deps();
    done.markDone.mockRejectedValueOnce(stale());
    done.refetch.mockResolvedValue(listWith(7));
    expect((await sendPrompt(done.d)).marked).toBe(true);
    expect(done.markDone).toHaveBeenCalledTimes(1);

    const gone = deps();
    gone.markDone.mockRejectedValueOnce(stale());
    gone.refetch.mockResolvedValue(listWith(null, false));
    expect((await sendPrompt(gone.d)).marked).toBe(false);
    expect(gone.markDone).toHaveBeenCalledTimes(1);
  });

  it("a non-412 failure while marking is not retried", async () => {
    const t = deps();
    t.markDone.mockRejectedValue(new PromptsApiError(500, null, "io", null));
    expect((await sendPrompt(t.d)).marked).toBe(false);
    expect(t.markDone).toHaveBeenCalledTimes(1);
    expect(t.refetch).not.toHaveBeenCalled();
  });
});
