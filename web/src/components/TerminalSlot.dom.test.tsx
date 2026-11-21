// @vitest-environment jsdom
//
// T14 — the slot seam (jsdom, React 19): the lazy chunk is NOT evaluated
// until the click. The mocked `../terminal/TerminalPanel.lazy` factory
// increments a counter when EVALUATED, so counter == 0 before click and
// == 1 after proves the boundary. A child row (`via: "root"`) shows
// link text and no button.
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";

let mockEvalCount = 0;

vi.mock("../terminal/TerminalPanel.lazy", () => {
  mockEvalCount += 1;
  return {
    __esModule: true,
    default: (props: { onClose?: () => void; onBell?: () => void }) => (
      <div data-testid="mock-panel">
        <button type="button" data-testid="mock-panel-bell" onClick={() => props.onBell?.()}>
          Bell
        </button>
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

  it("while the panel is open the slot shows no Open button and no second Close; the panel's own Close brings Open back", async () => {
    render(
      <TerminalSlot
        sid="sid-1"
        harness="omp"
        lane={{ available: true, term_id: "t1", harness: "omp", state: "working", admitted: true }}
      />,
    );
    (screen.getByTestId("kb-term-open") as HTMLButtonElement).click();
    expect(await screen.findByTestId("mock-panel")).toBeTruthy();
    expect(screen.queryByTestId("kb-term-open")).toBeNull();
    expect(screen.queryByText("Close terminal")).toBeNull();
    expect(screen.getAllByRole("button")).toHaveLength(1);
  });

  it("child row (via root) → link text, no button, no panel", () => {
    render(
      <MemoryRouter>
        <TerminalSlot
          sid="child-sid"
          rootSid="root-sid"
          harness="omp"
          lane={{ available: true, term_id: "t1", harness: "omp", state: "working", admitted: true, via: "root" }}
        />
      </MemoryRouter>,
    );
    expect(screen.getByTestId("session-term-root-link").textContent).toContain("Terminal");
    expect(screen.getByTestId("session-term-root-link").getAttribute("href")).toBe("/sessions/root-sid");
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
    await waitFor(() => expect(screen.queryByTestId("mock-panel")).toBeNull());
    expect(screen.getByTestId("kb-term-open").textContent).toBe("Open terminal");
  });
});

describe("TerminalSlot bell dot (FUTERMATTN)", () => {
  const lane = { available: true, term_id: "t1", harness: "omp", state: "working", admitted: true } as const;

  it("a bell sets one quiet dot, however many arrive, and focus clears it", async () => {
    render(<TerminalSlot sid="sid-1" harness="omp" lane={lane} />);
    expect(screen.queryByTestId("kb-term-bell")).toBeNull();
    screen.getByTestId("kb-term-open").click();
    const bell = await screen.findByTestId("mock-panel-bell");
    act(() => {
      bell.click();
      bell.click();
      bell.click();
    });
    expect(screen.getAllByTestId("kb-term-bell")).toHaveLength(1);
    act(() => {
      window.dispatchEvent(new Event("focus"));
    });
    expect(screen.queryByTestId("kb-term-bell")).toBeNull();
  });

  it("a tab turning visible clears the dot as well", async () => {
    render(<TerminalSlot sid="sid-1" harness="omp" lane={lane} />);
    screen.getByTestId("kb-term-open").click();
    const bell = await screen.findByTestId("mock-panel-bell");
    act(() => bell.click());
    expect(screen.getByTestId("kb-term-bell")).toBeTruthy();
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    expect(screen.queryByTestId("kb-term-bell")).toBeNull();
  });
});
