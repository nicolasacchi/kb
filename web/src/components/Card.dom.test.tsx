// @vitest-environment jsdom
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, useLocation } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { DocSummary } from "../api/client";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const m = vi.hoisted(() => ({
  excludeArtifact: vi.fn(),
  triggerDownload: vi.fn(),
  confirm: vi.fn(),
  navigate: vi.fn(),
  toastOk: vi.fn(),
  toastErr: vi.fn(),
  dialogAtConfirm: [] as boolean[],
}));

vi.mock("../api/client", async (orig) => ({
  ...(await orig<typeof import("../api/client")>()),
  excludeArtifact: m.excludeArtifact,
  fetchDocByPath: vi.fn(),
}));
vi.mock("../lib/download", async (orig) => ({
  ...(await orig<typeof import("../lib/download")>()),
  triggerDownload: m.triggerDownload,
}));
vi.mock("../lib/toast", () => ({ toast: { ok: m.toastOk, err: m.toastErr, info: vi.fn() } }));
vi.mock("./ConfirmProvider", () => ({ useConfirm: () => m.confirm, default: () => null }));
vi.mock("react-router-dom", async (orig) => ({
  ...(await orig<typeof import("react-router-dom")>()),
  useNavigate: () => m.navigate,
}));

import Card from "./Card";
import { artifactHref } from "../lib/artifactHref";
import { artifactDownloadUrl } from "../lib/download";

const doc = {
  id: "abc123def456",
  title: "Home",
  source_relative: "notes/home.html",
  folder: "notes",
  path: "/abs/notes/home.html",
  tags: [],
  word_count: 100,
} as unknown as DocSummary;

let root: Root | null = null;
let host: HTMLElement | null = null;
let loc = "";
function Probe({ children }: { children: ReactNode }) {
  const l = useLocation();
  loc = l.pathname + l.search;
  return <>{children}</>;
}
let bubbled = 0;

function mount() {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() =>
    root!.render(
      <QueryClientProvider client={new QueryClient()}>
        <MemoryRouter initialEntries={["/?kb=k"]}>
          <Probe>
            <div onClick={() => bubbled++}>
              <Card doc={doc} kb="k" />
            </div>
          </Probe>
        </MemoryRouter>
      </QueryClientProvider>,
    ),
  );
}

const more = () => document.querySelector<HTMLButtonElement>('[data-kb-act="more"]')!;
const dialog = () => document.querySelector('[role="dialog"]');
const row = (k: string) => document.querySelector<HTMLButtonElement>(`.kb-card-sheet__row[data-kb-act="${k}"]`)!;
const click = (el: Element) => act(() => { el.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true })); });
const frame = () => act(async () => { await new Promise((r) => requestAnimationFrame(() => r(null))); });
const openSheet = () => { more().focus(); click(more()); };

beforeEach(() => {
  vi.clearAllMocks();
  m.excludeArtifact.mockResolvedValue(undefined);
  m.confirm.mockImplementation(async () => {
    m.dialogAtConfirm.push(!!dialog());
    return true;
  });
  m.dialogAtConfirm.length = 0;
  bubbled = 0;
});
afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = host = null;
  document.body.style.overflow = "";
});

describe("Card More-actions sheet (OL TOUCH1)", () => {
  it("opens a dialog named by the doc title with exactly three actions", () => {
    mount();
    openSheet();
    const d = dialog()!;
    expect(d).not.toBeNull();
    expect(host!.contains(d)).toBe(false);
    expect(document.body.contains(d)).toBe(true);
    expect(document.getElementById(d.getAttribute("aria-labelledby")!)!.textContent).toBe("Home");
    const names = [...d.querySelectorAll(".kb-card-sheet__row")].map((b) => b.textContent);
    expect(names).toEqual(["Preview", "Download", "Exclude from index"]);
    expect(d.querySelectorAll(".kb-card-sheet__row").length).toBe(3);
    expect(more().getAttribute("aria-expanded")).toBe("true");
  });

  it("Escape closes and focus returns to More", async () => {
    mount();
    openSheet();
    act(() => {
      dialog()!.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    });
    await frame();
    expect(dialog()).toBeNull();
    expect(document.activeElement).toBe(more());
  });

  it("Preview closes the sheet and navigates", () => {
    mount();
    openSheet();
    click(row("peek"));
    expect(dialog()).toBeNull();
    expect(m.navigate).toHaveBeenCalledWith(artifactHref("k", doc.source_relative));
  });

  it("Download closes the sheet and downloads once", () => {
    mount();
    openSheet();
    click(row("download"));
    expect(dialog()).toBeNull();
    expect(m.triggerDownload).toHaveBeenCalledTimes(1);
    expect(m.triggerDownload).toHaveBeenCalledWith(artifactDownloadUrl("k", doc.id));
  });

  it("Exclude: the sheet is gone before confirm; confirm=true excludes and toasts", async () => {
    mount();
    openSheet();
    click(row("exclude"));
    await frame();
    expect(m.dialogAtConfirm).toEqual([false]);
    expect(m.excludeArtifact).toHaveBeenCalledWith("k", doc.source_relative);
    expect(m.toastOk).toHaveBeenCalledWith("excluded from index");
  });

  it("Exclude failure surfaces toast.err with the message", async () => {
    m.excludeArtifact.mockRejectedValue(new Error("boom"));
    mount();
    openSheet();
    click(row("exclude"));
    await frame();
    expect(m.toastErr).toHaveBeenCalledWith("exclude failed: boom");
  });

  it("a cancelled Exclude does not exclude and leaves focus on More", async () => {
    m.confirm.mockResolvedValue(false);
    mount();
    openSheet();
    click(row("exclude"));
    await frame();
    expect(m.excludeArtifact).not.toHaveBeenCalled();
    expect(document.activeElement).toBe(more());
  });

  it("Exclude: focus is not stolen from the confirm dialog by a deferred refocus", async () => {
    let go: HTMLButtonElement | null = null;
    m.confirm.mockImplementation(async () => {
      go = document.createElement("button");
      go.className = "confirm__go";
      document.body.appendChild(go);
      go.focus();
      return new Promise<boolean>(() => {});
    });
    mount();
    openSheet();
    click(row("exclude"));
    await frame();
    await frame();
    expect(go).not.toBeNull();
    expect(document.activeElement).toBe(go);
    expect(document.activeElement).not.toBe(more());
    (go as HTMLButtonElement | null)?.remove();
  });

  it("opening and acting never changes the URL and never bubbles", () => {
    mount();
    expect(loc).toBe("/?kb=k");
    openSheet();
    expect(loc).toBe("/?kb=k");
    click(row("download"));
    expect(loc).toBe("/?kb=k");
    expect(bubbled).toBe(0);
  });

  it("restores body overflow after close", () => {
    mount();
    document.body.style.overflow = "auto";
    openSheet();
    expect(document.body.style.overflow).toBe("hidden");
    click(row("peek"));
    expect(document.body.style.overflow).toBe("auto");
  });

  it("the desktop hover buttons call the same shared actions", async () => {
    mount();
    click(document.querySelector(".kb-card__peek")!);
    expect(m.navigate).toHaveBeenCalledWith(artifactHref("k", doc.source_relative));
    click(document.querySelector(".kb-card__download")!);
    expect(m.triggerDownload).toHaveBeenCalledWith(artifactDownloadUrl("k", doc.id));
    click(document.querySelector(".kb-card__exclude")!);
    await frame();
    expect(m.excludeArtifact).toHaveBeenCalledWith("k", doc.source_relative);
    expect(bubbled).toBe(0);
  });
});
