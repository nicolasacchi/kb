// BSTATE — the state overlay's markup per phase (server render; no DOM needed).

import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { TermStateOverlay, announceText, overlayKind, type TermStateOverlayProps } from "./TermStateOverlay";

const base: TermStateOverlayProps = {
  phase: "live",
  caption: "live",
  exitCode: null,
  blockedReason: null,
  attempt: 0,
  firstConnect: false,
  onReconnect: () => {},
  onBack: () => {},
  compact: false,
};
const html = (over: Partial<TermStateOverlayProps>): string => renderToString(<TermStateOverlay {...base} {...over} />);

describe("TermStateOverlay", () => {
  it("renders no node at all for live and sleeping", () => {
    expect(html({ phase: "live" })).toBe("");
    expect(html({ phase: "sleeping" })).toBe("");
  });

  it("first connect shows the connecting card with no action; a later connecting is the reconnecting treatment", () => {
    const first = html({ phase: "connecting", firstConnect: true });
    expect(first).toContain("Connecting");
    expect(first).not.toContain("<button");
    const later = html({ phase: "connecting", firstConnect: false });
    expect(later).toContain("Reconnecting");
    expect(later).not.toContain("<button");
  });

  it("reconnecting shows the caption and one primary action", () => {
    const out = html({ phase: "reconnecting", caption: "reconnecting in 3s (offline)" });
    expect(out).toContain("Reconnecting");
    expect(out).toContain("reconnecting in 3s (offline)");
    expect(out).toContain("Reconnect now");
    expect(out.match(/<button/g)).toHaveLength(1);
  });

  it("expired offers Reconnect now", () => {
    expect(html({ phase: "expired" })).toContain("Reconnect now");
  });

  it("ended says how it ended and offers only Back to terminals", () => {
    const code = html({ phase: "ended", exitCode: 3 });
    expect(code).toContain("Session ended");
    expect(code).toContain("exited (code 3)");
    expect(code).toContain("Back to terminals");
    expect(code).not.toContain("Reconnect now");
    expect(html({ phase: "ended", exitCode: null })).toContain(">exited<");
  });

  it("blocked names its reason and offers Back to terminals", () => {
    const out = html({ phase: "blocked", blockedReason: "width table unavailable" });
    expect(out).toContain("width table unavailable");
    expect(out).toContain("Back to terminals");
  });

  it("an action without a callback renders no button", () => {
    expect(html({ phase: "ended", onBack: undefined })).not.toContain("<button");
  });

  it("the card carries no live region and the raw phase token is never the visible copy", () => {
    for (const phase of ["connecting", "reconnecting", "expired", "ended", "blocked"]) {
      const out = html({ phase, firstConnect: true, blockedReason: "x" });
      expect(out).not.toContain("aria-live");
      expect(out).not.toContain("role=");
    }
  });

  it("compact adds the phone modifier", () => {
    expect(html({ phase: "ended", compact: true })).toContain("kb-term-state-overlay--compact");
  });

  it("overlayKind and announceText agree on which phases speak", () => {
    expect(overlayKind("live", false)).toBeNull();
    expect(announceText("live", false, null, null)).toBe("");
    expect(announceText("reconnecting", false, null, null)).toBe("Reconnecting");
    expect(announceText("ended", false, 1, null)).toBe("Session ended, exited (code 1)");
    expect(announceText("blocked", false, null, "nope")).toBe("Blocked, nope");
  });
});
