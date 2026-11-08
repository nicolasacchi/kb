// @vitest-environment jsdom
//
// A4 — the seam renders nothing until the kb-term track fills the lane:
// unavailable, unadmitted, and available-and-admitted all give no DOM.
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render } from "@testing-library/react";
import { TerminalSlot } from "./TerminalSlot";

afterEach(cleanup);

describe("TerminalSlot", () => {
  it("renders nothing for unavailable, unadmitted, and available-and-admitted lanes", () => {
    for (const lane of [
      { available: false, reason: "no terminal runner" },
      { available: true, reason: "", admitted: false },
      { available: true, reason: "", admitted: true },
    ] as const) {
      const { container, unmount } = render(
        <TerminalSlot sid="sid-1" harness="claude" lane={lane} />,
      );
      expect(container.firstChild).toBeNull();
      unmount();
    }
  });
});
