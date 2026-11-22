// @vitest-environment jsdom
//
// FUTERMLIST — the Terminals list, the End action and the phone switcher.
// The kill route is real server-side; here fetch is a recording stub.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { toast } from "../lib/toast";
import type { TermRow } from "../api/terms";

const confirmMock = vi.fn<(o: { title: string; body: unknown; confirmLabel?: string }) => Promise<boolean>>();
let confirmHost = true;
vi.mock("./ConfirmProvider", () => ({
  useConfirm: () => confirmMock,
  useConfirmOptional: () => (confirmHost ? confirmMock : null),
}));

vi.mock("../terminal/TerminalPanel.lazy", () => ({
  __esModule: true,
  default: (props: { termId: string; harness: string; onClose?: () => void; headerExtra?: unknown }) => (
    <div data-testid="mock-panel" data-term={props.termId} data-harness={props.harness}>
      <button type="button" data-testid="mock-panel-close" onClick={() => props.onClose?.()}>
        Close
      </button>
    </div>
  ),
}));

import { TermList } from "./TermList";
import { EndTerminalButton } from "./TermControls";

type Call = { method: string; url: string; intent: string | null; hasBody: boolean };
let calls: Call[];
let rows: Partial<TermRow>[];
let killReply: { status: number; body: unknown };

function row(patch: Partial<TermRow> = {}): TermRow {
  return {
    id: "t1",
    harness: "claude",
    cwd: "/srv/work/alpha",
    started_unix: 1,
    resume_sid: null,
    pane_pid: 1,
    cols: 120,
    rows: 40,
    bracket_paste_flag: false,
    pane_dead: false,
    exit_code: null,
    grid_owner: null,
    conf_version: 1,
    anchor_pid: null,
    stale: false,
    ...patch,
  } as TermRow;
}

