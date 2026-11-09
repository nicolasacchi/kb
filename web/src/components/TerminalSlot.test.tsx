// @vitest-environment jsdom
//
// A4/FXA4 — the seam renders nothing until the kb-term track fills the lane:
// unavailable, unadmitted, and available-and-admitted all give no DOM, and
// the page's shouldRenderTerminal gate stays false for every combination so
// no empty 'Terminal' section ever renders (brief A4 §3.1). MUTATION: if
// shouldRenderTerminal returned `lane.available && lane.admitted`, the
// available+admitted case below would fail.
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render } from "@testing-library/react";
import { TerminalSlot, shouldRenderTerminal } from "./TerminalSlot";

afterEach(cleanup);

describe("TerminalSlot", () => {
  it("renders nothing for unavailable, unadmitted, and available-and-admitted lanes", () => {
    for (const lane of [
      { available: false, reason: "terminal disabled", admitted: false },
      { available: true, term_id: "abc123", admitted: false },
      { available: true, term_id: "abc123", admitted: true },
    ] as const) {
      const { container, unmount } = render(
        <TerminalSlot sid="sid-1" harness="claude" lane={lane} />,
      );
      expect(container.firstChild).toBeNull();
      unmount();
    }
  });

  it("shouldRenderTerminal is false for all four available×admitted combinations", () => {
    for (const available of [true, false]) {
      for (const admitted of [true, false]) {
        expect(shouldRenderTerminal({ available, admitted })).toBe(false);
      }
    }
  });

  it("renders nothing even when termId/state are passed", () => {
    const { container } = render(
      <TerminalSlot
        sid="sid-1"
        termId="abc123"
        harness="claude"
        state="working"
        lane={{ available: true, term_id: "abc123", state: "working", admitted: true }}
      />,
    );
    expect(container.firstChild).toBeNull();
  });
});
