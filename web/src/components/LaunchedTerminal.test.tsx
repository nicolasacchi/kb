// @vitest-environment jsdom
//
// FUWEBLAUNCH — the binding poll is bounded: it invalidates the existing
// ["terms"] query on an interval, stops when the row binds or the pane dies,
// and stops for good at the deadline.
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { LaunchedTerminal } from "./LaunchedTerminal";

vi.mock("../terminal/TerminalPanel.lazy", () => ({
  __esModule: true,
  default: (p: { termId: string; sid: string }) => (
    <div data-testid="mock-panel" data-term={p.termId} data-sid={p.sid} />
  ),
}));

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function stub(rowFor: () => Record<string, unknown>) {
  const fn = vi.fn(
    async () =>
      new Response(JSON.stringify({ terms: [rowFor()] }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
  );
  vi.stubGlobal("fetch", fn);
  return fn;
}

function mount(props: Partial<React.ComponentProps<typeof LaunchedTerminal>> = {}) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>
        <LaunchedTerminal termId="t1" harness="claude" onClose={() => {}} {...props} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

describe("LaunchedTerminal poll", () => {
  it("polls while unbound, then stops at the deadline", async () => {
    const fetchFn = stub(() => ({ id: "t1", pane_dead: false }));
    mount({ pollMs: 20, deadlineMs: 120 });
    await waitFor(() => expect(fetchFn.mock.calls.length).toBeGreaterThan(2), { timeout: 2000 });
    await waitFor(
      () => expect(screen.getByTestId("launched-terminal-unbound").textContent).toMatch(/No session has appeared/),
      { timeout: 2000 },
    );
    const settled = fetchFn.mock.calls.length;
    await sleep(150);
    expect(fetchFn.mock.calls.length).toBe(settled);
  });

  it("stops polling once the row binds", async () => {
    let bound = false;
    const fetchFn = stub(() => ({ id: "t1", session_id: bound ? "s-1" : null }));
    mount({ pollMs: 20, deadlineMs: 5000 });
    await waitFor(() => expect(fetchFn.mock.calls.length).toBeGreaterThan(1), { timeout: 2000 });
    bound = true;
    await screen.findByTestId("launched-terminal-bound", {}, { timeout: 2000 });
    expect(screen.getByTestId("mock-panel").getAttribute("data-sid")).toBe("s-1");
    const settled = fetchFn.mock.calls.length;
    await sleep(120);
    expect(fetchFn.mock.calls.length).toBe(settled);
  });

  it("a different bound session id (resume into a new id) is the one linked", async () => {
    stub(() => ({ id: "t1", resume_sid: "old", session_id: "new-id" }));
    mount({ pollMs: 20, sid: "old" });
    const link = await screen.findByText("open this session's page", {}, { timeout: 2000 });
    expect(link.getAttribute("href")).toBe("/sessions/new-id#sec-terminal");
  });
});
