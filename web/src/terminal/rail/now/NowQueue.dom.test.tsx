// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactElement } from "react";

const live = vi.hoisted(() => ({ rows: [] as unknown[] }));
const detailMock = vi.hoisted(() => ({ detail: null as unknown }));
const phone = vi.hoisted(() => ({ on: false }));
const confirmMock = vi.hoisted(() => ({ fn: vi.fn(async () => true) }));
vi.mock("../../../hooks/useLiveSessions", () => ({ useLiveSessions: () => ({ rows: live.rows, loading: false }) }));
vi.mock("../../../hooks/useSessions", () => ({ useSessionDetail: () => ({ detail: detailMock.detail, loading: false }) }));
vi.mock("../../../hooks/useIsMobile", () => ({ usePhonePanel: () => phone.on }));
vi.mock("../../../components/ConfirmProvider", () => ({ useConfirm: () => confirmMock.fn }));

import { NowQueueSeam, STALE_CAPTION } from "./NowQueueSeam";
import { TermRailContext } from "../context";
import { termPromptsKey } from "../../prompts/promptsApi";

interface Item {
  id: string;
  text: string;
  created_at: number;
  done_at: number | null;
  order: number;
}
interface Server {
  rev: number;
  items: Item[];
  calls: { method: string; url: string; body: Record<string, unknown> | null; ifMatch: string | null }[];
  /** Force the next PATCH to answer 412. */
  stale: number;
}
let srv: Server;
let nextId = 1;

const mkItem = (text: string, done: number | null = null): Item => ({ id: `q_${String(nextId++).padStart(12, "0")}`, text, created_at: 1, done_at: done, order: 0 });
const view = () => ({ project: "demo", rev: srv.rev, items: srv.items.map((i, order) => ({ ...i, order })) });
const respond = (status: number, body: unknown) => new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });

function installFetch() {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      const method = init?.method ?? "GET";
      const body = typeof init?.body === "string" ? (JSON.parse(init.body) as Record<string, unknown>) : null;
      const h = (init?.headers ?? {}) as Record<string, string>;
      srv.calls.push({ method, url, body, ifMatch: h["If-Match"] ?? null });
      if (/\/api\/terms\/[^/]+\/(paste|enter|keys)$/.test(url)) return respond(200, {});
      const m = /\/api\/term-prompts\/demo(?:\/(q_[0-9a-f_]+))?$/.exec(url);
      if (!m) return respond(404, {});
      const id = m[1];
      if (method === "GET") return respond(200, view());
      if (method === "POST") {
        srv.items.push(mkItem(String(body?.text ?? "").trim()));
        srv.rev += 1;
        return respond(201, view());
      }
      if (srv.stale > 0 || h["If-Match"] !== `"${srv.rev}"`) {
        srv.stale = Math.max(0, srv.stale - 1);
        return respond(412, { type: "urn:kb:errors:prompts-stale", title: "Precondition Failed", detail: "changed", rev: srv.rev });
      }
      const idx = srv.items.findIndex((i) => i.id === id);
      if (idx < 0) return respond(404, { type: "urn:kb:errors:prompt-not-found", title: "Not Found", detail: "no such prompt" });
      if (method === "DELETE") srv.items.splice(idx, 1);
      else {
        const it = srv.items[idx];
        if (typeof body?.text === "string") it.text = body.text;
        if (typeof body?.done === "boolean") it.done_at = body.done ? 9 : null;
        if (typeof body?.position === "number") {
          srv.items.splice(idx, 1);
          srv.items.splice(body.position, 0, it);
        }
      }
      srv.rev += 1;
      return respond(200, view());
    }),
  );
}

let qc: QueryClient;
/** `null` = a terminal with no bound session (the default is the bound `sid-1`). */
function mount(sid: string | null = "sid-1"): ReactElement {
  return (
    <QueryClientProvider client={qc}>
      <TermRailContext.Provider value={{ termId: "t1", harness: "omp", sid: sid ?? undefined }}>
        <NowQueueSeam />
      </TermRailContext.Provider>
    </QueryClientProvider>
  );
}

const texts = () => screen.queryAllByTestId("queue-text").map((e) => e.textContent);
const apiCalls = (kind: string) => srv.calls.filter((c) => c.url.includes(kind));

