// T18 — the Claude background guard: Left and exactly `/bg`, on claude only.

import { describe, expect, it } from "vitest";
import { needsConfirm } from "./guards";

describe("needsConfirm", () => {
  it("confirms claude Left", () => {
    const c = needsConfirm("claude", { kind: "key", key: "Left" });
    expect(c).not.toBeNull();
    expect(c?.title).toBe("Move this conversation to Claude's background?");
    expect(c?.confirmLabel).toBe("Send anyway");
    expect(c?.danger).toBe(true);
    expect(c?.body).toContain("background supervisor");
  });

  it("confirms claude `/bg`, also padded", () => {
    expect(needsConfirm("claude", { kind: "text", text: "/bg" })).not.toBeNull();
    expect(needsConfirm("claude", { kind: "text", text: " /bg " })).not.toBeNull();
    expect(needsConfirm("claude", { kind: "text", text: "/bg\n" })).not.toBeNull();
  });

  it("does not confirm anything else", () => {
    expect(needsConfirm("omp", { kind: "key", key: "Left" })).toBeNull();
    expect(needsConfirm("omp", { kind: "text", text: "/bg" })).toBeNull();
    expect(needsConfirm("claude", { kind: "text", text: "/bgx" })).toBeNull();
    expect(needsConfirm("claude", { kind: "text", text: "/bg now" })).toBeNull();
    expect(needsConfirm("claude", { kind: "text", text: "hello /bg" })).toBeNull();
    expect(needsConfirm("claude", { kind: "key", key: "Right" })).toBeNull();
    expect(needsConfirm("claude", { kind: "key", key: "Esc" })).toBeNull();
    expect(needsConfirm("codex", { kind: "key", key: "Left" })).toBeNull();
  });
});
