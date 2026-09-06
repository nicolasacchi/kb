import { describe, expect, it } from "vitest";
import { peekPanelHandlesKey } from "./PeekPanel";

// V71-K4 — the peek panel's key filter.
//
// `PeekPanel` focuses itself on mount and its `onKeyDown` used to call
// `stopPropagation()` for EVERY key, so from the moment a peek opened the
// panel was a hole in the app's keyboard: no global command and no chord
// could reach `CommandRoot`'s window listener. V71-K3 found the case that
// makes it more than theory — `drawer.keep` (`Space K`, "keep THIS result
// set in the drawer") is only worth pressing with a populated peek open,
// which is precisely when the panel swallowed it. The panel now stops only
// what it acts on; this pins that set in both directions.

describe("peekPanelHandlesKey", () => {
  it("claims the list's own cursor keys and its dismissal", () => {
    for (const key of ["ArrowDown", "j", "ArrowUp", "k", "Escape"]) {
      expect(peekPanelHandlesKey({ key }), key).toBe(true);
    }
  });

  it("claims every Ramp rung it offers", () => {
    // Same table (`nav/ramp.ts`'s `rungForKey`) the handler itself uses.
    expect(peekPanelHandlesKey({ key: "Enter" })).toBe(true);
    expect(peekPanelHandlesKey({ key: "Enter", shiftKey: true })).toBe(true);
    expect(peekPanelHandlesKey({ key: "Enter", ctrlKey: true })).toBe(true);
    expect(peekPanelHandlesKey({ key: "Enter", metaKey: true })).toBe(true);
    expect(peekPanelHandlesKey({ key: "o" })).toBe(true);
    expect(peekPanelHandlesKey({ key: "O" })).toBe(true);
    expect(peekPanelHandlesKey({ key: "K" })).toBe(true);
  });

  it("lets everything else through — the leader, and every key a chord can continue with", () => {
    // ` ` is `Space`, the leader: without this the panel could never be
    // acted ON by a command, only closed.
    for (const key of [" ", "u", "a", "h", "v", "n", "p", "d", "x", "D", "R", "P", "g", "1", "?", "t", ":"]) {
      expect(peekPanelHandlesKey({ key }), JSON.stringify(key)).toBe(false);
    }
  });

  it("does not claim a modified key it has no rung for", () => {
    // Mod-K (the omnibox) and Ctrl-o/Ctrl-i (the jump list) must reach
    // their own owners, not die in the panel.
    expect(peekPanelHandlesKey({ key: "k", metaKey: true })).toBe(false);
    expect(peekPanelHandlesKey({ key: "K", metaKey: true })).toBe(false);
    expect(peekPanelHandlesKey({ key: "o", ctrlKey: true })).toBe(false);
    expect(peekPanelHandlesKey({ key: "i", ctrlKey: true })).toBe(false);
    // Alt disables the whole ramp table (`rungForKey`'s first line).
    expect(peekPanelHandlesKey({ key: "Enter", altKey: true })).toBe(false);
  });
});
