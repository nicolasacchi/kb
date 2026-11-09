// @vitest-environment jsdom
//
// T14 — the slot seam (jsdom, React 19): the lazy chunk is NOT evaluated
// until the click. The mocked `../terminal/TerminalPanel.lazy` factory
// increments a counter when EVALUATED, so counter == 0 before click and
// == 1 after proves the boundary. A child row (`via: "root"`) shows
// link text and no button.
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";

let mockEvalCount = 0;

vi.mock("../terminal/TerminalPanel.lazy", () => {
  mockEvalCount += 1;
  return {
    __esModule: true,
    default: (props: { onClose?: () => void }) => (
      <div data-testid="mock-panel">
        <button type="button" data-testid="mock-panel-close" onClick={() => props.onClose?.()}>
          Close
        </button>
      </div>
    ),
  };
});

import { TerminalSlot } from "./TerminalSlot";

afterEach(() => {
  cleanup();
  mockEvalCount = 0;
});

describe("TerminalSlot (T14 seam)", () => {
  it("lane unavailable → nothing, lazy chunk never evaluated", () => {
    const { container } = render(
      <TerminalSlot
        sid="sid-1"
        harness="omp"
        lane={{ available: false, reason: "terminal disabled", admitted: false }}
      />,
    );
    expect(container.firstChild).toBeNull();
    expect(mockEvalCount).toBe(0);
  });

  it("available → 'Open terminal', counter 0 before click, 1 after", async () => {
    render(
      <TerminalSlot
        sid="sid-1"
        harness="omp"
        lane={{ available: true, term_id: "t1", harness: "omp", state: "working", admitted: true }}
      />,
    );
    const btn = screen.getByTestId("kb-term-open");
    expect(btn.textContent).toBe("Open terminal");
    expect(mockEvalCount).toBe(0);
    btn.click();
    // Suspense fallback renders first; the lazy factory then evaluates.
    expect(await screen.findByTestId("mock-panel")).toBeTruthy();
    expect(mockEvalCount).toBe(1);
  });

  it("child row (via root) → link text, no button, no panel", () => {
    render(
      <MemoryRouter>
        <TerminalSlot
          sid="child-sid"
          harness="omp"
          lane={{ available: true, term_id: "t1", harness: "omp", state: "working", admitted: true, via: "root" }}
        />
      </MemoryRouter>,
    );
    expect(screen.getByTestId("session-term-root-link").textContent).toContain("Terminal");
    expect(screen.queryByTestId("kb-term-open")).toBeNull();
    expect(screen.queryByTestId("mock-panel")).toBeNull();
    expect(mockEvalCount).toBe(0);
  });

  it("panel Close (slot onClose) unmounts the panel; reopening works", async () => {
    render(
      <TerminalSlot
        sid="sid-1"
        harness="omp"
        lane={{ available: true, term_id: "t1", harness: "omp", state: "working", admitted: true }}
      />,
    );
    (screen.getByTestId("kb-term-open") as HTMLButtonElement).click();
    expect(await screen.findByTestId("mock-panel")).toBeTruthy();
    (screen.getByTestId("mock-panel-close") as HTMLButtonElement).click();
    expect(screen.queryByTestId("mock-panel")).toBeNull();
    expect(screen.getByTestId("kb-term-open").textContent).toBe("Open terminal");
  });
});
