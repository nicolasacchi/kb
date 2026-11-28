// @vitest-environment jsdom
// FUTERMPASTE — picture paste and drop on the desktop panel host.

import { act, useRef } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { toast } from "../lib/toast";
import { PASTE_NOTE, useTerminalPaste } from "./useTerminalPaste";
import { UPLOAD_MAX_BYTES } from "./upload";
import { fakeDt } from "./upload.testkit";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const SHA = "0123456789abcdef".repeat(4);
const png = (size: number): File => new File([new Uint8Array(size)], "p.png", { type: "image/png" });

let root: Root | null = null;
let container: HTMLElement | null = null;
let fetchMock: ReturnType<typeof vi.fn>;
const pasted: string[] = [];
const said: string[] = [];
let childSaw: string[] = [];

function Rig({ active = true }: { active?: boolean }) {
  const hostRef = useRef<HTMLDivElement | null>(null);
  useTerminalPaste({
    hostRef,
    termId: "abcd1234",
    active,
    paste: (t) => pasted.push(t),
    say: (m) => said.push(m),
  });
  return (
    <div ref={hostRef} data-testid="host">
      <textarea data-testid="xterm-ta" />
    </div>
  );
}

function mountRig(active = true): HTMLElement {
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  act(() => root!.render(<Rig active={active} />));
  const ta = container.querySelector("textarea")!;
  for (const t of ["paste", "drop"]) ta.addEventListener(t, () => childSaw.push(t));
  return ta;
}

async function fire(el: Element, type: string, dt: DataTransfer): Promise<Event> {
  const ev = new Event(type, { bubbles: true, cancelable: true });
  Object.defineProperty(ev, type === "paste" ? "clipboardData" : "dataTransfer", { value: dt });
  await act(async () => {
    el.dispatchEvent(ev);
    for (let i = 0; i < 6; i += 1) await Promise.resolve();
  });
  return ev;
}

