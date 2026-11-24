// @vitest-environment jsdom
//
// HUBLAUNCH - the launcher dialog: nothing is POSTed before the confirm click,
// prefill derives harness + cwd from the fetched row only, the confirm shows
// the exact target, cwd validation agrees with the server rule, the Resume tab
// offers only resumable rows, and the layout follows the phone predicate.
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { renderToString } from "react-dom/server";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { LaunchDialog, type LaunchDialogProps } from "./LaunchDialog";
import { toast } from "../../lib/toast";
import {
  loadLastTermHarness,
  loadRecentTermCwds,
  saveLastTermHarness,
  saveRecentTermCwd,
} from "../../api/prefs";

type Call = { method: string; url: string; body: Record<string, unknown> | null };
let calls: Call[];
let launchReply: { status: number; body: unknown };
let sessionRows: Record<string, unknown>[];
let resumeRows: Record<string, unknown>[];
let details: Record<string, { status: number; body: unknown } | "throw">;
let termRows: { cwd: string }[];

const srow = (p: Record<string, unknown> = {}) => ({
  id: "i",
  session_id: "s1",
  harness: "claude",
  cwd: "/w/a",
  started_at: 1_700_000_000,
  display_name: "Fix the thing",
  project_key: "proj",
  ...p,
});

beforeAll(() => {
  if (!HTMLDialogElement.prototype.showModal) {
    HTMLDialogElement.prototype.showModal = function (this: HTMLDialogElement) {
      this.open = true;
    };
  }
});

beforeEach(() => {
  calls = [];
  launchReply = { status: 200, body: { id: "t9" } };
  sessionRows = [];
  resumeRows = [];
  details = {};
  termRows = [];
  localStorage.clear();
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: string | URL, init?: RequestInit) => {
      const url = String(input);
      const method = init?.method ?? "GET";
      calls.push({ method, url, body: init?.body ? JSON.parse(String(init.body)) : null });
      const json = (status: number, body: unknown) =>
        new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
      if (method === "POST") return json(launchReply.status, launchReply.body);
      if (url.startsWith("/api/terms")) return json(200, { terms: termRows });
      const m = /^\/api\/sessions\/([^/?]+)$/.exec(url);
      if (m) {
        const d = details[decodeURIComponent(m[1])];
        if (d === "throw") throw new Error("net down");
        if (!d) return json(404, {});
        return json(d.status, d.body);
      }
      if (url.startsWith("/api/sessions")) {
        const q = new URL(url, "http://x").searchParams.get("q");
        return json(200, { sessions: q ? resumeRows : sessionRows.length ? sessionRows : resumeRows });
      }
      return json(404, {});
    }),
  );
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

function mount(props: Partial<LaunchDialogProps> = {}) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const onClose = vi.fn();
  const onLaunched = vi.fn();
  const view = render(
    <QueryClientProvider client={qc}>
      <LaunchDialog open admitted onClose={onClose} onLaunched={onLaunched} {...props} />
    </QueryClientProvider>,
  );
  return { ...view, onClose, onLaunched };
}

const posts = () => calls.filter((c) => c.method === "POST");
const type = (v: string) => fireEvent.change(screen.getByTestId("launch-cwd"), { target: { value: v } });
const review = () => fireEvent.click(screen.getByTestId("launch-review"));
const confirm = () => fireEvent.click(screen.getByTestId("launch-confirm"));

describe("gating and layout", () => {
  it("renders nothing for a non-admitted caller or when closed", () => {
    expect(mount({ admitted: false }).container.innerHTML).toBe("");
    cleanup();
    expect(mount({ open: false }).container.innerHTML).toBe("");
    expect(calls).toHaveLength(0);
  });
  it("desktop renders a native dialog, not a Sheet", () => {
    mount();
    expect(screen.getByTestId("launch-modal").tagName).toBe("DIALOG");
    expect(document.querySelector("[data-kb-sheet]")).toBeNull();
  });
  it("phone renders the shared Sheet", () => {
    vi.stubGlobal("matchMedia", (query: string) => ({
      matches: true,
      media: query,
      addEventListener: () => {},
      removeEventListener: () => {},
    }));
    mount();
    expect(document.querySelector("[data-kb-sheet]")).not.toBeNull();
    expect(screen.queryByTestId("launch-modal")).toBeNull();
    expect(screen.getByTestId("launch-dialog")).toBeTruthy();
  });
  it("server-renders without throwing (node smoke)", () => {
    const qc = new QueryClient();
    const html = renderToString(
      <QueryClientProvider client={qc}>
        <LaunchDialog open admitted onClose={() => {}} onLaunched={() => {}} />
      </QueryClientProvider>,
    );
    expect(html).toContain("kb-launch");
  });
});

