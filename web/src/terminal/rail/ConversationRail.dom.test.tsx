// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

const toastErr = vi.hoisted(() => vi.fn());
vi.mock("../../lib/toast", () => ({ toast: { err: toastErr, ok: vi.fn(), info: vi.fn() } }));
const clip = vi.hoisted(() => vi.fn());
vi.mock("../clipboard", () => ({ tryWriteClipboardText: clip }));
const detailMock = vi.hoisted(() => vi.fn());
vi.mock("../../hooks/useSessions", () => ({ useSessionDetail: detailMock }));

import { ConversationRail } from "./ConversationRail";
import { TermRailContext } from "./context";

const NOW = 1_700_000_000;
const rowsBody = {
  schema: "conversation/1",
  total: 3,
  from: 1,
  next_cursor: null,
  capture_gen: `${NOW - 300}-3`,
  unchanged: false,
  rows: [
    {
      n: 1, id: "t-aaaaaaaaaaaa", ts: null, prompt: "<img src=x onerror=alert(1)> first", answer: "done one",
      asks: [{ kind: "ask", q: "Pick?", options: ["x", "y"], answer: "x", pending: false }],
      commits: ["abcdef0123456"], files_touched: 2, errors: 0, subagents: { n: 0, errors: 0 },
    },
    { n: 2, id: "t-bbbbbbbbbbbb", ts: null, prompt: "second", answer: null, asks: [], commits: [], files_touched: 0, errors: 1, subagents: { n: 2, errors: 0 } },
    {
      n: 3, id: "t-cccccccccccc", ts: null, prompt: "third", answer: null,
      asks: [{ kind: "ask", q: "Which one now?", options: [], answer: null, pending: true }],
      commits: [], files_touched: 0, errors: 0, subagents: { n: 0, errors: 0 },
    },
  ],
};

function mockFetch(status: number, body: unknown = { kb: "demo", conversation: rowsBody }) {
  vi.stubGlobal("fetch", vi.fn(async () => ({ ok: status < 400, status, json: async () => body })));
}
function mount(sid: string | undefined) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <TermRailContext.Provider value={{ termId: "t1", harness: "claude", sid }}>
        <ConversationRail />
      </TermRailContext.Provider>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  vi.useRealTimers();
  detailMock.mockReturnValue({ detail: { source_relative: "sessions/x.html" } });
  clip.mockResolvedValue(true);
  vi.spyOn(Date, "now").mockReturnValue(NOW * 1000);
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  toastErr.mockReset();
});

