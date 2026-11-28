import { describe, expect, it } from "vitest";
import type { FileUploadAnswer } from "../../api/generated/FileUploadAnswer";
import * as Q from "./uploadQueue";

const f = (name: string, size = 3): File => new File([new Uint8Array(size)], name);
const ans = { path: "/up/a", name: "a", bytes: 3, kind: "other", sha256: "x", mime: null, kb: { status: "skipped", reason: "not-configured", id: null, stub_rel: null, created: null, inlined: null } } as FileUploadAnswer;
const status = (s: Q.QueueState): string[] => s.items.map((i) => i.status);

describe("uploadQueue", () => {
  it("sends one at a time, in the order added", () => {
    let s = Q.enqueue(Q.emptyQueue, [f("a"), f("b"), f("c")], false);
    const first = Q.nextToSend(s)!;
    expect(first.file.name).toBe("a");
    s = Q.start(s, first.id);
    expect(Q.nextToSend(s)).toBeNull();
    s = Q.succeed(s, first.id, ans);
    expect(Q.nextToSend(s)!.file.name).toBe("b");
    expect(status(s)).toEqual(["done", "queued", "queued"]);
  });

  it("carries the index flag per file at add time", () => {
    let s = Q.enqueue(Q.emptyQueue, [f("a")], true);
    s = Q.enqueue(s, [f("b")], false);
    expect(s.items.map((i) => i.index)).toEqual([true, false]);
  });

  it("an error does not stall the queue", () => {
    let s = Q.enqueue(Q.emptyQueue, [f("a"), f("b")], false);
    s = Q.start(s, 1);
    s = Q.fail(s, 1, "boom");
    expect(s.items[0].error).toBe("boom");
    expect(Q.nextToSend(s)!.file.name).toBe("b");
  });

  it("cancels a queued item (skipped) and an in-flight one", () => {
    let s = Q.enqueue(Q.emptyQueue, [f("a"), f("b"), f("c")], false);
    s = Q.start(s, 1);
    s = Q.cancel(s, 2);
    s = Q.cancel(s, 1);
    expect(status(s)).toEqual(["cancelled", "cancelled", "queued"]);
    expect(Q.nextToSend(s)!.file.name).toBe("c");
  });

  it("a late result cannot resurrect a cancelled item", () => {
    let s = Q.enqueue(Q.emptyQueue, [f("a")], false);
    s = Q.start(s, 1);
    s = Q.cancel(s, 1);
    s = Q.succeed(s, 1, ans);
    expect(status(s)).toEqual(["cancelled"]);
  });

  it("a request that finishes after Cancel lands as done + late; only a cancelled row can land", () => {
    let s = Q.enqueue(Q.emptyQueue, [f("a"), f("b")], false);
    s = Q.start(s, 1);
    s = Q.cancel(s, 1);
    s = Q.landed(s, 1, ans);
    expect(status(s)).toEqual(["done", "queued"]);
    expect(s.items[0].late).toBe(true);
    expect(s.items[0].result).toBe(ans);
    expect(Q.landed(s, 2, ans).items[1].status).toBe("queued");
    let t = Q.enqueue(Q.emptyQueue, [f("c")], false);
    t = Q.succeed(Q.start(t, 1), 1, ans);
    expect(t.items[0].late).toBe(false);
    expect(Q.landed(t, 1, ans).items[0].late).toBe(false);
  });

  it("fullySent is true only once every byte left", () => {
    let s = Q.enqueue(Q.emptyQueue, [f("a", 10)], false);
    s = Q.progress(Q.start(s, 1), 1, 4, 10);
    expect(Q.fullySent(s.items[0])).toBe(false);
    s = Q.progress(s, 1, 10, 10);
    expect(Q.fullySent(s.items[0])).toBe(true);
  });

  it("tracks progress and clears only finished rows", () => {
    let s = Q.enqueue(Q.emptyQueue, [f("a", 10), f("b"), f("c")], false);
    s = Q.start(s, 1);
    s = Q.progress(s, 1, 4, 10);
    expect(s.items[0].loaded).toBe(4);
    s = Q.succeed(Q.start(Q.succeed(s, 1, ans), 2), 2, ans);
    s = Q.clearFinished(s);
    expect(s.items.map((i) => i.file.name)).toEqual(["c"]);
  });
});
