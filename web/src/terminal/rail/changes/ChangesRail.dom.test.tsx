// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

import { ChecksSection } from "../../../routes/sessions";
import { ChangesRail } from "./ChangesRail";
import { TermRailContext } from "../context";

const NOW = 1_700_000_000;
const SID = "sid-1";
const CODE = "https://code.test";

type Detail = Record<string, unknown>;
interface World {
  card: Record<string, unknown> | number;
  detail: Detail | null;
  files: unknown[];
  commits: unknown[];
  codeUrl: string | null;
  lane: unknown;
  fetched: string[];
}

const detailBase = (over: Detail = {}): Detail => ({
  id: "a1",
  kb: "mem",
  artifact_id: "a1",
  session_id: SID,
  started_at: NOW - 900,
  ended_at: NOW - 300,
  source_relative: "sessions/x.html",
  memory_ids: [],
  segment_chain: [],
  continued_in: [],
  checks: [
    { label: "tests", cmd: "just ci", runners: [], outcome: "pass", text: "passed", after_last_edit: true },
  ],
  evidence: { verdict: "verified", phrase: "Verified: tests passed after the last edit", checks_total: 1, checks_after_edit: 1, edits_ignored: 0, uncommitted: false },
  ...over,
});

const FILES = [
  { path: "src/a.ts", basename: "a.ts", action: "edit", in_corpus: false, via_subagent: false },
  { path: "docs/<script>x.md", basename: "<script>x.md", action: "write", in_corpus: true, kb: "mem", source_relative: "docs/n.md", via_subagent: true },
  { path: "src/r.ts", basename: "r.ts", action: "read", in_corpus: false, via_subagent: false },
];
const COMMITS = [
  { kind: "commit", sha: "abcdef0123456", subject: "<img src=x onerror=alert(1)> fix", resolved: true, trailers: [] },
  { kind: "commit", sha: "9999999aaaa", subject: "second", resolved: false, trailers: [] },
];
const LANE = {
  schema: "kbc-session-card/1",
  sid: SID,
  now_unix: NOW,
  commits: {
    available: true, total: 1, shown: 1, truncated: false,
    items: [{ repo: "demo", sha: "abcdef0123456789", visible: true, confidence: "trailer", via: "x", attributed_here: true }],
  },
};

let world: World;
const res = (status: number, body: unknown) => ({
  ok: status < 400,
  status,
  statusText: "",
  headers: { get: () => "application/json" },
  url: "",
  json: async () => body,
});

function install(): void {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: unknown) => {
      const url = String(input);
      world.fetched.push(url);
      if (url.includes("/api/session-card")) return res(200, world.lane);
      if (url.endsWith("/api/kbs")) {
        return res(200, [{ name: "mem", path: "/m", doc_count: 1, last_index_at: null, code_url: world.codeUrl }]);
      }
      if (url.endsWith(`/api/sessions/${SID}/card`)) {
        return typeof world.card === "number" ? res(world.card, { title: "boom" }) : res(200, world.card);
      }
      if (url.endsWith(`/api/sessions/${SID}/files`)) return res(200, { files: world.files });
      if (url.endsWith(`/api/sessions/${SID}/commits`)) return res(200, { commits: world.commits });
      if (url.endsWith(`/api/sessions/${SID}`)) return world.detail ? res(200, world.detail) : res(404, {});
      return res(404, {});
    }),
  );
}

