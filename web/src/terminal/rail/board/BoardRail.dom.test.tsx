// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, renderHook, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter } from "react-router-dom";
import type { ReactNode } from "react";

const toastErr = vi.hoisted(() => vi.fn());
vi.mock("../../../lib/toast", () => ({ toast: { err: toastErr, ok: vi.fn(), info: vi.fn() } }));
const detailMock = vi.hoisted(() => vi.fn());
vi.mock("../../../hooks/useSessions", () => ({ useSessionDetail: detailMock }));

import { BoardRail } from "./BoardRail";
import { useBoardBadge } from "./useBoardBadge";
import { TermRailContext } from "../context";
import { SID, board, card, who } from "./boardRows.testkit";
import type { SlateBoardResponse } from "../../../api/slateTypes";

type Reply = { status: number; body: unknown };
let boardReply: Reply;
let postReply: Reply;
let posts: { url: string; body: unknown }[];

function res(r: Reply) {
  return {
    ok: r.status < 400,
    status: r.status,
    statusText: String(r.status),
    headers: { get: () => (r.status >= 400 ? "application/problem+json" : "application/json") },
    json: async () => r.body,
  };
}

function problem(status: number, code: string, title: string) {
  return { status, body: { type: "urn:x", title, status, detail: `${code}: x`, code } };
}

function install() {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      if (init?.method === "POST") {
        posts.push({ url, body: JSON.parse(String(init.body)) });
        return res(postReply);
      }
      return res(boardReply);
    }),
  );
}

const OK_POST: Reply = {
  status: 201,
  body: { post: { seq: 10 }, displaced: [], displaced_total: 0, nudge: null, head_seq: 10 },
};

function full(): SlateBoardResponse {
  return board(
    {
      now: [card(1, "now", { line: "doing the thing", who: who({ session_short: "8f2a" }) })],
      warn: [card(2, "warn", { line: "careful here" })],
      take: [card(3, "take", { line: "claimed x" })],
      ask: [
        card(4, "ask", { line: "agent asks agent", to: "claude" }),
        card(5, "ask", { line: "question for you", to: "you", topic: "api" }),
        card(6, "ask", { line: "already handled", to: "you", answers: 1 }),
      ],
    },
    11,
  );
}

function wrapper(sid: string | undefined) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={qc}>
      <MemoryRouter>
        <TermRailContext.Provider value={{ termId: "t1", harness: "claude", sid }}>{children}</TermRailContext.Provider>
      </MemoryRouter>
    </QueryClientProvider>
  );
}

function mount(sid: string | undefined = SID) {
  const W = wrapper(sid);
  return render(<BoardRail />, { wrapper: W });
}

beforeEach(() => {
  posts = [];
  boardReply = { status: 200, body: full() };
  postReply = OK_POST;
  detailMock.mockReturnValue({ detail: { project_slug: "demo" } });
  install();
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  toastErr.mockReset();
  detailMock.mockReset();
});

