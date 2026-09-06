import { afterEach, describe, expect, it } from "vitest";
import { __resetToasts, dismissToast, peekToasts, toast } from "./toast";

describe("toast store", () => {
  afterEach(() => __resetToasts());

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
    const b = toast.warn("two");
    dismissToast(a);
    dismissToast(9999);
    const list = peekToasts();
    expect(list).toHaveLength(1);
    expect(list[0]).toMatchObject({ id: b, kind: "warn" });
  });

  it("snapshot ref is stable when nothing changed (useSyncExternalStore contract)", () => {
    toast.ok("x");
    expect(peekToasts()).toBe(peekToasts());
  });

  it("__resetToasts clears the queue for the next test", () => {
    toast.ok("a");
    toast.err("b");
    __resetToasts();
    expect(peekToasts()).toHaveLength(0);
  });

  it("toast.ok accepts an optional link, absent by default", () => {
    const plain = toast.ok("no link");
    expect(peekToasts().find((t) => t.id === plain)?.link).toBeUndefined();

    toast.ok("with link", { to: "/r/kb/~sets/set_1", label: "View set" });
    const withLink = peekToasts().find((t) => t.msg === "with link");
    expect(withLink?.link).toEqual({ to: "/r/kb/~sets/set_1", label: "View set" });
  });
});