beforeEach(() => {
  nextId = 1;
  srv = { rev: 0, items: [], calls: [], stale: 0 };
  live.rows = [{ session_id: "sid-1", blocked: false }];
  detailMock.detail = { project_slug: "demo" };
  phone.on = false;
  confirmMock.fn = vi.fn(async () => true);
  qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  installFetch();
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function seed(...t: string[]): void {
  for (const x of t) srv.items.push(mkItem(x));
  srv.rev = t.length;
}

describe("NowQueueSeam", () => {
  it("keeps the reserved seam identity and states its freshness", async () => {
    seed("one");
    render(mount());
    const sec = await screen.findByTestId("now-queue");
    expect(sec.getAttribute("data-seam")).toBe("pqui");
    expect(sec.hidden).toBe(false);
    expect(screen.getByTestId("now-fresh-queue").textContent).toBe("synced by the daemon");
  });

  it("lists open items in stored order, done items in a collapsed Done group", async () => {
    srv.items = [mkItem("first"), mkItem("old", 5), mkItem("second")];
    srv.rev = 3;
    render(mount());
    await waitFor(() => expect(texts()).toEqual(["first", "second"]));
    const done = screen.getByTestId("queue-done") as HTMLDetailsElement;
    expect(done.open).toBe(false);
    expect(within(done).getByText("Done (1)")).toBeTruthy();
    expect(screen.getByTestId("queue-done-text").textContent).toBe("old");
  });

  it("adds a prompt (button disabled when blank or over the byte cap)", async () => {
    render(mount());
    const add = (await screen.findByTestId("queue-add")) as HTMLButtonElement;
    expect(add.disabled).toBe(true);
    const input = screen.getByTestId("queue-input") as HTMLTextAreaElement;
    fireEvent.change(input, { target: { value: "a".repeat(8193) } });
    expect(add.disabled).toBe(true);
    expect(screen.getByTestId("queue-count").getAttribute("data-over")).toBe("1");
    fireEvent.change(input, { target: { value: "ship it" } });
    expect(add.disabled).toBe(false);
    fireEvent.click(add);
    await waitFor(() => expect(texts()).toEqual(["ship it"]));
    expect(input.value).toBe("");
    expect(apiCalls("term-prompts")[1]).toMatchObject({ method: "POST", body: { text: "ship it" } });
  });

  it("edits inline, moves, deletes (through the confirm host) and reopens", async () => {
    seed("a", "b");
    render(mount());
    await waitFor(() => expect(texts()).toEqual(["a", "b"]));
    // Edit
    fireEvent.click(screen.getAllByTestId("queue-edit")[0]);
    fireEvent.change(screen.getByTestId("queue-edit-input"), { target: { value: "a2" } });
    fireEvent.click(screen.getByTestId("queue-save"));
    await waitFor(() => expect(texts()).toEqual(["a2", "b"]));
    expect(srv.calls.at(-1)).toMatchObject({ method: "PATCH", body: { text: "a2" }, ifMatch: '"2"' });
    // Move the second item up: position = its neighbour's index.
    fireEvent.click(screen.getAllByTestId("queue-up")[1]);
    await waitFor(() => expect(texts()).toEqual(["b", "a2"]));
    expect(srv.calls.at(-1)?.body).toEqual({ position: 0 });
    // The ends cannot move.
    expect((screen.getAllByTestId("queue-up")[0] as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getAllByTestId("queue-down")[1] as HTMLButtonElement).disabled).toBe(true);
    // Delete asks the confirm host (never window.confirm).
    const winConfirm = vi.spyOn(window, "confirm");
    fireEvent.click(screen.getAllByTestId("queue-delete")[0]);
    await waitFor(() => expect(texts()).toEqual(["a2"]));
    expect(confirmMock.fn).toHaveBeenCalledTimes(1);
    expect(winConfirm).not.toHaveBeenCalled();
    // A declined confirm deletes nothing.
    confirmMock.fn.mockResolvedValueOnce(false);
    fireEvent.click(screen.getAllByTestId("queue-delete")[0]);
    await act(async () => {});
    expect(texts()).toEqual(["a2"]);
  });

  it("renders hostile prompt text as text nodes", async () => {
    const tag = "kb-prompt";
    const evil = `<img src=x onerror="alert(1)"> <template id="${tag}">x</template>`;
    seed(evil);
    const { container } = render(mount());
    await waitFor(() => expect(texts()).toEqual([evil]));
    expect(container.querySelector("img")).toBeNull();
    expect(container.querySelector("template")).toBeNull();
    expect(screen.getByTestId("queue-text").children.length).toBe(0);
  });

  it("a 412 shows the honest caption and refetches (never auto-retries an edit)", async () => {
    seed("a");
    render(mount());
    await waitFor(() => expect(texts()).toEqual(["a"]));
    srv.items[0].text = "changed elsewhere";
    srv.rev = 7;
    fireEvent.click(screen.getByTestId("queue-edit"));
    fireEvent.change(screen.getByTestId("queue-edit-input"), { target: { value: "mine" } });
    fireEvent.click(screen.getByTestId("queue-save"));
    await waitFor(() => expect(screen.getByTestId("queue-note").textContent).toBe(STALE_CAPTION));
    expect(apiCalls("term-prompts").filter((c) => c.method === "PATCH")).toHaveLength(1);
    await waitFor(() => expect(qc.getQueryData<{ rev: number }>(termPromptsKey("demo"))?.rev).toBe(7));
    expect(srv.items[0].text).toBe("changed elsewhere");
  });

  it("Send pastes one line WITHOUT Enter, then marks the item done", async () => {
    seed("first", "line one\nline two");
    render(mount());
    await waitFor(() => expect(texts()).toHaveLength(2));
    fireEvent.click(screen.getAllByTestId("queue-send")[1]);
    await waitFor(() => expect(screen.getByTestId("queue-done-text").textContent).toBe("line one\nline two"));
    const pastes = apiCalls("/paste");
    expect(pastes).toHaveLength(1);
    expect(pastes[0].body).toEqual({ text: "line one line two" });
    expect(apiCalls("/enter")).toHaveLength(0);
    expect(screen.getByTestId("queue-note").textContent).toBe("Sent as one line (line breaks removed)");
    expect(texts()).toEqual(["first"]);
  });

  it("PHONE PATH: with the phone panel on, a two-line prompt is one /paste with no CR/LF, no `enter`, no /enter request", async () => {
    phone.on = true;
    seed("alpha\r\nbeta\ngamma");
    render(mount());
    await waitFor(() => expect(texts()).toHaveLength(1));
    expect(screen.getByTestId("now-queue-body").getAttribute("data-phone")).toBe("1");
    fireEvent.click(screen.getByTestId("queue-send"));
    await waitFor(() => expect(apiCalls("/paste")).toHaveLength(1));
    const body = apiCalls("/paste")[0].body as Record<string, unknown>;
    expect(String(body.text)).not.toMatch(/[\r\n]/);
    expect("enter" in body).toBe(false);
    expect(apiCalls("/enter")).toHaveLength(0);
    expect(apiCalls("/keys")).toHaveLength(0);
  });

  it("a stale done-mark after the paste refetches, retries once and never pastes twice", async () => {
    seed("go");
    render(mount());
    await waitFor(() => expect(texts()).toEqual(["go"]));
    srv.stale = 1;
    fireEvent.click(screen.getByTestId("queue-send"));
    await waitFor(() => expect(screen.getByTestId("queue-done-text").textContent).toBe("go"));
    expect(apiCalls("/paste")).toHaveLength(1);
    expect(apiCalls("term-prompts").filter((c) => c.method === "PATCH")).toHaveLength(2);
  });

  it("a blocked session disables Send and says why", async () => {
    live.rows = [{ session_id: "sid-1", blocked: true }];
    seed("go");
    render(mount());
    await waitFor(() => expect(texts()).toEqual(["go"]));
    expect((screen.getByTestId("queue-send") as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByTestId("queue-blocked").textContent).toBe("Session is waiting on you; answer it first");
    fireEvent.click(screen.getByTestId("queue-send"));
    await act(async () => {});
    expect(apiCalls("/paste")).toHaveLength(0);
  });

  it("an unbound terminal, or a session with no valid project, makes no request", async () => {
    const { unmount } = render(mount(null));
    expect(screen.getByTestId("now-queue-unbound").textContent).toBe("Link this terminal to a project to use the queue");
    unmount();
    detailMock.detail = { project_slug: "Not A Slug" };
    render(mount("sid-1"));
    expect(screen.getByTestId("now-queue-unbound")).toBeTruthy();
    await act(async () => {});
    expect(srv.calls).toHaveLength(0);
  });

  it("shows the empty state for a project with no prompts", async () => {
    render(mount());
    expect(await screen.findByText("No saved prompts")).toBeTruthy();
  });
});