let qc: QueryClient;
function mount(sid: string | undefined) {
  qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <TermRailContext.Provider value={{ termId: "t1", harness: "claude", sid }}>
        <ChangesRail />
      </TermRailContext.Provider>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  world = {
    card: { schema: "kb-session-card/1", session_id: SID },
    detail: detailBase(),
    files: FILES,
    commits: COMMITS,
    codeUrl: CODE,
    lane: structuredClone(LANE),
    fetched: [],
  };
  install();
  vi.spyOn(Date, "now").mockReturnValue(NOW * 1000);
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("ChangesRail", () => {
  it("groups files by directory, marks subagent edits, hides reads behind a toggle", async () => {
    mount(SID);
    const dirs = await screen.findAllByTestId("changes-dir");
    expect(dirs.map((d) => d.getAttribute("data-dir"))).toEqual(["docs", "src"]);
    expect(screen.getAllByTestId("changes-subagent")).toHaveLength(1);
    expect(screen.queryByText("r.ts")).toBeNull();
    fireEvent.click(screen.getByTestId("changes-toggle-reads"));
    expect(screen.getByText("r.ts")).toBeTruthy();
    expect(screen.getByTestId("changes-toggle-reads").textContent).toBe("Hide read files");
  });

  it("shows commits with sha, unresolved chip and the matched tier", async () => {
    mount(SID);
    const rows = await screen.findAllByTestId("changes-commit");
    expect(rows).toHaveLength(2);
    expect(rows[0].querySelector("code")?.textContent).toBe("abcdef0");
    expect(rows[0].querySelector('[data-testid="changes-unresolved"]')).toBeNull();
    await waitFor(() => expect(rows[0].querySelector('[data-testid="changes-tier"]')?.getAttribute("data-tier")).toBe("trailer"));
    expect(rows[1].querySelector('[data-testid="changes-unresolved"]')?.textContent).toBe("unresolved");
    expect(rows[1].querySelector('[data-testid="changes-tier"]')).toBeNull();
  });

  it("caps commits at 20 with Show all", async () => {
    world.commits = Array.from({ length: 25 }, (_, i) => ({ kind: "commit", sha: `${String(i).padStart(7, "0")}abc`, subject: `c${i}`, resolved: true, trailers: [] }));
    mount(SID);
    expect(await screen.findAllByTestId("changes-commit")).toHaveLength(20);
    fireEvent.click(screen.getByTestId("changes-show-all"));
    expect(screen.getAllByTestId("changes-commit")).toHaveLength(25);
  });

  it("verdict parity: the checks DOM is the session page's ChecksSection", async () => {
    mount(SID);
    const phrase = await screen.findByTestId("session-checks-phrase");
    const direct = render(<ChecksSection row={world.detail as never} detail={world.detail as never} />);
    const ref = direct.container.querySelector('[data-testid="session-checks-phrase"]');
    expect(ref?.textContent).toBe("Verified: tests passed after the last edit");
    expect(phrase.textContent).toBe(ref?.textContent);
    const tab = within(screen.getByTestId("changes-checks")).getByTestId("session-checks");
    expect(tab.textContent).toBe(direct.container.querySelector('[data-testid="session-checks"]')?.textContent);
  });

  it("doors exist only with a code_url; without it plain text and no code-lane request", async () => {
    mount(SID);
    const commitDoor = await screen.findByTestId("changes-commit-door");
    expect(commitDoor.getAttribute("href")).toBe(`${CODE}/r/demo/~commit/abcdef0123456789`);
    expect(commitDoor.getAttribute("target")).toBe("_blank");
    expect(commitDoor.getAttribute("rel")).toContain("noreferrer");
    expect(screen.getAllByTestId("changes-file-door")[0].getAttribute("href")).toContain(`${CODE}/search?q=`);
    cleanup();

    world.codeUrl = null;
    world.fetched = [];
    mount(SID);
    await screen.findAllByTestId("changes-commit");
    expect(screen.queryAllByTestId("changes-file-door")).toHaveLength(0);
    expect(screen.queryAllByTestId("changes-commit-door")).toHaveLength(0);
    expect(world.fetched.some((u) => u.includes("/api/session-card"))).toBe(false);
  });

  it("an invisible code-lane item gets no commit door", async () => {
    (world.lane as typeof LANE).commits.items[0] = { ...LANE.commits.items[0], visible: false } as never;
    mount(SID);
    await screen.findAllByTestId("changes-commit");
    await waitFor(() => expect(world.fetched.some((u) => u.includes("/api/session-card"))).toBe(true));
    expect(screen.queryAllByTestId("changes-commit-door")).toHaveLength(0);
  });

  it("session-page doors and the in-corpus artifact link", async () => {
    mount(SID);
    const doors = await screen.findAllByTestId("changes-page-door");
    expect(doors.length).toBeGreaterThanOrEqual(2);
    expect(doors[0].getAttribute("href")).toBe(`/sessions/${SID}`);
    expect(screen.getByTestId("changes-file-artifact").getAttribute("href")).toBe("/a/mem/docs/n.md");
  });

  it("live-only session: not-captured state, no sections, detail never fetched", async () => {
    world.card = { schema: "kb-session-card/1", session_id: SID, capture_pending: true };
    mount(SID);
    expect(await screen.findByText("Not captured yet")).toBeTruthy();
    expect(screen.getByText("Updates after the next capture.")).toBeTruthy();
    expect(screen.queryByTestId("changes-files")).toBeNull();
    expect(screen.queryByTestId("changes-commits")).toBeNull();
    expect(screen.queryByTestId("changes-checks")).toBeNull();
    expect(world.fetched.some((u) => u.endsWith(`/api/sessions/${SID}/files`))).toBe(false);
  });

  it("no bound session", () => {
    mount(undefined);
    expect(screen.getByText("No session linked yet")).toBeTruthy();
    expect(world.fetched.filter((u) => u.includes("/api/sessions"))).toHaveLength(0);
  });

  it("card error shows Retry which refetches", async () => {
    world.card = 500;
    mount(SID);
    expect(await screen.findByText("Could not load the changes")).toBeTruthy();
    world.card = { schema: "kb-session-card/1", session_id: SID };
    fireEvent.click(screen.getByText("Retry"));
    expect((await screen.findAllByTestId("changes-dir")).length).toBeGreaterThan(0);
  });

  it("captured but empty shows No changes recorded", async () => {
    world.files = [];
    world.commits = [];
    world.detail = detailBase({ evidence: undefined, checks: [] });
    mount(SID);
    expect(await screen.findByText("No changes recorded")).toBeTruthy();
  });

  it("HTML in a path or subject stays text", async () => {
    const { container } = mount(SID);
    await screen.findAllByTestId("changes-commit");
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("script")).toBeNull();
    expect(screen.getAllByTestId("changes-commit")[0].textContent).toContain("<img src=x onerror=alert(1)> fix");
    expect(screen.getByText("<script>x.md")).toBeTruthy();
  });

  it("freshness caption comes from the capture time", async () => {
    mount(SID);
    expect((await screen.findByTestId("changes-fresh")).textContent).toBe("As of last capture, 5m ago");
    expect(screen.getByText(/Live capture may be off/)).toBeTruthy();
  });

  it("the existing bridge key shapes refetch the tab and show a new commit", async () => {
    mount(SID);
    await screen.findAllByTestId("changes-commit");
    world.commits = [...COMMITS, { kind: "commit", sha: "5555555bbbb", subject: "brand new", resolved: true, trailers: [] }];
    await qc.invalidateQueries({ queryKey: ["sessions", "card", SID] });
    await qc.invalidateQueries({ queryKey: ["session", SID] });
    await waitFor(() => expect(screen.getAllByTestId("changes-commit")).toHaveLength(3));
    expect(screen.getByText("brand new")).toBeTruthy();
  });
});