describe("nothing is sent before the Launch click", () => {
  it("counts zero POSTs through open, typing, harness change, Enter, tabs and review; one after Launch; a double click is still one", async () => {
    mount();
    expect(posts()).toHaveLength(0);
    type("/tmp/x");
    fireEvent.click(screen.getByTestId("launch-harness-codex"));
    fireEvent.submit(screen.getByTestId("launch-new-form")); // Enter in the cwd field
    expect(screen.getByTestId("launch-confirm-view")).toBeTruthy();
    expect(posts()).toHaveLength(0);
    fireEvent.click(screen.getByTestId("launch-back"));
    fireEvent.click(screen.getByTestId("launch-tab-resume"));
    fireEvent.click(screen.getByTestId("launch-tab-new"));
    expect(posts()).toHaveLength(0);
    type("/tmp/x");
    review();
    expect(posts()).toHaveLength(0);
    confirm();
    confirm();
    await waitFor(() => expect(posts()).toHaveLength(1));
    expect(posts()[0].body).toEqual({ harness: "codex", cwd: "/tmp/x" });
  });

  it("on success remembers the choice, reports the term and closes", async () => {
    const { onLaunched, onClose } = mount();
    type("/tmp/x");
    fireEvent.click(screen.getByTestId("launch-harness-grok"));
    review();
    confirm();
    await waitFor(() => expect(onLaunched).toHaveBeenCalledWith("t9", "grok"));
    expect(onClose).toHaveBeenCalled();
    expect(loadLastTermHarness()).toBe("grok");
    expect(loadRecentTermCwds()).toContain("/tmp/x");
  });

  it("a launch failure toasts, shows an alert and stays on the confirm view", async () => {
    launchReply = { status: 500, body: { title: "boom", detail: "no runner" } };
    const err = vi.spyOn(toast, "err").mockImplementation(() => {});
    const { onLaunched, onClose } = mount();
    type("/tmp/x");
    review();
    confirm();
    await waitFor(() => expect(screen.getByTestId("launch-error")).toBeTruthy());
    expect(err).toHaveBeenCalled();
    expect(screen.getByTestId("launch-confirm-view")).toBeTruthy();
    expect((screen.getByTestId("launch-confirm") as HTMLButtonElement).disabled).toBe(false);
    expect(onLaunched).not.toHaveBeenCalled();
    expect(onClose).not.toHaveBeenCalled();
  });

  it("Cancel aborts the in-flight request and does not report a launch", async () => {
    let signal: AbortSignal | undefined;
    vi.stubGlobal(
      "fetch",
      vi.fn((input: string | URL, init?: RequestInit) => {
        if ((init?.method ?? "GET") === "POST") {
          signal = init?.signal ?? undefined;
          return new Promise<Response>(() => {});
        }
        return Promise.resolve(new Response(JSON.stringify({ terms: [], sessions: [] }), { status: 200 }));
      }),
    );
    const { onLaunched, onClose } = mount();
    type("/tmp/x");
    review();
    confirm();
    await waitFor(() => expect(signal).toBeDefined());
    fireEvent.click(screen.getByTestId("launch-close"));
    expect(signal?.aborted).toBe(true);
    expect(onClose).toHaveBeenCalled();
    expect(onLaunched).not.toHaveBeenCalled();
  });
});

describe("confirm step", () => {
  it("shows the exact cwd and harness label, and Back returns without a POST", () => {
    mount();
    type("/srv/proj");
    fireEvent.click(screen.getByTestId("launch-harness-opencode"));
    review();
    expect(screen.getByTestId("launch-confirm-cwd").textContent).toBe("Directory: /srv/proj");
    expect(screen.getByTestId("launch-confirm-harness").textContent).toBe("Harness: OpenCode");
    expect(screen.queryByTestId("launch-confirm-session")).toBeNull();
    fireEvent.click(screen.getByTestId("launch-back"));
    expect(screen.getByTestId("launch-new-form")).toBeTruthy();
    expect(posts()).toHaveLength(0);
  });
});

