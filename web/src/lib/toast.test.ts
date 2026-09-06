import { afterEach, describe, expect, it } from "vitest";
import { __resetToasts, dismissToast, peekToasts, toast } from "./toast";

describe("toast store", () => {
  afterEach(() => __resetToasts());

  // invariant:32
  it("push appends with the right kind + message and an incrementing id", () => {
    const a = toast.ok("saved");
    const b = toast.err("nope");
    expect(b).toBeGreaterThan(a);
    const list = peekToasts();
    expect(list).toHaveLength(2);
    expect(list[0]).toMatchObject({ id: a, kind: "ok", msg: "saved" });
    expect(list[1]).toMatchObject({ id: b, kind: "err", msg: "nope" });
  });

  it("dismiss removes only the matching id; unknown id is a no-op", () => {
    const a = toast.ok("one");
    const b = toast.info("two");
    dismissToast(a);
    dismissToast(9999);
    const list = peekToasts();
    expect(list).toHaveLength(1);
    expect(list[0]).toMatchObject({ id: b, kind: "info" });
  });

  it("snapshot ref is stable when nothing changed (useSyncExternalStore contract)", () => {
    toast.ok("x");
    expect(peekToasts()).toBe(peekToasts());
  });

  // U4 — capture's "Open" toast action.
  it("ok() with an action attaches it to the pushed toast", () => {
    const onClick = () => {};
    const id = toast.ok("captured", { label: "Open", onClick });
    const list = peekToasts();
    expect(list[0]).toMatchObject({
      id,
      kind: "ok",
      msg: "captured",
      action: { label: "Open", onClick },
    });
  });

  it("ok()/info() without an action leave it undefined", () => {
    toast.ok("plain");
    expect(peekToasts()[0].action).toBeUndefined();
  });
});
