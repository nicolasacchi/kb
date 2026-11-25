// @vitest-environment jsdom
//
// FUTERMLIST — End lives once on every panel host: above the panel on a
// desktop, in the phone sheet's header row on a phone, never both and never
// before the panel is open. The phone sheet also gets the terminal switcher.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import type { ReactNode } from "react";
import type { TermRow } from "../api/terms";

const confirmMock = vi.fn(async () => true);
vi.mock("./ConfirmProvider", () => ({
  useConfirm: () => confirmMock,
  useConfirmOptional: () => confirmMock,
}));
vi.mock("../terminal/TerminalPanel.lazy", () => ({
  __esModule: true,
  default: (props: { onClose?: () => void; headerExtra?: ReactNode }) => (
    <div data-testid="mock-panel">
      <div data-testid="mock-header-extra">{props.headerExtra}</div>
      <button type="button" data-testid="mock-panel-close" onClick={() => props.onClose?.()}>
        Close
      </button>
    </div>
  ),
}));

import { TerminalSlot } from "./TerminalSlot";
import { LaunchedTerminal } from "./LaunchedTerminal";

let posts: string[];

function row(patch: Partial<TermRow>): TermRow {
  return {
    id: "t1",
    harness: "claude",
    cwd: "/srv/a",
    started_unix: 1,
    resume_sid: null,
    pane_pid: 1,
    cols: 80,
    rows: 24,
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

function phone(on: boolean) {
  vi.stubGlobal(
    "matchMedia",
    (q: string) =>
      ({ matches: on, media: q, addEventListener() {}, removeEventListener() {}, addListener() {}, removeListener() {} }) as unknown as MediaQueryList,
  );
}

beforeEach(() => {
  posts = [];
  confirmMock.mockClear();
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string | URL, init?: RequestInit) => {
      if ((init?.method ?? "GET") === "POST") {
        posts.push(String(url));
        return new Response("{}", { status: 200, headers: { "Content-Type": "application/json" } });
      }
      return new Response(
        JSON.stringify({
          terms: [
            row({ id: "t1" }),
            row({ id: "t2", harness: "omp", cwd: "/srv/beta", blocked: true }),
            row({ id: "t3", harness: "codex", pane_dead: true }),
            row({ id: "t4", harness: "grok", stale: true }),
          ],
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }),
  );
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const wrap = (ui: ReactNode) => (
  <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    <MemoryRouter>{ui}</MemoryRouter>
  </QueryClientProvider>
);
const lane = { available: true, term_id: "t1", harness: "claude", state: "working", admitted: true } as const;

describe("TerminalSlot (BSHELL: a door, End lives on the /term page)", () => {
  it("renders no End and no panel, only the link", () => {
    phone(false);
    render(wrap(<TerminalSlot sid="s" harness="claude" lane={lane} />));
    expect(screen.queryByTestId("kb-term-end")).toBeNull();
    expect(screen.queryByTestId("mock-panel")).toBeNull();
    expect(screen.getByTestId("kb-term-open").getAttribute("href")).toBe("/terminals/t1");
  });
});

describe("LaunchedTerminal", () => {
  it("desktop: End above the panel, no switcher", async () => {
    phone(false);
    render(wrap(<LaunchedTerminal termId="t1" harness="claude" onClose={() => {}} onSwitch={() => {}} />));
    await screen.findByTestId("mock-panel");
    expect(screen.getAllByTestId("kb-term-end")).toHaveLength(1);
    expect(screen.getByTestId("launched-terminal-ctl").querySelector('[data-testid="kb-term-end"]')).toBeTruthy();
    expect(screen.queryByTestId("kb-term-switch")).toBeNull();
  });

  it("phone: the header row carries End and a switcher over the OTHER live terminals only", async () => {
    phone(true);
    const onSwitch = vi.fn();
    render(wrap(<LaunchedTerminal termId="t1" harness="claude" onClose={() => {}} onSwitch={onSwitch} />));
    await screen.findByTestId("mock-panel");
    expect(screen.queryByTestId("launched-terminal-ctl")).toBeNull();
    const extra = screen.getByTestId("mock-header-extra");
    expect(extra.querySelectorAll('[data-testid="kb-term-end"]')).toHaveLength(1);
    const sw = await waitFor(() => {
      const el = extra.querySelector('[data-testid="kb-term-switch"]') as HTMLSelectElement | null;
      if (!el) throw new Error("no switcher yet");
      return el;
    });
    const values = Array.from(sw.options).map((o) => o.value);
    // not itself (t1), not an ended (t3) or stale (t4) one
    expect(values).toEqual(["", "t2"]);
    expect(Array.from(sw.options)[1].textContent).toContain("omp");
    expect(Array.from(sw.options)[1].textContent).toContain("beta");
    fireEvent.change(sw, { target: { value: "t2" } });
    expect(onSwitch).toHaveBeenCalledWith("t2");
    expect(posts).toEqual([]);
  });

  it("phone without an onSwitch (inline launch) shows no switcher", async () => {
    phone(true);
    render(wrap(<LaunchedTerminal termId="t1" harness="claude" onClose={() => {}} />));
    await screen.findByTestId("mock-panel");
    expect(screen.queryByTestId("kb-term-switch")).toBeNull();
  });
});
