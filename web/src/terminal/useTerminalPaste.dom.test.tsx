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

  it("a dropped non-picture shows the refusal toast and uploads nothing", async () => {
    const err = vi.spyOn(toast, "err");
    const ta = mountRig();
    const pdf = new File([new Uint8Array(4)], "a.pdf", { type: "application/pdf" });
    await fire(ta, "drop", fakeDt({ types: ["Files"], files: [pdf] }));
    expect(fetchMock).not.toHaveBeenCalled();
    expect(err).toHaveBeenCalledWith(expect.stringMatching(/PNG, JPEG, GIF and WebP/));
  });
});
