// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

const toastErr = vi.hoisted(() => vi.fn());
const toastOk = vi.hoisted(() => vi.fn());
vi.mock("../../../lib/toast", () => ({ toast: { err: toastErr, ok: toastOk, info: vi.fn() } }));
const detailMock = vi.hoisted(() => vi.fn());
vi.mock("../../../hooks/useSessions", () => ({ useSessionDetail: detailMock }));

import { KnowledgeRail } from "./KnowledgeRail";
import { TermRailContext } from "../context";

const EVIL = "<img src=x onerror=alert(1)>";

type Resp = { status: number; body?: unknown };
interface Routes {
  recalls: Resp;
  memories: Resp;
  research: Resp;
  recollect: Resp;
  recall: Resp;
  paste: Resp;
}
const OK = (body: unknown): Resp => ({ status: 200, body });
function defaults(): Routes {
  return {
    recalls: OK({
      recalls: [
        { kb: "mem", id: "a", title: EVIL, source_relative: "m/a.html", used: true, pos: 1 },
        { kb: "mem", id: "b", title: "second", source_relative: "m/b.html", used: false },
        { kb: "mem", id: "c", used: false },
      ],
    }),
    memories: OK({ memories: [{ id: "w", kb: "mem", title: "written one", path: "/x", source_relative: "m/w.html" }] }),
    research: OK({ research: [{ kind: "web", query: "how to x" }, { kind: "artifact_open", query: "docs/a.md" }] }),
    recollect: OK({
      ms: 1,
      sessions: [
        { session_id: "sid-1", kb: "s", display_name: "self", age_days: 0, stale: false, committed: false },
        { session_id: "sid-9", kb: "s", display_name: "older run", age_days: 4, stale: true, committed: true, summary: "did it" },
      ],
    }),
    recall: OK({ ms: 1, hits: [{ id: "h1", kb: "mem", title: "hit one", source_relative: "m/h1.html", summary: "line1\nline2" }] }),
    paste: OK({}),
  };
}
let routes: Routes;
let calls: Array<{ url: string; init?: RequestInit }>;

function installFetch() {
  calls = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      calls.push({ url, init });
      const key: keyof Routes = url.includes("/paste")
        ? "paste"
        : url.includes("/recalls")
          ? "recalls"
          : url.includes("/memories")
            ? "memories"
            : url.includes("/research")
              ? "research"
              : url.includes("/recollect")
                ? "recollect"
                : "recall";
      const r = routes[key];
      return {
        ok: r.status < 400,
        status: r.status,
        statusText: "err",
        json: async () => r.body ?? { detail: "refused" },
      };
    }),
  );
}
function mount(sid: string | undefined) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <TermRailContext.Provider value={{ termId: "t1", harness: "codex", sid }}>
        <KnowledgeRail />
      </TermRailContext.Provider>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  routes = defaults();
  installFetch();
  detailMock.mockReturnValue({ detail: { first_user_prompt: "build the thing", project_key: "proj" } });
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  toastErr.mockReset();
  toastOk.mockReset();
});

const lane = (id: string) => screen.getByTestId(`kn-lane-${id}`);

