// @vitest-environment jsdom
//
// FUWEBLAUNCH — "Resume in a kb terminal": the four-way gate, the exact
// resume body, the inline panel, and honest errors.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import { ResumeInTerminal } from "./ResumeInTerminal";
import { toast } from "../lib/toast";

vi.mock("../terminal/TerminalPanel.lazy", () => ({
  __esModule: true,
  default: (p: { termId: string; harness: string }) => (
    <div data-testid="mock-panel" data-term={p.termId} data-harness={p.harness} />
  ),
}));

let posts: { body: unknown; intent: string | null }[];
let reply: { status: number; body: unknown };
let rows: Record<string, unknown>[];

beforeEach(() => {
  posts = [];
  reply = { status: 200, body: { id: "t7" } };
  rows = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (_u: string | URL, init?: RequestInit) => {
      if (init?.method === "POST") {
        posts.push({
          body: JSON.parse(String(init.body)),
          intent: new Headers(init.headers).get("X-Kb-Intent"),
        });
        return new Response(JSON.stringify(reply.body), {
          status: reply.status,
          headers: { "Content-Type": "application/json" },
        });
      }
      return new Response(JSON.stringify({ terms: rows }), {
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

type P = React.ComponentProps<typeof ResumeInTerminal>;
function mount(over: Partial<P> = {}) {
  const props: P = {
    sessionId: "sid-1",
    harness: "claude",
    cwd: "/srv/w",
    terminalAdmitted: true,
    hasLiveTerminal: false,
    ...over,
  };
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>
        <ResumeInTerminal pollMs={20} {...props} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}
const btn = () => screen.queryByTestId("session-resume-term-go");

describe("gate", () => {
  it("shows the button when admitted, cwd known, harness launchable, no live terminal", () => {
    mount();
    expect(btn()).toBeTruthy();
  });
  it("hides it when not admitted or the tier is disabled", () => {
    mount({ terminalAdmitted: false });
    expect(btn()).toBeNull();
  });
  it("hides it when the session already has a live terminal", () => {
    mount({ hasLiveTerminal: true });
    expect(btn()).toBeNull();
  });
  it("hides it without a cwd, with an unusable cwd, or for a harness outside the set", () => {
    mount({ cwd: null });
    expect(btn()).toBeNull();
    cleanup();
    mount({ cwd: "relative/dir" });
    expect(btn()).toBeNull();
    cleanup();
    mount({ harness: "kimi" });
    expect(btn()).toBeNull();
  });
  it("sends nothing before the click", () => {
    mount();
    expect(posts).toEqual([]);
  });
});

describe("launch", () => {
  it.each(["claude", "codex", "omp", "opencode", "grok"])(
    "%s resumes with resume_sid = the session id, never a bare continue",
    async (h) => {
      mount({ harness: h });
      fireEvent.click(btn()!);
      await waitFor(() => expect(posts).toHaveLength(1));
      expect(posts[0].body).toEqual({ harness: h, cwd: "/srv/w", resume_sid: "sid-1" });
      expect(posts[0].intent).toBe("terminal");
      expect(JSON.stringify(posts[0].body)).not.toMatch(/continue|"-c"/);
    },
  );

  it("mounts the panel after a 2xx and links the bound session (possibly a new id)", async () => {
    mount();
    fireEvent.click(btn()!);
    const panel = await screen.findByTestId("mock-panel");
    expect(panel.getAttribute("data-term")).toBe("t7");
    expect(btn()).toBeNull();
    expect(screen.queryByTestId("launched-terminal-bound")).toBeNull();
    rows = [{ id: "t7", session_id: "sid-2" }];
    const link = await screen.findByText("open this session's page", {}, { timeout: 2000 });
    expect(link.getAttribute("href")).toBe("/sessions/sid-2#sec-terminal");
  });

  it.each([400, 404, 500])("a %i answer shows the error, toasts, mounts no panel", async (status) => {
    reply = { status, body: { title: "nope", detail: "x" } };
    const err = vi.spyOn(toast, "err");
    mount();
    fireEvent.click(btn()!);
    const e = await screen.findByTestId("session-resume-term-error");
    expect(e.textContent).toMatch(/launch failed/);
    expect(err).toHaveBeenCalled();
    expect(screen.queryByTestId("mock-panel")).toBeNull();
    expect(btn()).toBeTruthy();
  });

  it("a dead pane is reported with its exit code", async () => {
    mount();
    fireEvent.click(btn()!);
    await screen.findByTestId("mock-panel");
    rows = [{ id: "t7", pane_dead: true, exit_code: 1 }];
    const dead = await screen.findByTestId("launched-terminal-dead", {}, { timeout: 2000 });
    expect(dead.textContent).toMatch(/exit code 1/);
  });

  it("keeps the panel mounted when the card flips to 'live terminal' after the launch", async () => {
    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const base: P = {
      sessionId: "sid-1",
      harness: "claude",
      cwd: "/srv/w",
      terminalAdmitted: true,
      hasLiveTerminal: false,
    };
    const tree = (live: boolean) => (
      <QueryClientProvider client={qc}>
        <MemoryRouter>
          <ResumeInTerminal pollMs={20} {...base} hasLiveTerminal={live} />
        </MemoryRouter>
      </QueryClientProvider>
    );
    const { rerender } = render(tree(false));
    fireEvent.click(btn()!);
    await screen.findByTestId("mock-panel");
    rerender(tree(true));
    expect(screen.getByTestId("mock-panel")).toBeTruthy();
  });
});