describe("harness chips", () => {
  it("is a radiogroup, starts on the remembered choice and captions experimental ones", () => {
    saveLastTermHarness("omp");
    mount();
    const group = screen.getByRole("radiogroup");
    expect(group).toBeTruthy();
    const omp = screen.getByTestId("launch-harness-omp");
    expect(omp.getAttribute("aria-checked")).toBe("true");
    expect(screen.getByTestId("launch-harness-opencode").textContent).toContain("(experimental)");
    expect(screen.getByTestId("launch-harness-grok").textContent).toContain("(experimental)");
    expect(screen.getByTestId("launch-harness-claude").textContent).not.toContain("(experimental)");
    fireEvent.click(screen.getByTestId("launch-harness-codex"));
    expect(screen.getByTestId("launch-harness-codex").getAttribute("aria-checked")).toBe("true");
    expect(screen.getByTestId("launch-harness-omp").getAttribute("aria-checked")).toBe("false");
  });
});

describe("cwd validation parity with the server rule", () => {
  const cases: [string, string, boolean][] = [
    ["empty", "", false],
    ["512 bytes", "/" + "a".repeat(511), true],
    ["513 bytes", "/" + "a".repeat(512), false],
    ["511 bytes, multibyte", "/" + "é".repeat(255), true],
    ["513 bytes, multibyte", "/" + "é".repeat(256), false],
    ["NUL", "/a\u0000b", false],
    ["unit separator", "/a\u001fb", false],
    ["DEL", "/a\u007fb", false],
    ["NEL (C1)", "/a\u0085b", false],
    ["relative", "relative/dir", false],
    ["root", "/", true],
  ];
  it.each(cases)("%s", (_n, value, ok) => {
    mount();
    type(value);
    review();
    expect(screen.queryByTestId("launch-confirm-view") !== null).toBe(ok);
    if (!ok) expect(screen.getByRole("alert")).toBeTruthy();
    expect(posts()).toHaveLength(0);
  });
  it("trims surrounding spaces and sends the trimmed cwd", async () => {
    mount();
    type("  /tmp/x  ");
    review();
    expect(screen.getByTestId("launch-confirm-cwd").textContent).toBe("Directory: /tmp/x");
    confirm();
    await waitFor(() => expect(posts()).toHaveLength(1));
    expect(Object.keys(posts()[0].body ?? {}).sort()).toEqual(["cwd", "harness"]);
    expect(posts()[0].body?.cwd).toBe("/tmp/x");
  });
});

describe("suggestions", () => {
  it("merges recent, session (grouped by project) and live-term cwds, dropping invalid ones, with only sessions and terms fetched", async () => {
    saveRecentTermCwd("bad-relative");
    saveRecentTermCwd("/recent");
    sessionRows = [
      srow({ session_id: "a", cwd: "/old", started_at: 1, project_key: "p" }),
      srow({ session_id: "b", cwd: "/new", started_at: 9, project_key: "p" }),
      srow({ session_id: "c", cwd: "rel", started_at: 99, project_key: "q" }),
    ];
    termRows = [{ cwd: "/live" }, { cwd: "/new" }];
    mount();
    await waitFor(() => {
      const opts = [...document.querySelectorAll("datalist option")].map((o) => o.getAttribute("value"));
      expect(opts).toEqual(["/recent", "/new", "/live"]);
    });
    const opt = [...document.querySelectorAll("datalist option")].find((o) => o.getAttribute("value") === "/new");
    expect(opt?.getAttribute("label")).toBe("p");
    for (const c of calls) expect(c.url.startsWith("/api/sessions") || c.url.startsWith("/api/terms")).toBe(true);
    expect(posts()).toHaveLength(0);
  });
});