describe("KnowledgeRail lanes", () => {
  it("injected lane shows rank only when recorded, else rank unknown, and keeps unresolved rows", async () => {
    mount("sid-1");
    await waitFor(() => expect(within(lane("injected")).getAllByTestId("kn-row")).toHaveLength(3));
    const ranks = within(lane("injected")).getAllByTestId("kn-rank").map((e) => e.textContent);
    expect(ranks).toEqual(["#1", "rank unknown", "rank unknown"]);
    expect(within(lane("injected")).getByText("(memory no longer resolves)")).toBeTruthy();
    expect(within(lane("injected")).getAllByTestId("kn-used")).toHaveLength(1);
    expect(screen.getByTestId("kn-fresh-injected").textContent).toBe("From last capture · recall ledger");
  });

  it("written lane lists session memories with an open link", async () => {
    mount("sid-1");
    await waitFor(() => expect(within(lane("written")).getAllByTestId("kn-row")).toHaveLength(1));
    const a = within(lane("written")).getByTestId("kn-open");
    expect(a.getAttribute("href")).toBe("/a/mem/m/w.html");
    expect(a.getAttribute("target")).toBe("_blank");
    expect(screen.getByTestId("kn-fresh-written").textContent).toContain("session memories");
  });

  it("research lane groups rows as plain text with a title attribute", async () => {
    mount("sid-1");
    await waitFor(() => expect(screen.getAllByTestId("kn-research-group")).toHaveLength(2));
    expect(within(lane("research")).queryAllByRole("link")).toHaveLength(0);
    expect(screen.getByText("docs/a.md").getAttribute("title")).toBe("docs/a.md");
  });

  it("similar lane drops the bound session and links the others", async () => {
    mount("sid-1");
    await waitFor(() => expect(screen.getAllByTestId("kn-similar-row")).toHaveLength(1));
    expect(screen.getByText("older run")).toBeTruthy();
    expect(screen.getByTestId("kn-similar-open").getAttribute("href")).toBe("/sessions/sid-9");
    expect(screen.getByText("stale")).toBeTruthy();
    const url = calls.find((c) => c.url.includes("/recollect"))!.url;
    expect(url).toContain("q=build+the+thing");
    expect(url).toContain("project=proj");
  });

  it("empty lanes say Nothing yet", async () => {
    routes.recalls = OK({ recalls: [] });
    routes.memories = OK({ memories: [] });
    routes.research = OK({ research: [] });
    mount("sid-1");
    expect(await screen.findByTestId("kn-state-injected-empty")).toBeTruthy();
    expect(await screen.findByTestId("kn-state-written-empty")).toBeTruthy();
    expect(await screen.findByTestId("kn-state-research-empty")).toBeTruthy();
  });

  it("404 shows Not captured yet instead of an empty list", async () => {
    routes.recalls = { status: 404 };
    mount("sid-1");
    const s = await screen.findByTestId("kn-state-injected-not-captured");
    expect(s.textContent).toContain("Not captured yet");
    expect(s.textContent).toContain("Appears after the next capture");
  });

  it("a failing lane errors alone and Retry recovers only it", async () => {
    routes.recalls = { status: 500 };
    mount("sid-1");
    const err = await screen.findByTestId("kn-state-injected-error");
    await waitFor(() => expect(within(lane("written")).getAllByTestId("kn-row")).toHaveLength(1));
    expect(within(lane("research")).getAllByTestId("kn-research-row")).toHaveLength(2);
    expect(screen.getAllByTestId("kn-similar-row")).toHaveLength(1);
    const before = calls.length;
    routes.recalls = defaults().recalls;
    fireEvent.click(within(err).getByText("Retry"));
    await waitFor(() => expect(within(lane("injected")).getAllByTestId("kn-row")).toHaveLength(3));
    const newCalls = calls.slice(before).map((c) => c.url);
    expect(newCalls.every((u) => u.includes("/recalls"))).toBe(true);
  });

  it("without a session the session lanes say so and search still queries", async () => {
    mount(undefined);
    for (const id of ["injected", "written", "similar", "research"]) {
      expect(within(lane(id)).getByText("No session linked yet")).toBeTruthy();
    }
    await waitFor(() => expect(within(lane("search")).getAllByTestId("kn-row")).toHaveLength(1));
    expect(screen.getByTestId("kn-fresh-search").textContent).toBe("Live search · memory recall");
    expect(calls.some((c) => c.url.includes("/api/sessions/"))).toBe(false);
  });
});

describe("KnowledgeRail rendering and paste", () => {
  it("renders memory text as text, never as markup", async () => {
    const { container } = mount("sid-1");
    await waitFor(() => expect(within(lane("injected")).getAllByTestId("kn-row")).toHaveLength(3));
    expect(container.querySelector("img")).toBeNull();
    expect(within(lane("injected")).getByText(EVIL)).toBeTruthy();
  });

  it("paste posts one single-line body with no enter key", async () => {
    mount("sid-1");
    await waitFor(() => expect(within(lane("search")).getAllByTestId("kn-row")).toHaveLength(1));
    fireEvent.click(within(lane("search")).getByTestId("kn-paste"));
    await waitFor(() => expect(toastOk).toHaveBeenCalledWith("Pasted without Enter"));
    const pastes = calls.filter((c) => c.url.includes("/api/terms/t1/paste"));
    expect(pastes).toHaveLength(1);
    expect(pastes[0].init?.method).toBe("POST");
    const body = JSON.parse(String(pastes[0].init?.body)) as Record<string, unknown>;
    expect(Object.keys(body)).toEqual(["text"]);
    expect(String(body.text)).not.toMatch(/[\n\r]/);
    expect(body.text).toBe("[hit one] (mem/m/h1.html)");
  });

  it("a refused paste raises toast.err with the server detail", async () => {
    routes.paste = { status: 422, body: { detail: "multi-line text refused" } };
    mount("sid-1");
    await waitFor(() => expect(within(lane("search")).getAllByTestId("kn-row")).toHaveLength(1));
    fireEvent.click(within(lane("search")).getByTestId("kn-paste"));
    await waitFor(() => expect(toastErr).toHaveBeenCalledWith("multi-line text refused"));
    expect(toastOk).not.toHaveBeenCalled();
  });

  it("a typed query debounces into one search with the chosen scope", async () => {
    mount("sid-1");
    await waitFor(() => expect(calls.some((c) => c.url.includes("/api/memory/recall"))).toBe(true));
    fireEvent.click(screen.getByTestId("kn-scope-global"));
    fireEvent.change(screen.getByTestId("kn-search-input"), { target: { value: "needle" } });
    await waitFor(() =>
      expect(calls.some((c) => c.url.includes("q=needle") && c.url.includes("scope=global"))).toBe(true),
    );
  });
});
