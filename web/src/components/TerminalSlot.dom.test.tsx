// @vitest-environment jsdom
//
// T14/BSHELL — the slot seam (jsdom): a door to /terminals/<id>, never a host. The
// mocked panel factory increments a counter when EVALUATED, so counter == 0
// after render and click proves the slot mounts no panel and loads no chunk.
// A child row (`via: "root"`) shows link text and no door.
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";

let mockEvalCount = 0;

vi.mock("../terminal/TerminalPanel.lazy", () => {
  mockEvalCount += 1;
  return {
    __esModule: true,
    default: () => <div data-testid="mock-panel" />,
  };
});

import { TerminalSlot } from "./TerminalSlot";

afterEach(() => {
  cleanup();
  mockEvalCount = 0;
});

function Where() {
  return <div data-testid="where">{useLocation().pathname}</div>;
}

describe("TerminalSlot (BSHELL door)", () => {
  it("lane unavailable → nothing", () => {
    const { container } = render(
      <MemoryRouter>
        <TerminalSlot
          sid="sid-1"
          harness="omp"
          lane={{ available: false, reason: "terminal disabled", admitted: false }}
        />
      </MemoryRouter>,
    );
    expect(container.firstChild).toBeNull();
  });

  it("available → a link to /terminals/<id> beside the state word; no panel mounts and no chunk loads", () => {
    render(
      <MemoryRouter>
        <TerminalSlot
          sid="sid-1"
          harness="omp"
          lane={{ available: true, term_id: "t1", harness: "omp", state: "working", admitted: true }}
        />
      </MemoryRouter>,
    );
    const link = screen.getByTestId("kb-term-open");
    expect(link.tagName).toBe("A");
    expect(link.getAttribute("href")).toBe("/terminals/t1");
    expect(link.textContent).toBe("Open terminal");
    expect(screen.getByTestId("kb-term-slot-state").textContent).toBe("working");
    expect(screen.getByTestId("kb-terminal-slot")).toBeTruthy();
    expect(screen.queryByTestId("mock-panel")).toBeNull();
    expect(screen.queryByTestId("kb-term-bell")).toBeNull();
    expect(mockEvalCount).toBe(0);
  });

  it("clicking the link navigates to the terminal page", () => {
    render(
      <MemoryRouter initialEntries={["/sessions/sid-1"]}>
        <Routes>
          <Route
            path="/sessions/:sid"
            element={
              <TerminalSlot
                sid="sid-1"
                harness="omp"
                lane={{ available: true, term_id: "t1", harness: "omp", state: "working", admitted: true }}
              />
            }
          />
          <Route path="/terminals/:id" element={<Where />} />
        </Routes>
      </MemoryRouter>,
    );
    fireEvent.click(screen.getByTestId("kb-term-open"));
    expect(screen.getByTestId("where").textContent).toBe("/terminals/t1");
  });

  it("child row (via root) → link text, no door, no panel", () => {
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
});