describe("resume tab", () => {
  it("offers only resumable rows, then confirms the row's own harness and cwd and sends resume_sid", async () => {
    resumeRows = [
      srow({ session_id: "ok-claude", harness: "claude", cwd: "/r/claude", display_name: "Claude one" }),
      srow({ session_id: "ok-codex", harness: "codex", cwd: "/r/codex", display_name: "Codex one" }),
      srow({ session_id: "ok-omp", harness: "omp" }),
      srow({ session_id: "ok-oc", harness: "opencode" }),
      srow({ session_id: "ok-grok", harness: "grok" }),
      srow({ session_id: "k", harness: "kimi" }),
      srow({ session_id: "nocwd", cwd: undefined }),
      srow({ session_id: "rel", cwd: "rel/dir" }),
      srow({ session_id: "-bad" }),
    ];
    mount();
    fireEvent.click(screen.getByTestId("launch-tab-resume"));
    await waitFor(() => expect(screen.getAllByTestId("launch-resume-row").length).toBe(5));
    expect(screen.getAllByTestId("launch-resume-row").map((b) => b.getAttribute("data-sid"))).toEqual([
      "ok-claude",
      "ok-codex",
      "ok-omp",
      "ok-oc",
      "ok-grok",
    ]);
    expect(posts()).toHaveLength(0);
    fireEvent.click(screen.getAllByTestId("launch-resume-row")[1]);
    expect(screen.getByTestId("launch-confirm-cwd").textContent).toBe("Directory: /r/codex");
    expect(screen.getByTestId("launch-confirm-harness").textContent).toBe("Harness: Codex");
    expect(screen.getByTestId("launch-confirm-session").textContent).toBe(
      "Resuming session: Codex one (ok-codex)",
    );
    expect(posts()).toHaveLength(0);
    confirm();
    await waitFor(() => expect(posts()).toHaveLength(1));
    expect(posts()[0].body).toEqual({ harness: "codex", cwd: "/r/codex", resume_sid: "ok-codex" });
  });

  it("debounces the search and queries with q", async () => {
    resumeRows = [srow({ session_id: "hit" })];
    mount();
    fireEvent.click(screen.getByTestId("launch-tab-resume"));
    fireEvent.change(screen.getByTestId("launch-resume-search"), { target: { value: "needle" } });
    expect(calls.some((c) => c.url.includes("q=needle"))).toBe(false);
    await waitFor(() => expect(calls.some((c) => c.url.includes("q=needle") && c.url.includes("limit=20"))).toBe(true));
  });
});

describe("prefill", () => {
  const detail = (p: Record<string, unknown> = {}) => ({
    status: 200,
    body: srow({ session_id: "sid1", harness: "codex", cwd: "/p/x", display_name: "Fix it", ...p }),
  });

  it("derives harness and cwd from the fetched row, not the defaults or the last choice", async () => {
    saveLastTermHarness("omp");
    details.sid1 = detail();
    // Extra props a caller might smuggle in are ignored by construction.
    const smuggled = { resumeSid: "sid1", harness: "grok", cwd: "/evil" } as unknown as { resumeSid: string };
    mount({ prefill: smuggled });
    expect(screen.getByTestId("launch-prefill-loading")).toBeTruthy();
    expect((screen.getByTestId("launch-review") as HTMLButtonElement).disabled).toBe(true);
    await waitFor(() => expect((screen.getByTestId("launch-review") as HTMLButtonElement).disabled).toBe(false));
    expect(posts()).toHaveLength(0);
    review();
    expect(screen.getByTestId("launch-confirm-cwd").textContent).toBe("Directory: /p/x");
    expect(screen.getByTestId("launch-confirm-harness").textContent).toBe("Harness: Codex");
    expect(screen.getByTestId("launch-confirm-session").textContent).toContain("(sid1)");
    expect(posts()).toHaveLength(0);
    confirm();
    await waitFor(() => expect(posts()).toHaveLength(1));
    expect(posts()[0].body).toEqual({ harness: "codex", cwd: "/p/x", resume_sid: "sid1" });
  });

  it.each([
    ["unknown sid (404)", undefined],
    ["fetch error", "throw" as const],
    ["legacy kimi", detail({ harness: "kimi" })],
    ["bad cwd", detail({ cwd: "relative" })],
  ])("%s shows an alert and offers no launch path", async (_n, d) => {
    if (d) details.sid1 = d;
    mount({ prefill: { resumeSid: "sid1" } });
    await waitFor(() => expect(screen.getByTestId("launch-prefill-error")).toBeTruthy());
    expect(screen.getByRole("alert")).toBeTruthy();
    const btn = screen.getByTestId("launch-review") as HTMLButtonElement;
    expect(btn.disabled).toBe(true);
    fireEvent.click(btn);
    expect(screen.queryByTestId("launch-confirm-view")).toBeNull();
    expect(screen.queryByTestId("launch-confirm")).toBeNull();
    expect(posts()).toHaveLength(0);
  });
});

describe("prefill with a non-token session id", () => {
  it("refuses to resume", async () => {
    details["-sid1"] = { status: 200, body: srow({ session_id: "-sid1", harness: "codex" }) };
    mount({ prefill: { resumeSid: "-sid1" } });
    await waitFor(() => expect(screen.getByTestId("launch-prefill-error")).toBeTruthy());
    expect((screen.getByTestId("launch-review") as HTMLButtonElement).disabled).toBe(true);
    expect(posts()).toHaveLength(0);
  });
});

describe("prefill type contract", () => {
  it("accepts only a resume sid", () => {
    // @ts-expect-error prefill carries nothing but resumeSid
    const bad: NonNullable<LaunchDialogProps["prefill"]> = { resumeSid: "s", harness: "claude" };
    expect(bad.resumeSid).toBe("s");
  });
});
