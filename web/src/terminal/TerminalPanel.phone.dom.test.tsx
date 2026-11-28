// @vitest-environment jsdom
// FUTERMFULL — the phone mount of the panel: the sheet's full detent is the whole
// dynamic viewport, and the Sheet's own title is name-only (the phone bar carries
// the visible one). The engine, the session and the phone view are faked.

import { act } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

vi.mock("../hooks/useIsMobile", () => ({ useIsMobile: () => true, usePhonePanel: () => true }));
// BPHONE: what the panel hands the dock, and the callbacks it gave the session.
const seam = vi.hoisted(() => ({
  dockProps: [] as Array<{ ownerKnown?: boolean }>,
  onState: null as null | ((s: Record<string, unknown>) => void),
}));

vi.mock("@xterm/xterm", () => ({ Terminal: class {} }));
vi.mock("@xterm/addon-image", () => ({ ImageAddon: class {} }));
vi.mock("@xterm/addon-webgl", () => ({ WebglAddon: class {} }));
vi.mock("@xterm/addon-unicode-graphemes", () => ({ UnicodeGraphemesAddon: class {} }));
vi.mock("@xterm/xterm/css/xterm.css", () => ({}));
vi.mock("./session", () => ({
  TermSession: class {
    constructor(opts: { callbacks: { onState: (s: Record<string, unknown>) => void } }) {
      seam.onState = opts.callbacks.onState;
    }
    start(): void {}
    dispose(): void {}
    kick(): void {}
  },
}));
vi.mock("../hooks/useLiveSessions", () => ({ useLiveSessions: () => ({ rows: [], loading: false }) }));
vi.mock("./useTerms", () => ({ useTerms: () => ({ terms: [], isLoading: false }) }));
vi.mock("./phone/PhoneTerminal", () => ({
  PhoneTerminal: (p: { onClose?: () => void; renderDrive?: (api: { measure: () => null; fullscreen: boolean }) => unknown }) => (
    <div data-testid="phone-stub">
      <button type="button" data-testid="kb-term-close" onClick={p.onClose} />
      {p.renderDrive?.({ measure: () => null, fullscreen: false }) as never}
    </div>
  ),
  attachTerm: () => {},
}));
vi.mock("./phone/DriveDock", () => ({
  DriveDock: (p: { ownerKnown?: boolean }) => {
    seam.dockProps.push(p);
    return null;
  },
}));
vi.mock("./gfx", () => ({ attachGfx: () => ({}) }));
vi.mock("./Lightbox", () => ({ Lightbox: () => null }));

import { TerminalPanel } from "./TerminalPanel";

let root: Root | null = null;
let host: HTMLElement | null = null;

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  host = root = null;
  document.body.style.overflow = "";
});

describe("phone sheet", () => {
  it("opens at the whole dynamic viewport (--sheet-h 100dvh) with a name-only title", () => {
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
    act(() =>
      root!.render(
        <QueryClientProvider client={new QueryClient()}>
          <TerminalPanel termId="abcd1234" sid="s1" harness="omp" />
        </QueryClientProvider>,
      ),
    );
    const sheet = document.querySelector("[data-kb-sheet]") as HTMLElement;
    expect(sheet.style.getPropertyValue("--sheet-h")).toBe("100dvh");
    const title = sheet.querySelector(".kb-sheet__title") as HTMLElement;
    expect(title.className).toContain("kb-sheet__title--sr");
    expect(title.textContent).toBe("Terminal");
    expect(sheet.getAttribute("aria-labelledby")).toBe(title.id);
  });

  // M1 (FUPHONEMODE r1): without a CloseWatcher the sheet owns one history entry
  // above the router's ?term= entry. The Close button must pop it first; closing
  // by a router replace on top of it strands the entry and Back re-opens the sheet.
  it("Close pops the sheet's history entry first, so Back does not bring ?term= back", async () => {
    delete (window as unknown as { CloseWatcher?: unknown }).CloseWatcher;
    window.history.replaceState({ idx: 3 }, "", "/sessions");
    window.history.pushState({ idx: 4 }, "", "/sessions?term=abcd1234");
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
    const onClose = vi.fn(() => {
      // what the sessions page does: replace ?term= away, which unmounts the panel
      window.history.replaceState({ idx: 4 }, "", "/sessions");
      act(() => root!.render(<div />));
    });
    act(() =>
      root!.render(
        <QueryClientProvider client={new QueryClient()}>
          <TerminalPanel termId="abcd1234" sid="s1" harness="omp" onClose={onClose} />
        </QueryClientProvider>,
      ),
    );
    expect((window.history.state as { kbSheet?: string } | null)?.kbSheet).toBeTruthy();
    const tick = () => new Promise<void>((r) => setTimeout(r, 40));
    await act(async () => {
      (document.querySelector('[data-testid="kb-term-close"]') as HTMLElement).click();
      await tick();
    });
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(window.location.search).toBe("");
    await act(async () => {
      window.history.back();
      await tick();
    });
    expect(window.location.search).toBe("");
    expect(window.location.pathname).toBe("/sessions");
  });

  // BPHONE (e): the dock is told whether an owner can be vouched for at all.
  it("passes ownerKnown=false to the dock while the stream is not live, and true while it is", () => {
    seam.dockProps.length = 0;
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
    act(() =>
      root!.render(
        <QueryClientProvider client={new QueryClient()}>
          <TerminalPanel termId="abcd1234" sid="s1" harness="omp" />
        </QueryClientProvider>,
      ),
    );
    const last = (): boolean | undefined => seam.dockProps[seam.dockProps.length - 1]?.ownerKnown;
    const base = { epoch: null, applied: 0, resets: 0, gaps: 0, attempt: 0, caption: "x", exitCode: null, blockedReason: null };
    expect(last()).toBe(true);
    act(() => seam.onState!({ ...base, phase: "live" }));
    expect(last()).toBe(true);
    for (const phase of ["reconnecting", "ended", "expired", "blocked", "sleeping"]) {
      act(() => seam.onState!({ ...base, phase }));
      expect(last(), phase).toBe(false);
    }
    act(() => seam.onState!({ ...base, phase: "live" }));
    expect(last()).toBe(true);
  });
});
