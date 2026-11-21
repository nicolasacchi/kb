// @vitest-environment jsdom
// FUTERMFULL — the phone mount of the panel: the sheet's full detent is the whole
// dynamic viewport, and the Sheet's own title is name-only (the phone bar carries
// the visible one). The engine, the session and the phone view are faked.

import { act } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

vi.mock("../hooks/useIsMobile", () => ({ useIsMobile: () => true }));
vi.mock("@xterm/xterm", () => ({ Terminal: class {} }));
vi.mock("@xterm/addon-image", () => ({ ImageAddon: class {} }));
vi.mock("@xterm/addon-webgl", () => ({ WebglAddon: class {} }));
vi.mock("@xterm/addon-unicode-graphemes", () => ({ UnicodeGraphemesAddon: class {} }));
vi.mock("@xterm/xterm/css/xterm.css", () => ({}));
vi.mock("./session", () => ({
  TermSession: class {
    start(): void {}
    dispose(): void {}
    kick(): void {}
  },
}));
vi.mock("./useTerms", () => ({ useTerms: () => ({ terms: [], isLoading: false }) }));
vi.mock("./phone/PhoneTerminal", () => ({ PhoneTerminal: () => <div data-testid="phone-stub" />, attachTerm: () => {} }));
vi.mock("./phone/DriveDock", () => ({ DriveDock: () => null }));
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
});