beforeEach(() => {
  calls = [];
  rows = [row()];
  killReply = { status: 200, body: { exit_code: 0 } };
  confirmMock.mockReset();
  confirmHost = true;
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string | URL, init?: RequestInit) => {
      const method = init?.method ?? "GET";
      const headers = new Headers(init?.headers);
      calls.push({ method, url: String(url), intent: headers.get("X-Kb-Intent"), hasBody: init?.body != null });
      if (method === "POST") {
        return new Response(JSON.stringify(killReply.body), {
          status: killReply.status,
          statusText: killReply.status === 200 ? "OK" : "Err",
          headers: { "Content-Type": "application/json" },
        });
      }
      return new Response(JSON.stringify({ terms: rows.map((r) => row(r)) }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }),
  );
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

function mount(props: Partial<React.ComponentProps<typeof TermList>> = {}) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>
        <TermList openId={null} onOpen={() => {}} onCloseOpen={() => {}} {...props} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

const posts = () => calls.filter((c) => c.method === "POST");

describe("the list rows", () => {
  it("shows harness, state, owner and the bound-session link", async () => {
    rows = [row({ id: "a1", session_id: "s-1", state: "working", grid_owner: "kb-term" })];
    mount();
    const r = await screen.findByTestId("term-row");
    expect(r.textContent).toContain("claude");
    expect(r.textContent).toContain("alpha");
    expect(screen.getByTestId("term-row-state").textContent).toContain("working");
    expect(screen.getByTestId("term-row-owner").textContent).toBe("120x40 · web");
    expect(screen.getByTestId("term-row-session").getAttribute("href")).toBe("/sessions/s-1#sec-terminal");
  });

  it("an unbound terminal is marked unbound and has no session link, but can be opened", async () => {
    const onOpen = vi.fn();
    mount({ onOpen });
    await screen.findByTestId("term-row");
    expect(screen.getByTestId("term-row-unbound")).toBeTruthy();
    expect(screen.queryByTestId("term-row-session")).toBeNull();
    fireEvent.click(screen.getByTestId("term-row-open"));
    expect(onOpen).toHaveBeenCalledWith("t1");
  });

  it("renders nothing when there is no terminal and none is open", async () => {
    rows = [];
    const { container } = mount();
    await waitFor(() => expect(calls.length).toBeGreaterThan(0));
    expect(container.innerHTML).toBe("");
  });

  it("a stale row offers Remove (not Open) and says stale", async () => {
    rows = [row({ stale: true })];
    mount();
    await screen.findByTestId("term-row");
    expect(screen.getByTestId("term-row-state").textContent).toContain("stale");
    expect(screen.queryByTestId("term-row-open")).toBeNull();
    expect(screen.getByTestId("kb-term-end").textContent).toBe("Remove");
  });

  it("opening mounts the panel through the same lazy seam with the row's harness", async () => {
    rows = [row({ harness: "omp" })];
    mount({ openId: "t1" });
    const p = await screen.findByTestId("mock-panel");
    expect(p.getAttribute("data-term")).toBe("t1");
    expect(p.getAttribute("data-harness")).toBe("omp");
    fireEvent.click(screen.getByTestId("mock-panel-close"));
  });

  it("the panel's Close hands back to the page (a plain detach, no request)", async () => {
    const onCloseOpen = vi.fn();
    mount({ openId: "t1", onCloseOpen });
    await screen.findByTestId("mock-panel");
    fireEvent.click(screen.getByTestId("mock-panel-close"));
    expect(onCloseOpen).toHaveBeenCalledTimes(1);
    expect(posts()).toEqual([]);
  });

  it("an id equal to the just-launched id and not yet listed still mounts, with its harness", async () => {
    rows = [];
    mount({ openId: "zz9", launched: { id: "zz9", harness: "codex" } });
    const p = await screen.findByTestId("mock-panel");
    expect(p.getAttribute("data-harness")).toBe("codex");
  });

  it("an id that is not listed and not just launched is not mounted: caption + close", async () => {
    rows = [row({ id: "t-a" })];
    const onCloseOpen = vi.fn();
    mount({ openId: "t-zzz", onCloseOpen });
    await screen.findByTestId("term-not-found");
    expect(screen.queryByTestId("mock-panel")).toBeNull();
    expect(onCloseOpen).toHaveBeenCalled();
  });

  it("a launch hint for another id does not admit the open id", async () => {
    rows = [row({ id: "t-a" })];
    mount({ openId: "t-zzz", launched: { id: "t-other", harness: "codex" } });
    await screen.findByTestId("term-not-found");
    expect(screen.queryByTestId("mock-panel")).toBeNull();
  });

  it("after End removes the row, the sheet is not remounted for a ghost id", async () => {
    rows = [row({ id: "t1" })];
    confirmMock.mockResolvedValue(true);
    mount({ openId: "t1", launched: { id: "t1", harness: "claude" } });
    await screen.findByTestId("term-row");
    rows = [];
    fireEvent.click(screen.getAllByTestId("kb-term-end")[0]);
    await screen.findByTestId("term-not-found");
    expect(screen.queryByTestId("mock-panel")).toBeNull();
  });
});

describe("End terminal", () => {
  it("opens the confirm first and sends nothing before the answer", async () => {
    let resolve: (v: boolean) => void = () => {};
    confirmMock.mockImplementation(() => new Promise<boolean>((r) => (resolve = r)));
    mount();
    await screen.findByTestId("term-row");
    fireEvent.click(screen.getByTestId("kb-term-end"));
    await waitFor(() => expect(confirmMock).toHaveBeenCalledTimes(1));
    expect(confirmMock.mock.calls[0][0].title).toBe("End terminal");
    expect(String(confirmMock.mock.calls[0][0].body)).toContain("claude");
    expect(String(confirmMock.mock.calls[0][0].body)).toContain("/srv/work/alpha");
    expect(posts()).toEqual([]);
    resolve(false);
  });

  it("Cancel sends zero POSTs", async () => {
    confirmMock.mockResolvedValue(false);
    mount();
    await screen.findByTestId("term-row");
    fireEvent.click(screen.getByTestId("kb-term-end"));
    await waitFor(() => expect(confirmMock).toHaveBeenCalled());
    await new Promise((r) => setTimeout(r, 30));
    expect(posts()).toEqual([]);
  });

  it("Confirm sends exactly one POST /api/terms/{id}/kill with the intent header and no body", async () => {
    confirmMock.mockResolvedValue(true);
    mount();
    await screen.findByTestId("term-row");
    fireEvent.click(screen.getByTestId("kb-term-end"));
    await waitFor(() => expect(posts()).toHaveLength(1));
    expect(posts()[0].url).toBe("/api/terms/t1/kill");
    expect(posts()[0].intent).toBe("terminal");
    expect(posts()[0].hasBody).toBe(false);
  });

  it("a double click while the confirm is open asks once and kills once", async () => {
    confirmMock.mockResolvedValue(true);
    mount();
    await screen.findByTestId("term-row");
    const b = screen.getByTestId("kb-term-end");
    fireEvent.click(b);
    fireEvent.click(b);
    await waitFor(() => expect(posts()).toHaveLength(1));
    await new Promise((r) => setTimeout(r, 30));
    expect(confirmMock).toHaveBeenCalledTimes(1);
    expect(posts()).toHaveLength(1);
  });

  it("a 200 with exit_code null (the holder was already gone) is success: no toast", async () => {
    confirmMock.mockResolvedValue(true);
    killReply = { status: 200, body: { exit_code: null } };
    const err = vi.spyOn(toast, "err");
    mount();
    await screen.findByTestId("term-row");
    fireEvent.click(screen.getByTestId("kb-term-end"));
    await waitFor(() => expect(posts()).toHaveLength(1));
    await new Promise((r) => setTimeout(r, 30));
    expect(err).not.toHaveBeenCalled();
  });

  it("a failure toasts the error and leaves the row, the button and the open panel in place", async () => {
    confirmMock.mockResolvedValue(true);
    killReply = { status: 502, body: { type: "urn:kb:errors:runner", title: "Bad Gateway", detail: "runner down" } };
    const err = vi.spyOn(toast, "err");
    mount({ openId: "t1" });
    await screen.findByTestId("mock-panel");
    fireEvent.click(screen.getAllByTestId("kb-term-end")[0]);
    await waitFor(() => expect(err).toHaveBeenCalledTimes(1));
    expect(String(err.mock.calls[0][0])).toContain("runner down");
    expect(screen.getByTestId("mock-panel")).toBeTruthy();
    expect(screen.getByTestId("term-row")).toBeTruthy();
    // and the button is usable again
    await waitFor(() => expect((screen.getAllByTestId("kb-term-end")[0] as HTMLButtonElement).disabled).toBe(false));
  });

  it("a stale row's Remove confirms with its own words, then kills once", async () => {
    confirmMock.mockResolvedValue(true);
    rows = [row({ stale: true })];
    mount();
    await screen.findByTestId("term-row");
    fireEvent.click(screen.getByTestId("kb-term-end"));
    await waitFor(() => expect(posts()).toHaveLength(1));
    expect(confirmMock.mock.calls[0][0].title).toBe("Remove terminal");
    expect(posts()[0].url).toBe("/api/terms/t1/kill");
  });

  it("after a successful End the list is refetched (the SSE bridge does the same on term.ended)", async () => {
    confirmMock.mockResolvedValue(true);
    mount();
    await screen.findByTestId("term-row");
    const gets = () => calls.filter((c) => c.method === "GET").length;
    const before = gets();
    rows = [];
    fireEvent.click(screen.getByTestId("kb-term-end"));
    await waitFor(() => expect(gets()).toBeGreaterThan(before));
    await waitFor(() => expect(screen.queryByTestId("term-row")).toBeNull());
  });

  it("the client never marks the terminal ended itself: a 200 changes nothing until the list says so", async () => {
    confirmMock.mockResolvedValue(true);
    mount();
    await screen.findByTestId("term-row");
    // the refetch still reports the row as running
    fireEvent.click(screen.getByTestId("kb-term-end"));
    await waitFor(() => expect(posts()).toHaveLength(1));
    await new Promise((r) => setTimeout(r, 30));
    expect(screen.getByTestId("term-row-state").textContent).not.toContain("ended");
  });
});

describe("EndTerminalButton without a confirm host", () => {
  it("renders nothing (never a destructive control with no prompt)", () => {
    confirmHost = false;
    const { container } = render(<EndTerminalButton target={{ termId: "t1", harness: "claude" }} />);
    expect(container.innerHTML).toBe("");
  });

  it("with a host it is one End button", () => {
    const { container } = render(<EndTerminalButton target={{ termId: "t1", harness: "claude" }} />);
    expect(container.querySelectorAll('[data-testid="kb-term-end"]')).toHaveLength(1);
  });
});

// FUMINLIST — a failed list read is not an empty list; the row End is not doubled.
describe("list read failure (FUMINLIST)", () => {
  function failList() {
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string | URL, init?: RequestInit) => {
        const method = init?.method ?? "GET";
        calls.push({ method, url: String(url), intent: null, hasBody: false });
        return new Response(JSON.stringify({ title: "Bad Gateway", detail: "runner down" }), {
          status: 502,
          statusText: "Bad Gateway",
          headers: { "Content-Type": "application/json" },
        });
      }),
    );
  }

  it("with ?term= set, an error keeps the param (no close, no 'not found') and shows an error with retry", async () => {
    failList();
    const onCloseOpen = vi.fn();
    mount({ openId: "t1", onCloseOpen });
    await screen.findByTestId("term-list-error");
    await new Promise((r) => setTimeout(r, 20));
    expect(onCloseOpen).not.toHaveBeenCalled();
    expect(screen.queryByTestId("term-not-found")).toBeNull();
    expect(screen.getByTestId("term-list-error").textContent).toMatch(/cannot read the terminal list/i);
  });

  it("Retry refetches, and a good list that has the id mounts the panel", async () => {
    failList();
    const onCloseOpen = vi.fn();
    mount({ openId: "t1", onCloseOpen });
    await screen.findByTestId("term-list-error");
    const before = calls.length;
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        new Response(JSON.stringify({ terms: [row()] }), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        }),
      ),
    );
    fireEvent.click(screen.getByTestId("term-list-retry"));
    await screen.findByTestId("mock-panel");
    expect(calls.length).toBeGreaterThanOrEqual(before);
    expect(screen.queryByTestId("term-list-error")).toBeNull();
    expect(onCloseOpen).not.toHaveBeenCalled();
  });

  it("only a SUCCESSFUL list without the id says not found", async () => {
    rows = [row({ id: "t-a" })];
    const onCloseOpen = vi.fn();
    mount({ openId: "t-zzz", onCloseOpen });
    await screen.findByTestId("term-not-found");
    expect(screen.queryByTestId("term-list-error")).toBeNull();
    expect(onCloseOpen).toHaveBeenCalled();
  });

  it("with no ?term= an error stays quiet (the list simply is not shown)", async () => {
    failList();
    const { container } = mount();
    await waitFor(() => expect(calls.length).toBeGreaterThan(0));
    await new Promise((r) => setTimeout(r, 20));
    expect(container.innerHTML).toBe("");
  });
});

describe("one End per open terminal (FUMINLIST)", () => {
  it("the row of the terminal open in its panel has no End; the panel End stays; other rows keep theirs", async () => {
    rows = [row({ id: "t1" }), row({ id: "t2" })];
    mount({ openId: "t1" });
    await screen.findByTestId("mock-panel");
    const rowEls = screen.getAllByTestId("term-row");
    const r1 = rowEls.find((r) => r.getAttribute("data-term") === "t1") as HTMLElement;
    const r2 = rowEls.find((r) => r.getAttribute("data-term") === "t2") as HTMLElement;
    expect(within(r1).queryByTestId("kb-term-end")).toBeNull();
    expect(within(r2).getByTestId("kb-term-end")).toBeTruthy();
    expect(within(screen.getByTestId("launched-terminal")).getByTestId("kb-term-end")).toBeTruthy();
    expect(screen.getAllByTestId("kb-term-end")).toHaveLength(2);
  });

  it("with nothing open every row has its End", async () => {
    rows = [row({ id: "t1" }), row({ id: "t2" })];
    mount();
    await screen.findAllByTestId("term-row");
    expect(screen.getAllByTestId("kb-term-end")).toHaveLength(2);
  });
});