beforeEach(() => {
  pasted.length = 0;
  said.length = 0;
  childSaw = [];
  fetchMock = vi.fn(
    async () =>
      new Response(
        JSON.stringify({ path: "/up/x.png", bytes: 4, mime: "image/png", sha256: SHA }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      ),
  );
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  if (root) act(() => root!.unmount());
  container?.remove();
  root = null;
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("a picture paste", () => {
  it("uploads once, pastes the returned path (no Enter) and keeps xterm's handler out", async () => {
    const ta = mountRig();
    const ev = await fire(ta, "paste", fakeDt({ types: ["Files"], files: [png(4)] }));
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(fetchMock.mock.calls[0][0]).toBe("/api/terms/abcd1234/upload");
    expect(pasted).toEqual(["/up/x.png"]);
    expect(pasted[0]).not.toMatch(/[\r\n]/);
    expect(ev.defaultPrevented).toBe(true);
    expect(childSaw).toEqual([]);
    expect(said.at(-1)).toBe(PASTE_NOTE);
  });

  it("refuses 8 MiB + 1 client-side: a toast, zero fetches", async () => {
    const err = vi.spyOn(toast, "err");
    const ta = mountRig();
    await fire(ta, "paste", fakeDt({ types: ["Files"], files: [png(UPLOAD_MAX_BYTES + 1)] }));
    expect(fetchMock).not.toHaveBeenCalled();
    expect(pasted).toEqual([]);
    expect(err).toHaveBeenCalledWith(expect.stringMatching(/8 MiB/));
  });

  it("a text-only paste is not prevented and reaches xterm (negative control: passes on main too)", async () => {
    const ta = mountRig();
    const ev = await fire(ta, "paste", fakeDt({ types: ["text/plain"] }));
    expect(ev.defaultPrevented).toBe(false);
    expect(childSaw).toEqual(["paste"]);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("an image alongside text/plain is a text paste", async () => {
    const ta = mountRig();
    const ev = await fire(ta, "paste", fakeDt({ types: ["text/plain", "Files"], files: [png(4)] }));
    expect(ev.defaultPrevented).toBe(false);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("a server refusal is a toast and pastes nothing", async () => {
    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify({ type: "urn:kb:errors:unsupported-media", detail: "not a picture" }), {
        status: 415,
        headers: { "Content-Type": "application/json" },
      }),
    );
    const err = vi.spyOn(toast, "err");
    const ta = mountRig();
    await fire(ta, "paste", fakeDt({ types: ["Files"], files: [png(4)] }));
    expect(pasted).toEqual([]);
    expect(err).toHaveBeenCalledWith("not a picture");
  });

  it("does nothing while inactive (a phone sheet owns its own paste)", async () => {
    const ta = mountRig(false);
    const ev = await fire(ta, "paste", fakeDt({ types: ["Files"], files: [png(4)] }));
    expect(ev.defaultPrevented).toBe(false);
    expect(fetchMock).not.toHaveBeenCalled();
  });
});

describe("a file drop", () => {
  it("dragover over files is accepted; over text it is left alone", async () => {
    const ta = mountRig();
    const a = await fire(ta, "dragover", fakeDt({ types: ["Files"] }));
    expect(a.defaultPrevented).toBe(true);
    const b = await fire(ta, "dragover", fakeDt({ types: ["text/plain"] }));
    expect(b.defaultPrevented).toBe(false);
  });

  it("a dropped picture takes the same path", async () => {
    const ta = mountRig();
    await fire(ta, "drop", fakeDt({ types: ["Files"], files: [png(4)] }));
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(pasted).toEqual(["/up/x.png"]);
  });

  it("FXUI: a dropped PDF uploads via …/files and its path is pasted without Enter", async () => {
    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify({ path: "/run/up/a.pdf", name: "a.pdf", bytes: 4, kind: "other", sha256: SHA, mime: null, kb: { status: "skipped", reason: "not-configured", id: null, stub_rel: null, created: null, inlined: null } }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      }),
    );
    const ta = mountRig();
    const pdf = new File([new Uint8Array(4)], "a b.pdf", { type: "application/pdf" });
    const ev = await fire(ta, "drop", fakeDt({ types: ["Files"], files: [pdf] }));
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(fetchMock.mock.calls[0][0]).toBe("/api/terms/abcd1234/files?name=a%20b.pdf");
    expect((fetchMock.mock.calls[0][1].headers as Record<string, string>)["X-Kb-Intent"]).toBe("terminal");
    expect(pasted).toEqual(["/run/up/a.pdf"]);
    expect(pasted[0]).not.toMatch(/[\r\n]/);
    expect(ev.defaultPrevented).toBe(true);
    expect(childSaw).toEqual([]);
  });

  it("FXUI: a PNG still goes via …/upload, not …/files", async () => {
    const ta = mountRig();
    await fire(ta, "drop", fakeDt({ types: ["Files"], files: [png(4)] }));
    expect(fetchMock.mock.calls.map((c) => c[0])).toEqual(["/api/terms/abcd1234/upload"]);
  });

  it("FXUI: a pasted non-image file takes the …/files route", async () => {
    fetchMock.mockResolvedValueOnce(
      new Response(JSON.stringify({ path: "/run/up/n.txt", name: "n.txt", bytes: 2, kind: "other", sha256: SHA, mime: null, kb: { status: "skipped", reason: null, id: null, stub_rel: null, created: null, inlined: null } }), { status: 200 }),
    );
    const ta = mountRig();
    const txt = new File(["hi"], "n.txt", { type: "text/plain" });
    await fire(ta, "paste", fakeDt({ types: ["Files"], files: [txt] }));
    expect(fetchMock.mock.calls[0][0]).toBe("/api/terms/abcd1234/files?name=n.txt");
    expect(pasted).toEqual(["/run/up/n.txt"]);
  });

  it("FXUI: an oversize non-picture (32 MiB + 1) is refused client-side with a toast", async () => {
    const err = vi.spyOn(toast, "err");
    const ta = mountRig();
    await fire(ta, "drop", fakeDt({ types: ["Files"], files: [{ name: "big.bin", size: 32 * 1024 * 1024 + 1, type: "" } as File] }));
    expect(fetchMock).not.toHaveBeenCalled();
    expect(err).toHaveBeenCalledWith(expect.stringMatching(/32 MiB/));
  });
});

describe("FUMINPANEL upload hardening", () => {
  const pendingFetch = (): { release: () => void; signals: AbortSignal[] } => {
    const signals: AbortSignal[] = [];
    let release: () => void = () => {};
    fetchMock.mockImplementation(
      (_u: string, init: RequestInit) =>
        new Promise<Response>((resolve, reject) => {
          signals.push(init.signal as AbortSignal);
          init.signal?.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")));
          release = () =>
            resolve(
              new Response(JSON.stringify({ path: "/up/x.png", bytes: 4, mime: "image/png", sha256: SHA }), {
                status: 200,
                headers: { "Content-Type": "application/json" },
              }),
            );
        }),
    );
    return { release: () => release(), signals };
  };

  it("a second picture while one uploads is refused with a toast and sends nothing more", async () => {
    const pending = pendingFetch();
    const err = vi.spyOn(toast, "err");
    const ta = mountRig();
    await fire(ta, "paste", fakeDt({ types: ["Files"], files: [png(4)] }));
    await fire(ta, "paste", fakeDt({ types: ["Files"], files: [png(4)] }));
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(err).toHaveBeenCalledWith(expect.stringMatching(/Still uploading/));
    await act(async () => {
      pending.release();
      for (let i = 0; i < 6; i += 1) await Promise.resolve();
    });
    expect(pasted).toEqual(["/up/x.png"]);
  });

  it("a drop of two pictures and one oversize file uploads the two, toasts the refusal and joins the paths", async () => {
    const err = vi.spyOn(toast, "err");
    const ta = mountRig();
    const note = { name: "big.bin", size: 32 * 1024 * 1024 + 1, type: "" } as File;
    const ev = await fire(ta, "drop", fakeDt({ types: ["Files"], files: [png(4), note, png(4)] }));
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(err).toHaveBeenCalledWith(expect.stringMatching(/32 MiB/));
    expect(pasted).toEqual(["/up/x.png /up/x.png"]);
    expect(ev.defaultPrevented).toBe(true);
    // stopPropagation: xterm's own drop handling never sees it.
    expect(childSaw).toEqual([]);
  });

  it("unmounting mid-upload aborts the request and stays silent (no paste, no toast)", async () => {
    const pending = pendingFetch();
    const err = vi.spyOn(toast, "err");
    const ta = mountRig();
    await fire(ta, "paste", fakeDt({ types: ["Files"], files: [png(4)] }));
    expect(pending.signals).toHaveLength(1);
    expect(pending.signals[0].aborted).toBe(false);
    await act(async () => {
      root!.unmount();
      for (let i = 0; i < 6; i += 1) await Promise.resolve();
    });
    root = null;
    expect(pending.signals[0].aborted).toBe(true);
    expect(pasted).toEqual([]);
    expect(err).not.toHaveBeenCalled();
    expect(said.at(-1)).not.toBe(PASTE_NOTE);
  });
});
