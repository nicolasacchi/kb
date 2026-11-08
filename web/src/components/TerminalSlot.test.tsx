// @vitest-environment jsdom
//
// A4 — the seam renders nothing while the lane is unavailable or
// unadmitted. T14 fills the seam: an available+admitted lane with a
// term renders the "Open terminal" button (the panel itself mounts
// lazily after the click; the dom test
// `TerminalSlot.dom.test.tsx` pins the chunk evaluation).
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { TerminalSlot } from "./TerminalSlot";

afterEach(cleanup);

describe("TerminalSlot", () => {
  it("renders nothing for unavailable and unadmitted lanes", () => {
    for (const lane of [
      { available: false, reason: "terminal disabled", admitted: false },
      { available: true, term_id: "abc123", admitted: false },
    ] as const) {
      const { container, unmount } = render(
        <TerminalSlot sid="sid-1" harness="claude" lane={lane} />,
      );
      expect(container.firstChild).toBeNull();
      unmount();
    }
  });

  it("renders the Open terminal button for an available+admitted lane", () => {
    render(
      <TerminalSlot
        sid="sid-1"
        harness="claude"
        lane={{ available: true, term_id: "abc123", admitted: true }}
      />,
    );
    expect(screen.getByTestId("kb-term-open").textContent).toBe("Open terminal");
  });
});