describe("ConversationRail", () => {
  it("shows every row field, the pending chip and the freshness caption", async () => {
    mockFetch(200);
    mount("sid-1");
    expect(await screen.findAllByTestId("conv-row")).toHaveLength(3);
    const rows = screen.getAllByTestId("conv-row");
    expect(rows[0].textContent).toContain("done one");
    expect(rows[0].textContent).toContain("Pick?");
    expect(rows[0].textContent).toContain("x · y");
    expect(rows[0].querySelector('[data-testid="conv-commit"]')?.textContent).toBe("abcdef0");
    expect(rows[0].textContent).toContain("2 files");
    expect(rows[1].textContent).toContain("No answer yet");
    expect(rows[1].textContent).toContain("1 error");
    expect(rows[1].textContent).toContain("2 subagents");
    expect(rows[2].querySelector('[data-pending="true"]')?.textContent).toBe("Pending");
    expect(rows[0].querySelector('[data-pending="true"]')).toBeNull();
    expect(screen.getByTestId("conv-fresh").textContent).toBe("As of last capture, 5 min ago");
    expect(screen.getByText(/Live capture may be off/)).toBeTruthy();
  });

  it("HTML in a prompt stays text", async () => {
    mockFetch(200);
    const { container } = mount("sid-1");
    await screen.findAllByTestId("conv-row");
    expect(container.querySelector("img")).toBeNull();
    expect(screen.getAllByTestId("conv-row")[0].textContent).toContain("<img src=x onerror=alert(1)> first");
  });

  it("filters narrow the list and an empty filter offers Show all", async () => {
    mockFetch(200);
    mount("sid-1");
    await screen.findAllByTestId("conv-row");
    fireEvent.click(screen.getByTestId("conv-filter-questions"));
    expect(screen.getAllByTestId("conv-row").map((r) => r.getAttribute("data-turn"))).toEqual(["t-aaaaaaaaaaaa", "t-cccccccccccc"]);
    fireEvent.click(screen.getByTestId("conv-filter-answers"));
    expect(screen.getAllByTestId("conv-row")).toHaveLength(1);
    expect(screen.getByTestId("conv-filter-answers").getAttribute("aria-checked")).toBe("true");
  });

  it("a filter that empties the list shows No matching turns with Show all", async () => {
    mockFetch(200, { kb: "demo", conversation: { ...rowsBody, rows: [rowsBody.rows[1]], total: 1 } });
    mount("sid-1");
    await screen.findAllByTestId("conv-row");
    fireEvent.click(screen.getByTestId("conv-filter-questions"));
    expect(screen.getByText("No matching turns")).toBeTruthy();
    fireEvent.click(screen.getByText("Show all"));
    expect(screen.getAllByTestId("conv-row")).toHaveLength(1);
  });

  it("the jump link targets the reader at the verbatim turn id in a new tab", async () => {
    mockFetch(200);
    mount("sid-1");
    const links = await screen.findAllByTestId("conv-jump");
    expect(links[2].getAttribute("href")).toBe("/a/demo/sessions/x.html?turn=t-cccccccccccc");
    expect(links[2].getAttribute("target")).toBe("_blank");
    expect(links[2].getAttribute("rel")).toContain("noreferrer");
  });

  it("the jump link is absent while the session detail is missing", async () => {
    detailMock.mockReturnValue({ detail: null });
    mockFetch(200);
    mount("sid-1");
    await screen.findAllByTestId("conv-row");
    expect(screen.queryAllByTestId("conv-jump")).toHaveLength(0);
  });

  it("copy writes prompt + answer; a failure toasts", async () => {
    mockFetch(200);
    mount("sid-1");
    const copies = await screen.findAllByTestId("conv-copy");
    fireEvent.click(copies[0]);
    await waitFor(() => expect(clip).toHaveBeenCalledWith("<img src=x onerror=alert(1)> first\n\ndone one"));
    expect(toastErr).not.toHaveBeenCalled();
    clip.mockResolvedValue(false);
    fireEvent.click(copies[1]);
    await waitFor(() => expect(toastErr).toHaveBeenCalled());
  });

  it("window caption when the capture holds more turns than shown", async () => {
    mockFetch(200, { kb: "demo", conversation: { ...rowsBody, total: 250 } });
    mount("sid-1");
    expect((await screen.findByTestId("conv-window")).textContent).toBe("Showing last 3 of 250 turns");
  });

  it("no bound session", () => {
    mockFetch(200);
    mount(undefined);
    expect(screen.getByText("No session linked yet")).toBeTruthy();
    expect(screen.getByText(/links on the first harness beat/)).toBeTruthy();
    expect(fetch).not.toHaveBeenCalled();
  });

  it("loading, then no capture yet (404)", async () => {
    mockFetch(404, {});
    mount("sid-1");
    expect(screen.getByText("Loading conversation")).toBeTruthy();
    expect(await screen.findByText("No capture yet")).toBeTruthy();
  });

  it("zero rows", async () => {
    mockFetch(200, { kb: "demo", conversation: { ...rowsBody, rows: [], total: 0 } });
    mount("sid-1");
    expect(await screen.findByText("No turns yet")).toBeTruthy();
  });

  it("error state offers Retry, which refetches", async () => {
    mockFetch(500, {});
    mount("sid-1");
    expect(await screen.findByText("Could not load the conversation")).toBeTruthy();
    mockFetch(200);
    fireEvent.click(screen.getByText("Retry"));
    expect(await screen.findAllByTestId("conv-row")).toHaveLength(3);
  });
});
