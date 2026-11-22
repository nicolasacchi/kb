// @vitest-environment jsdom
//
// FUWEBLAUNCH — the binding poll is bounded: it invalidates the existing
// ["terms"] query on an interval, stops when the row binds or the pane dies,
// and stops for good at the deadline.
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { LaunchedTerminal } from "./LaunchedTerminal";
import { BOUND_DEADLINE_MS, BOUND_POLL_MS } from "../lib/termLaunch";

vi.mock("../terminal/TerminalPanel.lazy", () => ({
  __esModule: true,
  default: (p: { termId: string; sid: string }) => (
    <div data-testid="mock-panel" data-term={p.termId} data-sid={p.sid} />
  ),
}));

afterEach(() => {
  vi.useRealTimers();
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

// Only the interval and the clock are faked: promises and setTimeout stay real,
// so findBy/waitFor keep working while the poll advances on a virtual clock.
function fakeClock() {
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval", "Date"] });
}
/** Poll a condition on REAL setTimeout (RTL's waitFor leans on setInterval, which is faked here). */
async function until(cond: () => void, ms = 3000) {
  const end = performance.now() + ms;
  for (;;) {
    try {
      cond();
      return;
    } catch (e) {
      if (performance.now() > end) throw e;
      await act(async () => {
        await new Promise((r) => setTimeout(r, 5));
      });
    }
  }
}
/** Advance the virtual clock, then hop one real macrotask so refetches start. */
async function advance(ms: number) {
  await act(async () => {
    vi.advanceTimersByTime(ms);
    await new Promise((r) => setTimeout(r, 0));
  });
}

describe("LaunchedTerminal poll", () => {
  it("polls every BOUND_POLL_MS while unbound and stops at BOUND_DEADLINE_MS", async () => {
    fakeClock();
    const fetchFn = stub(() => ({ id: "t1", pane_dead: false }));
    mount();
    await until(() => expect(screen.getByTestId("launched-terminal-unbound")).toBeTruthy());
    await until(() => expect(fetchFn.mock.calls.length).toBe(1));
    // Just under one period: nothing yet. On the period: exactly one more read.
    await advance(BOUND_POLL_MS - 1);
    expect(fetchFn.mock.calls.length).toBe(1);
    await advance(1);
    await until(() => expect(fetchFn.mock.calls.length).toBe(2));
    // Up to just before the deadline the honest "still waiting" text stays.
    await advance(BOUND_DEADLINE_MS - 2 * BOUND_POLL_MS);
    expect(screen.getByTestId("launched-terminal-unbound").textContent).toMatch(/appears in the list/);
    expect(screen.getByTestId("launched-terminal-unbound").textContent).not.toMatch(/No session has appeared/);
    await advance(BOUND_POLL_MS);
    await until(() => expect(screen.getByTestId("launched-terminal-unbound").textContent).toMatch(/No session has appeared/));
    const settled = fetchFn.mock.calls.length;
    await advance(10 * BOUND_POLL_MS);
    expect(fetchFn.mock.calls.length).toBe(settled);
  });

  it("stops polling once the row binds", async () => {
    fakeClock();
    let bound = false;
    const fetchFn = stub(() => ({ id: "t1", session_id: bound ? "s-1" : null }));
    mount();
    await until(() => expect(screen.getByTestId("launched-terminal-unbound")).toBeTruthy());
    await advance(BOUND_POLL_MS);
    await until(() => expect(fetchFn.mock.calls.length).toBeGreaterThan(1));
    bound = true;
    await advance(BOUND_POLL_MS);
    await until(() => expect(screen.getByTestId("launched-terminal-bound")).toBeTruthy());
    expect(screen.getByTestId("mock-panel").getAttribute("data-sid")).toBe("s-1");
    const settled = fetchFn.mock.calls.length;
    await advance(10 * BOUND_POLL_MS);
    expect(fetchFn.mock.calls.length).toBe(settled);
  });

  it("a different bound session id (resume into a new id) is the one linked", async () => {
    stub(() => ({ id: "t1", resume_sid: "old", session_id: "new-id" }));
    mount({ sid: "old" });
    const link = await screen.findByText("open this session's page");
    expect(link.getAttribute("href")).toBe("/sessions/new-id#sec-terminal");
  });
});

// FUMINLIST — an unreadable list is not "unbound".
describe("LaunchedTerminal list read failure", () => {
  it("says it cannot read the terminal list (not the unbound caption) and Retry re-reads it", async () => {
    let ok = false;
    const fn = vi.fn(async () =>
      ok
        ? new Response(JSON.stringify({ terms: [{ id: "t1", session_id: "s-9" }] }), { status: 200 })
        : new Response(JSON.stringify({ title: "Bad Gateway", detail: "down" }), {
            status: 502,
            statusText: "Bad Gateway",
            headers: { "Content-Type": "application/json" },
          }),
    );
    vi.stubGlobal("fetch", fn);
    mount();
    const err = await screen.findByTestId("launched-terminal-list-error");
    expect(err.textContent).toMatch(/cannot read the terminal list/i);
    expect(screen.queryByTestId("launched-terminal-unbound")).toBeNull();
    ok = true;
    fireEvent.click(screen.getByTestId("launched-terminal-list-retry"));
    await screen.findByTestId("launched-terminal-bound");
    expect(screen.queryByTestId("launched-terminal-list-error")).toBeNull();
  });
});