describe("BoardRail", () => {
  it("renders the sections in order with freshness and the as-of line", async () => {
    mount();
    await screen.findByTestId("board-sec-asks");
    const ids = Array.from(document.querySelectorAll("[data-testid^='board-sec-']")).map((e) => e.getAttribute("data-testid"));
    expect(ids).toEqual(["board-sec-asks", "board-sec-takes", "board-sec-warnings", "board-sec-now", "board-sec-mine"]);
    expect(screen.getByTestId("board-asof").textContent).toBe("board as of seq 11");
    expect(screen.getByTestId("board-fresh-takes").textContent).toBe("newest 2m ago");
    // the NOW line also belongs to this session (short 8f2a)
    expect(screen.getByTestId("board-sec-mine").textContent).toContain("doing the thing");
  });

  it("orders asks: operator-open first, other open, answered muted last", async () => {
    mount();
    await screen.findByTestId("board-sec-asks");
    const asks = screen.getAllByTestId("board-ask");
    expect(asks.map((a) => a.getAttribute("data-seq"))).toEqual(["5", "4", "6"]);
    expect(asks.map((a) => a.getAttribute("data-open"))).toEqual(["1", "1", "0"]);
    // only open asks carry an Answer button
    expect(asks[0].querySelector('[data-act="answer"]')).toBeTruthy();
    expect(asks[2].querySelector('[data-act="answer"]')).toBeNull();
  });

  it("an empty section says None rather than vanishing", async () => {
    boardReply = { status: 200, body: board({}, 1) };
    mount();
    await screen.findByTestId("board-sec-asks");
    expect(screen.getAllByText("None")).toHaveLength(5);
    expect(screen.getByTestId("board-fresh-asks").textContent).toBe("no posts");
  });

  it("no bound session is a calm empty state", () => {
    mount(undefined);
    expect(screen.getByText("No session linked yet")).toBeTruthy();
    expect(fetch).not.toHaveBeenCalled();
  });

  it("a session without a project slug, and a 404 slate, both read No slate for this project", async () => {
    detailMock.mockReturnValue({ detail: {} });
    mount();
    expect(screen.getByText("No slate for this project")).toBeTruthy();
    cleanup();
    detailMock.mockReturnValue({ detail: { project_slug: "demo" } });
    boardReply = problem(404, "slate-unknown", "Not Found");
    mount();
    expect(await screen.findByText("No slate for this project")).toBeTruthy();
  });

  it("a daemon failure is not dressed up as an empty slate", async () => {
    boardReply = problem(500, "x", "Internal Server Error");
    mount();
    expect(await screen.findByText("Could not load the board")).toBeTruthy();
    expect(screen.queryByText("No slate for this project")).toBeNull();
  });

  it("answers in place: posts the exact body, then closes the form", async () => {
    mount();
    await screen.findByTestId("board-sec-asks");
    fireEvent.click(screen.getAllByTestId("board-ask")[0].querySelector('[data-act="answer"]') as HTMLElement);
    const input = screen.getByTestId("board-answer-input") as HTMLInputElement;
    expect(input.maxLength).toBe(200);
    expect((screen.getByTestId("board-answer-send") as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(input, { target: { value: "  yes, go ahead  " } });
    fireEvent.click(screen.getByTestId("board-answer-send"));
    await waitFor(() => expect(posts).toHaveLength(1));
    expect(posts[0].url).toContain("/api/slates/demo/posts");
    expect(posts[0].body).toEqual({
      kind: "answer",
      line: "yes, go ahead",
      re: 5,
      topic: "api",
      prov: { harness: "spa", origin: "human" },
    });
    await waitFor(() => expect(screen.queryByTestId("board-answer-form")).toBeNull());
    expect(toastErr).not.toHaveBeenCalled();
  });

  it.each([
    ["slate-taken", problem(409, "slate-taken", "Conflict")],
    ["slate-live-author", problem(409, "slate-live-author", "Conflict")],
    ["a generic 500", problem(500, "x", "Internal Server Error")],
  ])("%s refusal surfaces via toast.err and keeps the form text", async (_n, reply) => {
    postReply = reply;
    mount();
    await screen.findByTestId("board-sec-asks");
    fireEvent.click(screen.getAllByTestId("board-ask")[0].querySelector('[data-act="answer"]') as HTMLElement);
    fireEvent.change(screen.getByTestId("board-answer-input"), { target: { value: "my answer" } });
    fireEvent.click(screen.getByTestId("board-answer-send"));
    await waitFor(() => expect(toastErr).toHaveBeenCalled());
    expect((screen.getByTestId("board-answer-input") as HTMLInputElement).value).toBe("my answer");
  });

  it("posts are data: HTML in a line renders as literal text", async () => {
    const evil = "<img src=x onerror=alert(1)><b>hi</b>";
    boardReply = { status: 200, body: board({ take: [card(3, "take", { line: evil, body: "<script>x</script>" })] }) };
    const { container } = mount();
    await screen.findByTestId("board-take");
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("b")).toBeNull();
    expect(container.querySelector("script")).toBeNull();
    expect(screen.getByTestId("board-take").textContent).toContain(evil);
    // a body would open a Markdown pane: none is offered
    expect(container.querySelector(".slate-card__bodytoggle")).toBeNull();
  });
});

describe("useBoardBadge", () => {
  const run = (sid: string | undefined = SID) => renderHook(() => useBoardBadge(), { wrapper: wrapper(sid) });

  it("is undefined at zero and without a slug", async () => {
    boardReply = { status: 200, body: board({ ask: [card(1, "ask", { to: "claude" })], take: [card(2, "take", { to: "you" })] }) };
    const { result } = run();
    await waitFor(() => expect(fetch).toHaveBeenCalled());
    await new Promise((r) => setTimeout(r, 20));
    expect(result.current).toBeUndefined();
    detailMock.mockReturnValue({ detail: {} });
    expect(run().result.current).toBeUndefined();
  });

  it("is N for N open operator-addressed asks", async () => {
    const { result } = run();
    await waitFor(() => expect(result.current).toBe(1));
    boardReply = {
      status: 200,
      body: board({ ask: [card(1, "ask", { to: "you" }), card(2, "ask", { to: "you" }), card(3, "ask", { to: "you", answers: 1 })] }),
    };
    const second = run();
    await waitFor(() => expect(second.result.current).toBe(2));
  });
});
