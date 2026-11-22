// @vitest-environment jsdom
//
// FUWEBLAUNCH — "New terminal": gating, client-side cwd validation (no request
// for an invalid path), the exact launch body per harness, the inline panel
// right after a 2xx, honest errors, and the bounded binding poll.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { NewTerminalPanel } from "./NewTerminalPanel";
import { toast } from "../lib/toast";
import type { TermRow } from "../api/terms";

vi.mock("../terminal/TerminalPanel.lazy", () => ({
  __esModule: true,
  default: (props: { termId: string; harness: string; sid: string; onClose?: () => void }) => (
    <div data-testid="mock-panel" data-term={props.termId} data-harness={props.harness}>
      <button type="button" data-testid="mock-panel-close" onClick={() => props.onClose?.()}>
        Close
      </button>
    </div>
  ),
}));

type Call = { method: string; url: string; body: unknown; intent: string | null };
let calls: Call[];
let launchReply: { status: number; body: unknown };
let rows: Partial<TermRow>[];

function row(patch: Partial<TermRow> = {}): TermRow {
  return {
    id: "t1",
    harness: "claude",
    cwd: "/w",
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

beforeEach(() => {
  calls = [];
  launchReply = { status: 200, body: { id: "t1" } };
  rows = [];
  localStorage.clear();
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string | URL, init?: RequestInit) => {
      const method = init?.method ?? "GET";
      const headers = new Headers(init?.headers);
      calls.push({
        method,
        url: String(url),
        body: init?.body ? JSON.parse(String(init.body)) : null,
        intent: headers.get("X-Kb-Intent"),
      });
      if (method === "POST") {
        return new Response(JSON.stringify(launchReply.body), {
          status: launchReply.status,
          statusText: launchReply.status === 200 ? "OK" : "Err",
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

function mount(props: Partial<React.ComponentProps<typeof NewTerminalPanel>> = {}) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>
        <NewTerminalPanel admitted pollMs={20} {...props} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

const posts = () => calls.filter((c) => c.method === "POST");
const open = () => fireEvent.click(screen.getByTestId("new-terminal-open"));
const type = (v: string) => fireEvent.change(screen.getByTestId("new-terminal-cwd"), { target: { value: v } });
const launch = () => fireEvent.click(screen.getByTestId("new-terminal-launch"));

describe("gating", () => {
  it("renders no control when the caller is not admitted (or the tier is disabled)", () => {
    const { container } = mount({ admitted: false });
    expect(container.innerHTML).toBe("");
    expect(screen.queryByTestId("new-terminal-open")).toBeNull();
  });
  it("sends nothing until the Launch click", () => {
    mount();
    open();
    expect(calls).toEqual([]);
  });
});

describe("form buttons (FUTERMPOLISH W7)", () => {
  it("Launch is the primary button and Cancel the secondary one, both ordinary buttons (not pill links)", () => {
    mount();
    open();
    const go = screen.getByTestId("new-terminal-launch");
    const cancel = screen.getByTestId("new-terminal-cancel");
    expect(go.className).toContain("kb-ses__newterm-btn--primary");
    expect(cancel.className).toContain("kb-ses__newterm-btn");
    expect(cancel.className).not.toContain("--primary");
    expect(go.className).not.toContain("kb-ses__resume-copy");
    expect(cancel.className).not.toContain("kb-ses__resume-copy");
    expect(go.getAttribute("type")).toBe("submit");
    expect(cancel.getAttribute("type")).toBe("button");
  });
});

describe("cwd validation", () => {
  it.each([
    ["a relative path", "project/sub"],
    ["an oversize path", "/" + "a".repeat(600)],
    ["a control character", "/a\u0007b"],
    ["an empty path", ""],
  ])("%s sends no request", (_n, v) => {
    mount();
    open();
    type(v);
    launch();
    expect(posts()).toEqual([]);
    expect(screen.getByTestId("new-terminal-invalid")).toBeTruthy();
    expect(screen.queryByTestId("launched-terminal")).toBeNull();
  });
});

describe("launch body", () => {
  it.each(["claude", "codex", "omp", "opencode", "grok"])(
    "%s sends exactly {harness, cwd} with the intent header and no resume_sid",
    async (h) => {
      mount();
      open();
      fireEvent.change(screen.getByTestId("new-terminal-harness"), { target: { value: h } });
      type("/srv/work");
      launch();
      await waitFor(() => expect(posts()).toHaveLength(1));
      expect(posts()[0].url).toContain("/api/terms");
      expect(posts()[0].body).toEqual({ harness: h, cwd: "/srv/work" });
      expect(posts()[0].intent).toBe("terminal");
    },
  );
  it("trims the typed path and remembers harness + cwd for next time", async () => {
    mount();
    open();
    fireEvent.change(screen.getByTestId("new-terminal-harness"), { target: { value: "codex" } });
    type("  /srv/work  ");
    launch();
    await waitFor(() => expect(screen.getByTestId("mock-panel")).toBeTruthy());
    expect(posts()[0].body).toEqual({ harness: "codex", cwd: "/srv/work" });
    cleanup();
    mount({ sessionCwds: ["/from/session", "relative"] });
    open();
    expect((screen.getByTestId("new-terminal-harness") as HTMLSelectElement).value).toBe("codex");
    const opts = Array.from(document.querySelectorAll("datalist option")).map((o) =>
      o.getAttribute("value"),
    );
    expect(opts).toEqual(["/srv/work", "/from/session"]);
  });
  it("defaults to claude and marks grok and opencode experimental", () => {
    mount();
    open();
    expect((screen.getByTestId("new-terminal-harness") as HTMLSelectElement).value).toBe("claude");
    const labels = Array.from(
      (screen.getByTestId("new-terminal-harness") as HTMLSelectElement).options,
    ).map((o) => o.textContent);
    expect(labels.filter((l) => l?.includes("experimental"))).toHaveLength(2);
  });
});

describe("after the launch", () => {
  it("mounts the panel right after a 2xx, before any binding", async () => {
    mount();
    open();
    type("/w");
    launch();
    await waitFor(() => expect(screen.getByTestId("mock-panel")).toBeTruthy());
    expect(screen.getByTestId("mock-panel").getAttribute("data-term")).toBe("t1");
    expect(screen.getByTestId("launched-terminal-unbound").textContent).toMatch(/appears in the list/);
    expect(screen.queryByTestId("launched-terminal-bound")).toBeNull();
  });

  it("shows the session link only once the row carries session_id, and never navigates", async () => {
    mount();
    open();
    type("/w");
    launch();
    await waitFor(() => expect(screen.getByTestId("mock-panel")).toBeTruthy());
    expect(screen.queryByTestId("launched-terminal-bound")).toBeNull();
    rows = [{ id: "t1", session_id: "sess-9" }];
    const link = await screen.findByText("open this session's page", {}, { timeout: 2000 });
    expect(link.getAttribute("href")).toBe("/sessions/sess-9#sec-terminal");
    expect(screen.queryByTestId("launched-terminal-unbound")).toBeNull();
  });

  it("closing the panel returns to a closed launcher", async () => {
    mount();
    open();
    type("/w");
    launch();
    await waitFor(() => expect(screen.getByTestId("mock-panel")).toBeTruthy());
    fireEvent.click(screen.getByTestId("mock-panel-close"));
    expect(screen.queryByTestId("launched-terminal")).toBeNull();
  });
});

describe("failures are shown honestly", () => {
  it.each([400, 404, 500])("a %i launch answer shows the error, toasts and mounts no panel", async (status) => {
    launchReply = { status, body: { type: "x", title: "nope", detail: "refused" } };
    const err = vi.spyOn(toast, "err");
    mount();
    open();
    type("/w");
    launch();
    const msg = await screen.findByTestId("new-terminal-error");
    expect(msg.textContent).toMatch(/launch failed/);
    expect(err).toHaveBeenCalled();
    expect(screen.queryByTestId("launched-terminal")).toBeNull();
    // The form stays so the operator can correct and retry.
    expect(screen.getByTestId("new-terminal-launch")).toBeTruthy();
  });

  it("a dead pane shows the exit code and stops saying 'appears in the list'", async () => {
    mount();
    open();
    type("/w");
    launch();
    await waitFor(() => expect(screen.getByTestId("mock-panel")).toBeTruthy());
    rows = [{ id: "t1", pane_dead: true, exit_code: 3 }];
    const dead = await screen.findByTestId("launched-terminal-dead", {}, { timeout: 2000 });
    expect(dead.textContent).toMatch(/exit code 3/);
    expect(screen.queryByTestId("launched-terminal-unbound")).toBeNull();
  });
});

describe("onLaunched (FUTERMLIST)", () => {
  it("hands the new terminal to the page and mounts no inline panel", async () => {
    const onLaunched = vi.fn();
    mount({ onLaunched });
    open();
    type("/srv/work");
    launch();
    await waitFor(() => expect(onLaunched).toHaveBeenCalledWith("t1", "claude"));
    expect(screen.queryByTestId("launched-terminal")).toBeNull();
    expect(screen.queryByTestId("mock-panel")).toBeNull();
    expect(screen.queryByTestId("new-terminal-form")).toBeNull();
    expect(posts()).toHaveLength(1);
  });
  it("a failed launch never calls it", async () => {
    launchReply = { status: 500, body: { title: "x" } };
    const onLaunched = vi.fn();
    mount({ onLaunched });
    open();
    type("/srv/work");
    launch();
    await screen.findByTestId("new-terminal-error");
    expect(onLaunched).not.toHaveBeenCalled();
  });
});
