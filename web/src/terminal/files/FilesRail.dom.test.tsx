// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { toast } from "../../lib/toast";
import { TermRailContext } from "../rail/context";
import { FilesRail } from "./FilesRail";
import { FILE_MAX_BYTES, type TransportRequest, type TransportResponse } from "./filesApi";
import { resetRecent } from "./recentStore";
import { resetUploads, setUploadTransport } from "./uploadRunner";

const TID = "t1";
const kbOf = (over: Record<string, unknown> = {}) => ({ status: "stored", reason: null, id: "id1", stub_rel: "files/a.html", created: true, inlined: false, ...over });
const answer = (name: string, kb = kbOf()) => ({ path: `/run/up/${name}`, name, bytes: 3, kind: "other", sha256: `sha-${name}`, mime: null, kb });
const ok = (body: unknown): TransportResponse => ({ status: 200, statusText: "OK", text: JSON.stringify(body) });
const file = (name: string, size = 3): File => new File([new Uint8Array(size)], name, { type: "application/pdf" });
const ls = (dir: string, entries: unknown[], over: Record<string, unknown> = {}) => ({ root: "cwd", dir, entries, truncated: false, skipped: 0, ...over });
const ent = (name: string, kind = "file", over: Record<string, unknown> = {}) => ({ name, kind, bytes: 12, mtime_unix: 1_700_000_000, hidden: false, ...over });
const json = (status: number, body: unknown) => new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });

let fetchMock: ReturnType<typeof vi.fn>;
let routes: (url: string, init?: RequestInit) => Response | Promise<Response>;

function mount(qc = new QueryClient({ defaultOptions: { queries: { retry: false } } })) {
  const ui = (
    <QueryClientProvider client={qc}>
      <TermRailContext.Provider value={{ termId: TID, harness: "claude" }}>
        <FilesRail />
      </TermRailContext.Provider>
    </QueryClientProvider>
  );
  const r = render(ui);
  return { ...r, qc, remount: () => { r.unmount(); return render(ui); } };
}

const flush = async (): Promise<void> => {
  await act(async () => {
    for (let i = 0; i < 40; i += 1) await Promise.resolve();
  });
};
const pick = async (...files: File[]): Promise<void> => {
  const input = screen.getByTestId("files-input") as HTMLInputElement;
  Object.defineProperty(input, "files", { value: files, configurable: true });
  await act(async () => {
    fireEvent.change(input);
  });
  await flush();
};

beforeEach(() => {
  routes = (u) => (u.includes("/ls?") ? json(200, ls("/w", [])) : json(200, {}));
  fetchMock = vi.fn(async (u: string, init?: RequestInit) => routes(u, init));
  vi.stubGlobal("fetch", fetchMock);
});
afterEach(() => {
  cleanup();
  resetUploads();
  resetRecent();
  setUploadTransport(undefined);
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("upload", () => {
  it("queues files one at a time with per-file progress, then shows each result", async () => {
    const gates: Array<(r: TransportResponse) => void> = [];
    const seen: TransportRequest[] = [];
    setUploadTransport((req) => {
      seen.push(req);
      req.onProgress?.(1, 3);
      return new Promise((res) => gates.push(res));
    });
    mount();
    await pick(file("a.pdf"), file("b.json"));
    expect(seen).toHaveLength(1);
    const rows = screen.getAllByTestId("files-upload-row");
    expect(rows[0].dataset.status).toBe("uploading");
    expect(rows[1].dataset.status).toBe("queued");
    expect(within(rows[0]).getByTestId("files-progress")).toHaveProperty("value", 33);
    await act(async () => gates[0](ok(answer("a.pdf"))));
    await flush();
    expect(seen).toHaveLength(2);
    expect(seen[0].url).toContain("name=a.pdf");
    expect(seen[1].url).toContain("name=b.json");
    await act(async () => gates[1](ok(answer("b.json"))));
    await flush();
    expect(within(screen.getByTestId("files-upload-list")).getAllByTestId("files-runner-path").map((n) => n.textContent)).toEqual(["/run/up/a.pdf", "/run/up/b.json"]);
  });

  it("sends index=1 only when the box is ticked, and the box starts off", async () => {
    const urls: string[] = [];
    setUploadTransport(async (req) => (urls.push(req.url), ok(answer("a.pdf"))));
    mount();
    expect((screen.getByTestId("files-index") as HTMLInputElement).checked).toBe(false);
    expect(screen.getByText(/unscrubbed/)).toBeTruthy();
    await pick(file("a.pdf"));
    fireEvent.click(screen.getByTestId("files-index"));
    await pick(file("b.pdf"));
    expect(urls[0]).not.toContain("index=1");
    expect(urls[1]).toContain("index=1");
  });

  it("cancelling the in-flight file aborts it, shows cancelled, and the next file still goes", async () => {
    const signals: AbortSignal[] = [];
    setUploadTransport((req) => {
      signals.push(req.signal!);
      if (signals.length === 2) return Promise.resolve(ok(answer("b.json")));
      return new Promise((_, rej) => req.signal!.addEventListener("abort", () => rej(new DOMException("aborted", "AbortError"))));
    });
    const err = vi.spyOn(toast, "err");
    mount();
    await pick(file("a.pdf"), file("b.json"));
    await act(async () => fireEvent.click(within(screen.getAllByTestId("files-upload-row")[0]).getByText("Cancel")));
    await flush();
    expect(signals[0].aborted).toBe(true);
    expect(screen.getByTestId("files-cancelled").textContent).toBe("cancelled");
    expect(screen.getAllByTestId("files-upload-row")[1].dataset.status).toBe("done");
    expect(err).not.toHaveBeenCalled();
  });

  it("cancel then the request still resolves: the row is done + late, listed in Recent exactly once", async () => {
    let gate!: (r: TransportResponse) => void;
    setUploadTransport(() => new Promise((res) => (gate = res)));
    mount();
    await pick(file("a.pdf"));
    await act(async () => fireEvent.click(within(screen.getAllByTestId("files-upload-row")[0]).getByText("Cancel")));
    expect(screen.getAllByTestId("files-upload-row")[0].dataset.status).toBe("cancelled");
    await act(async () => gate(ok(answer("a.pdf"))));
    await flush();
    const row = screen.getAllByTestId("files-upload-row")[0];
    expect(row.dataset.status).toBe("done");
    expect(screen.queryByTestId("files-cancelled")).toBeNull();
    expect(within(row).getByTestId("files-late").textContent).toBe("Cancelled too late: the file was stored.");
    expect(within(row).getByTestId("files-runner-path").textContent).toBe("/run/up/a.pdf");
    expect(screen.getAllByTestId("files-recent-row")).toHaveLength(1);
  });

  it("cancel then a real abort: cancelled, NOT in Recent; the may-have-stored note only once every byte was sent", async () => {
    const sig: AbortSignal[] = [];
    const prog: Array<(l: number, t: number) => void> = [];
    setUploadTransport((req) => {
      sig.push(req.signal!);
      prog.push(req.onProgress!);
      return new Promise((_, rej) => req.signal!.addEventListener("abort", () => rej(new DOMException("aborted", "AbortError"))));
    });
    mount();
    await pick(file("a.pdf", 10), file("b.pdf", 10));
    await act(async () => prog[0](4, 10));
    await act(async () => fireEvent.click(within(screen.getAllByTestId("files-upload-row")[0]).getByText("Cancel")));
    await flush();
    await act(async () => prog[1](10, 10));
    await act(async () => fireEvent.click(within(screen.getAllByTestId("files-upload-row")[1]).getByText("Cancel")));
    await flush();
    const rows = screen.getAllByTestId("files-upload-row");
    expect(rows.map((r) => r.dataset.status)).toEqual(["cancelled", "cancelled"]);
    expect(within(rows[0]).queryByTestId("files-cancelled-maybe")).toBeNull();
    expect(within(rows[1]).getByTestId("files-cancelled-maybe").textContent).toMatch(/may still have been stored; check Uploads/);
    expect(screen.queryAllByTestId("files-recent-row")).toHaveLength(0);
  });

  it("a server error shows on the row AND as a toast, and later files still go", async () => {
    let n = 0;
    setUploadTransport(async () =>
      ++n === 1
        ? { status: 413, statusText: "x", text: JSON.stringify({ type: "urn:kb:errors:terminal-upload-quota", detail: "q" }) }
        : ok(answer("b.json")),
    );
    const err = vi.spyOn(toast, "err");
    mount();
    await pick(file("a.pdf"), file("b.json"));
    expect(screen.getByTestId("files-row-error").textContent).toMatch(/Upload area is full/);
    expect(err).toHaveBeenCalledWith(expect.stringMatching(/a\.pdf: Upload area is full/));
    expect(screen.getAllByTestId("files-upload-row")[1].dataset.status).toBe("done");
  });

  it("refuses an oversize or empty file with a toast and no request", async () => {
    const transport = vi.fn();
    setUploadTransport(transport);
    const err = vi.spyOn(toast, "err");
    mount();
    await pick({ name: "big.bin", size: FILE_MAX_BYTES + 1, type: "" } as File, file("empty.txt", 0));
    expect(transport).not.toHaveBeenCalled();
    expect(err).toHaveBeenCalledWith(expect.stringMatching(/big\.bin: Over the 32 MiB/));
    expect(err).toHaveBeenCalledWith(expect.stringMatching(/empty\.txt: That file is empty/));
    expect(screen.queryAllByTestId("files-upload-row")).toHaveLength(0);
  });

  it("a drop on the tab queues the files", async () => {
    const urls: string[] = [];
    setUploadTransport(async (req) => (urls.push(req.url), ok(answer("d.txt"))));
    mount();
    const rail = screen.getByTestId("files-rail");
    const dt = { types: ["Files"], files: [file("d.txt")] };
    await act(async () => {
      fireEvent.dragOver(rail, { dataTransfer: dt });
    });
    expect(rail.dataset.dragover).toBe("1");
    await act(async () => {
      fireEvent.drop(rail, { dataTransfer: dt });
    });
    await flush();
    expect(urls).toHaveLength(1);
    expect(rail.dataset.dragover).toBe("0");
  });

  it("paste posts {text} only, single line, no Enter; a refusal is toast.err with the server's detail", async () => {
    setUploadTransport(async () => ok(answer("a.pdf")));
    mount();
    await pick(file("a.pdf"));
    await act(async () => fireEvent.click(within(screen.getAllByTestId("files-upload-row")[0]).getByText("Paste path into terminal")));
    await flush();
    const call = fetchMock.mock.calls.find((c) => String(c[0]).endsWith("/paste"))!;
    expect(JSON.parse(call[1].body as string)).toEqual({ text: "/run/up/a.pdf" });
    routes = (u) => (u.endsWith("/paste") ? json(409, { detail: "session is blocked" }) : json(200, ls("/w", [])));
    const err = vi.spyOn(toast, "err");
    await act(async () => fireEvent.click(within(screen.getAllByTestId("files-upload-row")[0]).getByText("Paste path into terminal")));
    await flush();
    expect(err).toHaveBeenCalledWith("session is blocked");
  });

  it("refuses to paste a path with a line break (no request)", async () => {
    setUploadTransport(async () => ok({ ...answer("a.pdf"), path: "/run/up/a\nb" }));
    mount();
    await pick(file("a.pdf"));
    const err = vi.spyOn(toast, "err");
    await act(async () => fireEvent.click(within(screen.getByTestId("files-upload-list")).getByText("Paste path into terminal")));
    await flush();
    expect(fetchMock.mock.calls.some((c) => String(c[0]).endsWith("/paste"))).toBe(false);
    expect(err).toHaveBeenCalledWith(expect.stringMatching(/line break/));
  });

  it("shows stored, skipped and failed kb status honestly", async () => {
    const kbs = [kbOf(), kbOf({ status: "skipped", reason: "quota", stub_rel: null, id: null }), kbOf({ status: "failed", reason: "disk", stub_rel: null, id: null })];
    let i = 0;
    setUploadTransport(async () => ok(answer(`f${i}.txt`, kbs[i++])));
    mount();
    await pick(file("f0.txt"), file("f1.txt"), file("f2.txt"));
    const keeps = Array.from(screen.getByTestId("files-upload-list").querySelectorAll<HTMLElement>("[data-keep]"));
    expect(keeps.map((k) => k.dataset.keep)).toEqual(["stored", "skipped", "failed"]);
    expect(keeps[0].textContent).toContain("files/a.html");
    expect(keeps[1].textContent).toMatch(/Not kept in kb: quota/);
    expect(keeps[2].textContent).toMatch(/kb keep failed: disk/);
  });

  it("Recent survives a remount in the same page and touches no web storage", async () => {
    const ls1 = vi.spyOn(Storage.prototype, "setItem");
    setUploadTransport(async () => ok(answer("a.pdf")));
    const m = mount();
    await pick(file("a.pdf"));
    expect(screen.getAllByTestId("files-recent-row")).toHaveLength(1);
    m.remount();
    expect(screen.getAllByTestId("files-recent-row")).toHaveLength(1);
    expect(ls1).not.toHaveBeenCalled();
    expect(localStorage.length + sessionStorage.length).toBe(0);
  });
});

describe("paste quoting", () => {
  it("single-quotes a path with a space, a quote or $(...) and never sends Enter", async () => {
    const r = { ...answer("a.pdf"), path: "/run/up/it's $(touch x).pdf" };
    setUploadTransport(async () => ok(r));
    mount();
    await pick(file("a.pdf"));
    await act(async () => fireEvent.click(within(screen.getAllByTestId("files-upload-row")[0]).getByText("Paste path into terminal")));
    await flush();
    const call = fetchMock.mock.calls.find((c) => String(c[0]).endsWith("/paste"))!;
    const body = JSON.parse(call[1].body as string);
    expect(body).toEqual({ text: "'/run/up/it'\\''s $(touch x).pdf'" });
    expect(JSON.stringify(body)).not.toMatch(/enter/i);
  });
});

describe("browse", () => {
  const tree = (u: string): Response => {
    const q = new URL(u, "http://x").searchParams;
    const path = q.get("path");
    if (path === "sub") return json(200, ls("/w/sub", [ent("inner.txt")]));
    if (path === "secret") return json(403, { type: "urn:kb:errors:terminal-outside-root", detail: "outside" });
    if (path === "link") return json(422, { type: "urn:kb:errors:terminal-not-a-file", detail: "not a file" });
    return json(200, ls("/w", [ent("sub", "dir"), ent("a.txt"), ent(".env", "file", { hidden: true })]));
  };

  it("lists in server order, hides dotfiles until toggled, navigates and returns by breadcrumb", async () => {
    routes = tree;
    mount();
    await waitFor(() => expect(screen.getAllByTestId("files-entry")).toHaveLength(2));
    expect(screen.getAllByTestId("files-entry").map((e) => e.dataset.kind)).toEqual(["dir", "file"]);
    fireEvent.click(screen.getByTestId("files-hidden"));
    expect(screen.getAllByTestId("files-entry")).toHaveLength(3);
    fireEvent.click(screen.getByText("sub/"));
    await waitFor(() => expect(screen.getByText("inner.txt")).toBeTruthy());
    expect(within(screen.getByTestId("files-crumbs")).getByText("sub")).toBeTruthy();
    fireEvent.click(within(screen.getByTestId("files-crumbs")).getByText("Working tree"));
    await waitFor(() => expect(screen.getByText("a.txt")).toBeTruthy());
    const lsCalls = fetchMock.mock.calls.map((c) => String(c[0])).filter((u) => u.includes("/ls?"));
    expect(lsCalls[1]).toBe("/api/terms/t1/ls?root=cwd&path=sub");
  });

  it("the Uploads root queries root=up", async () => {
    routes = tree;
    mount();
    await waitFor(() => screen.getByText("a.txt"));
    fireEvent.click(screen.getByTestId("files-root-up"));
    await waitFor(() => expect(fetchMock.mock.calls.some((c) => String(c[0]) === "/api/terms/t1/ls?root=up&path=")).toBe(true));
  });

  it("shows truncated and skipped captions only when set", async () => {
    routes = () => json(200, ls("/w", [ent("a.txt")], { truncated: true, skipped: 2 }));
    mount();
    await waitFor(() => screen.getByTestId("files-truncated"));
    expect(screen.getByTestId("files-skipped").textContent).toMatch(/2 items/);
    cleanup();
    routes = () => json(200, ls("/w", [ent("a.txt")]));
    mount();
    await waitFor(() => screen.getByText("a.txt"));
    expect(screen.queryByTestId("files-truncated")).toBeNull();
    expect(screen.queryByTestId("files-skipped")).toBeNull();
  });

  it("empty, loading, and a retryable error each read honestly", async () => {
    routes = () => json(200, ls("/w", []));
    const m = mount();
    expect(screen.getByText("Loading the directory")).toBeTruthy();
    await waitFor(() => screen.getByText("This folder is empty"));
    m.unmount();
    let fail = true;
    routes = () => (fail ? json(500, { detail: "boom" }) : json(200, ls("/w", [ent("a.txt")])));
    mount();
    await waitFor(() => screen.getByText("Could not list the directory"));
    expect(screen.getByText("boom")).toBeTruthy();
    fail = false;
    fireEvent.click(document.querySelector('[data-kb-action="retry"]')!);
    await waitFor(() => screen.getByText("a.txt"));
  });

  it("distinguishes outside-root (403), not-a-file (422) and runner down (503)", async () => {
    for (const [status, title] of [[403, "Outside the allowed folder"], [422, "Not a regular file or folder"], [503, "The terminal's runner is not reachable"]] as const) {
      routes = () => json(status, { detail: "d" });
      mount();
      await waitFor(() => screen.getByText(title));
      cleanup();
    }
  });

  it("refresh refetches once and nothing polls", async () => {
    routes = tree;
    mount();
    await waitFor(() => screen.getByText("a.txt"));
    const count = () => fetchMock.mock.calls.filter((c) => String(c[0]).includes("/ls?")).length;
    expect(count()).toBe(1);
    await act(async () => fireEvent.click(screen.getByTestId("files-refresh")));
    await waitFor(() => expect(count()).toBe(2));
    await flush();
    expect(count()).toBe(2);
  });

  it("downloads with the intent header, blob + anchor, and revokes the URL after a delay; paste uses the absolute path", async () => {
    routes = (u) => (u.includes("/file?") ? new Response(new Uint8Array([1, 2, 3]), { status: 200, headers: { "Content-Length": "3" } }) : tree(u));
    const create = vi.fn(() => "blob:z");
    const revoke = vi.fn();
    Object.assign(URL, { createObjectURL: create, revokeObjectURL: revoke });
    const click = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => {});
    mount();
    await waitFor(() => screen.getByText("a.txt"));
    const row = screen.getAllByTestId("files-entry")[1];
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    await act(async () => fireEvent.click(within(row).getByText("Download")));
    await flush();
    const call = fetchMock.mock.calls.find((c) => String(c[0]).includes("/file?"))!;
    expect(call[0]).toBe("/api/terms/t1/file?root=cwd&path=a.txt");
    expect((call[1].headers as Record<string, string>)["X-Kb-Intent"]).toBe("terminal");
    expect(click).toHaveBeenCalledTimes(1);
    expect(revoke).not.toHaveBeenCalled();
    vi.advanceTimersByTime(60_000);
    expect(revoke).toHaveBeenCalledWith("blob:z");
    vi.useRealTimers();
    await act(async () => fireEvent.click(within(row).getByText("Paste path into terminal")));
    await flush();
    const p = fetchMock.mock.calls.find((c) => String(c[0]).endsWith("/paste"))!;
    expect(JSON.parse(p[1].body as string)).toEqual({ text: "/w/a.txt" });
    delete (URL as unknown as Record<string, unknown>).createObjectURL;
    delete (URL as unknown as Record<string, unknown>).revokeObjectURL;
  });

  it("a download that fails is a toast.err", async () => {
    routes = (u) => (u.includes("/file?") ? json(404, { detail: "gone" }) : tree(u));
    const err = vi.spyOn(toast, "err");
    mount();
    await waitFor(() => screen.getByText("a.txt"));
    await act(async () => fireEvent.click(within(screen.getAllByTestId("files-entry")[1]).getByText("Download")));
    await flush();
    expect(err).toHaveBeenCalledWith("a.txt: gone");
  });

  it("a hostile name renders as text: no element is created and controls are escaped", async () => {
    const hostile = "<img src=x onerror=alert(1)>\u202e\n.txt";
    routes = () => json(200, ls("/w", [ent(hostile)]));
    const { container } = mount();
    await waitFor(() => screen.getByTestId("files-entry"));
    expect(container.querySelector("img")).toBeNull();
    const text = screen.getByTestId("files-entry").textContent ?? "";
    expect(text).toContain("<img src=x onerror=alert(1)>");
    expect(text).toContain("\\u{202e}\\u{a}.txt");
    expect(text).not.toContain("\u202e");
    expect(container.querySelector("a[href]")).toBeNull();
  });
});

describe("keep in kb", () => {
  const KEPT = { name: "a.txt", bytes: 12, sha256: "abc", kb: kbOf({ id: "kid1", stub_rel: "files/a.txt.html" }) };
  const listing = (u: string): Response | null => {
    if (!u.includes("/ls?")) return null;
    return json(200, ls("/w", [ent("sub", "dir"), ent("a.txt"), ent("big.bin", "file", { bytes: FILE_MAX_BYTES + 1 })]));
  };
  let keepBodies: Array<Record<string, unknown>>;
  let keepInits: RequestInit[];
  const keepRoute = (res: () => Response) => (u: string, init?: RequestInit) => {
    if (u.endsWith("/files/keep")) {
      keepBodies.push(JSON.parse(init!.body as string));
      keepInits.push(init!);
      return res();
    }
    return listing(u) ?? json(200, {});
  };
  beforeEach(() => {
    keepBodies = [];
    keepInits = [];
  });
  const keepBtn = (): HTMLElement => within(screen.getAllByTestId("files-entry")[1]).getByText("Keep in kb");
  const click = async (el: HTMLElement): Promise<void> => {
    await act(async () => fireEvent.click(el));
    await flush();
  };

  it("offers Keep on file rows only, in both roots; the index box starts off with the scrub caption", async () => {
    routes = keepRoute(() => json(200, KEPT));
    mount();
    await waitFor(() => screen.getByText("a.txt"));
    const rows = screen.getAllByTestId("files-entry");
    expect(within(rows[0]).queryByText("Keep in kb")).toBeNull();
    expect(within(rows[1]).getByText("Keep in kb")).toBeTruthy();
    expect((screen.getByTestId("files-keep-index") as HTMLInputElement).checked).toBe(false);
    expect(screen.getByText(/secrets are scrubbed from the text but check it/)).toBeTruthy();
    fireEvent.click(screen.getByTestId("files-root-up"));
    await waitFor(() => expect(screen.getAllByTestId("files-entry")).toHaveLength(3));
    expect(screen.getAllByText("Keep in kb")).toHaveLength(2);
  });

  it("keeps with index:false by default, index:true when ticked, and the up root", async () => {
    routes = keepRoute(() => json(200, KEPT));
    mount();
    await waitFor(() => screen.getByText("a.txt"));
    await click(keepBtn());
    fireEvent.click(screen.getByTestId("files-keep-index"));
    await click(keepBtn());
    fireEvent.click(screen.getByTestId("files-root-up"));
    await waitFor(() => expect(screen.getAllByTestId("files-entry")).toHaveLength(3));
    await click(keepBtn());
    expect(keepBodies).toEqual([
      { path: "a.txt", root: "cwd", index: false },
      { path: "a.txt", root: "cwd", index: true },
      { path: "a.txt", root: "up", index: true },
    ]);
    expect((keepInits[0].headers as Record<string, string>)["X-Kb-Intent"]).toBe("terminal");
  });

  it("success shows name, size, stub path with Copy, and Download original through kept/{id}", async () => {
    routes = (u, init) =>
      u.includes("/kept/") ? new Response(new Uint8Array([1, 2]), { status: 200, headers: { "Content-Length": "2" } }) : keepRoute(() => json(200, KEPT))(u, init) as Response;
    Object.assign(URL, { createObjectURL: vi.fn(() => "blob:k"), revokeObjectURL: vi.fn() });
    const aclick = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => {});
    mount();
    await waitFor(() => screen.getByText("a.txt"));
    await click(keepBtn());
    const res = screen.getByTestId("files-keep-result");
    expect(res.textContent).toMatch(/a\.txt · 12\.0? ?B|a\.txt · 12 B/);
    expect(res.textContent).toContain("Kept in kb");
    expect(res.textContent).toContain("files/a.txt.html");
    expect(within(res).getByText("Copy")).toBeTruthy();
    await click(within(res).getByText("Download original"));
    const call = fetchMock.mock.calls.find((c) => String(c[0]).includes("/kept/"))!;
    expect(call[0]).toBe("/api/terms/t1/kept/kid1");
    expect((call[1].headers as Record<string, string>)["X-Kb-Intent"]).toBe("terminal");
    expect(aclick).toHaveBeenCalledTimes(1);
    delete (URL as unknown as Record<string, unknown>).createObjectURL;
    delete (URL as unknown as Record<string, unknown>).revokeObjectURL;
  });

  it("created:false reads as Already kept, inlined says so", async () => {
    routes = keepRoute(() => json(200, { ...KEPT, kb: kbOf({ created: false, inlined: true }) }));
    mount();
    await waitFor(() => screen.getByText("a.txt"));
    await click(keepBtn());
    expect(screen.getByTestId("files-keep-result").textContent).toMatch(/Already kept in kb \(content indexed\)/);
  });

  it("a skipped or failed 200 answer is shown honestly and toasted", async () => {
    const err = vi.spyOn(toast, "err");
    routes = keepRoute(() => json(200, { ...KEPT, kb: kbOf({ status: "skipped", reason: "not-configured", id: null, stub_rel: null }) }));
    mount();
    await waitFor(() => screen.getByText("a.txt"));
    await click(keepBtn());
    expect(screen.getByTestId("files-keep-result").textContent).toMatch(/Not kept in kb: not-configured/);
    expect(err).toHaveBeenCalledWith(expect.stringMatching(/a\.txt: Not kept in kb: not-configured/));
    cleanup();
    routes = keepRoute(() => json(200, { ...KEPT, kb: kbOf({ status: "failed", reason: "disk", id: null, stub_rel: null }) }));
    mount();
    await waitFor(() => screen.getByText("a.txt"));
    await click(keepBtn());
    expect(screen.getByTestId("files-keep-result").textContent).toMatch(/kb keep failed: disk/);
    expect(err).toHaveBeenCalledWith(expect.stringMatching(/a\.txt: kb keep failed: disk/));
  });

  it.each([
    [409, "urn:kb:errors:terminal-files-not-configured", /not configured on this server/],
    [409, "urn:kb:errors:terminal-file-changed", /changed while it was being read/],
    [413, "urn:kb:errors:terminal-upload-quota", /quota/],
    [413, "urn:kb:errors:payload-too-large", /32 MiB/],
    [503, "urn:kb:errors:terminal-runner-unavailable", /runner is not reachable/],
    [403, "urn:kb:errors:terminal-outside-root", /outside the root/],
    [422, "urn:kb:errors:terminal-not-a-file", /not a file/],
    [404, "urn:kb:errors:not-found", /no such file/],
    [400, "urn:kb:errors:bad-request", /empty path/],
  ])("refusal %i %s shows on the row and as a toast", async (status, type, text) => {
    const err = vi.spyOn(toast, "err");
    const detail = ({ 403: "outside the root", 422: "not a file", 404: "no such file", 400: "empty path" } as Record<number, string>)[status] ?? "x";
    routes = keepRoute(() => json(status, { type, detail }));
    mount();
    await waitFor(() => screen.getByText("a.txt"));
    await click(keepBtn());
    expect(screen.getByTestId("files-keep-error").textContent).toMatch(text);
    expect(err).toHaveBeenCalledWith(expect.stringMatching(text));
  });

  it("a file over 32 MiB is refused with no request", async () => {
    routes = keepRoute(() => json(200, KEPT));
    const err = vi.spyOn(toast, "err");
    mount();
    await waitFor(() => screen.getByText("big.bin"));
    await click(within(screen.getAllByTestId("files-entry")[2]).getByText("Keep in kb"));
    expect(keepBodies).toHaveLength(0);
    expect(err).toHaveBeenCalledWith(expect.stringMatching(/big\.bin: Over the 32 MiB/));
  });

  it("the button is disabled while a keep is in flight", async () => {
    let release!: (r: Response) => void;
    routes = (u) => (u.endsWith("/files/keep") ? (new Promise((r) => (release = r)) as unknown as Response) : (listing(u) ?? json(200, {})));
    mount();
    await waitFor(() => screen.getByText("a.txt"));
    await click(keepBtn());
    expect((keepBtn() as HTMLButtonElement).disabled).toBe(true);
    await act(async () => release(json(200, KEPT)));
    await flush();
    expect((keepBtn() as HTMLButtonElement).disabled).toBe(false);
  });

  it("a hostile name stays plain text in the keep result and touches no web storage", async () => {
    const hostile = "<b>x</b>‮.txt";
    routes = (u) =>
      u.endsWith("/files/keep") ? json(200, { ...KEPT, name: hostile, kb: kbOf({ stub_rel: "<i>s</i>" }) }) : json(200, ls("/w", [ent(hostile)]));
    const set = vi.spyOn(Storage.prototype, "setItem");
    const { container } = mount();
    await waitFor(() => screen.getByTestId("files-entry"));
    await click(screen.getByText("Keep in kb"));
    const res = screen.getByTestId("files-keep-result");
    expect(container.querySelector("b, i")).toBeNull();
    expect(res.textContent).toContain("<b>x</b>\\u{202e}.txt");
    expect(res.textContent).toContain("<i>s</i>");
    expect(set).not.toHaveBeenCalled();
  });
});

// BPHONE (f): the breadcrumb follows a rotation without a remount.
describe("breadcrumb follows the viewport", () => {
  type Listener = () => void;
  function stubMedia(initial: boolean) {
    const listeners = new Set<Listener>();
    const mql = {
      matches: initial,
      media: "(max-width: 480px)",
      addEventListener: vi.fn((_t: string, f: Listener) => listeners.add(f)),
      removeEventListener: vi.fn((_t: string, f: Listener) => listeners.delete(f)),
    };
    vi.stubGlobal("matchMedia", vi.fn(() => mql));
    return {
      mql,
      listeners,
      set: (matches: boolean) => {
        mql.matches = matches;
        act(() => listeners.forEach((f) => f()));
      },
    };
  }
  const deep = (u: string): Response => {
    const path = new URL(u, "http://x").searchParams.get("path") ?? "";
    const next: Record<string, string> = { "": "a", a: "b", "a/b": "c", "a/b/c": "d" };
    return json(200, ls(`/w/${path}`, path in next ? [ent(next[path], "dir")] : []));
  };
  async function descend(): Promise<void> {
    for (const seg of ["a", "b", "c"]) {
      await waitFor(() => screen.getByText(`${seg}/`));
      fireEvent.click(screen.getByText(`${seg}/`));
    }
    await waitFor(() => screen.getByText("d/"));
  }
  const labels = (): string[] => [...screen.getByTestId("files-crumbs").querySelectorAll(".kb-filesrail__crumb")].map((b) => b.textContent ?? "");

  it("folds when narrow, unfolds on a change event without a remount, folds again, and the listener goes on unmount", async () => {
    routes = deep;
    const media = stubMedia(true);
    const view = mount();
    await descend();
    expect(labels()).toEqual(["Working tree", "…", "b", "c"]);
    media.set(false);
    expect(labels()).toEqual(["Working tree", "a", "b", "c"]);
    media.set(true);
    expect(labels()).toEqual(["Working tree", "…", "b", "c"]);
    expect(media.listeners.size).toBe(1);
    view.unmount();
    expect(media.listeners.size).toBe(0);
    expect(media.mql.removeEventListener).toHaveBeenCalled();
  });

  it("is safe without matchMedia (unfolded)", async () => {
    routes = deep;
    vi.stubGlobal("matchMedia", undefined);
    mount();
    await descend();
    expect(labels()).toEqual(["Working tree", "a", "b", "c"]);
  });
});
