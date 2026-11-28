// @vitest-environment jsdom
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";

const toastErr = vi.hoisted(() => vi.fn());
vi.mock("../../../lib/toast", () => ({ toast: { err: toastErr, ok: vi.fn(), info: vi.fn() } }));
vi.mock("../../../hooks/useArtifactHost", () => ({ useIdentity: () => ({ terminal_admitted: true }) }));
const phone = vi.hoisted(() => ({ value: false }));
vi.mock("../../../hooks/useIsMobile", () => ({ usePhonePanel: () => phone.value }));

import { CommandsButton } from "./CommandsButton";
import { installDialogPolyfill, rigFetch, wrap, type Rig } from "./paletteTestkit";

beforeAll(installDialogPolyfill);

let rig: Rig;
beforeEach(() => {
  phone.value = false;
  toastErr.mockReset();
  rig = rigFetch();
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function mountPage(harness = "claude") {
  return render(
    wrap(
      <div>
        <div data-testid="kb-terminal">
          <textarea className="xterm-helper-textarea" />
        </div>
        <CommandsButton termId="t1" harness={harness} />
      </div>,
    ),
  );
}

async function openPalette(harness = "claude"): Promise<HTMLElement> {
  mountPage(harness);
  fireEvent.click(screen.getByTestId("kb-term-commands-btn"));
  const dlg = await screen.findByRole("dialog");
  await waitFor(() => expect(within(dlg).queryByText("Showing shipped commands")).toBeNull());
  return dlg;
}

const rowFor = (label: string): HTMLElement => screen.getAllByTestId("kb-cmdp-row").find((r) => r.textContent?.startsWith(label)) as HTMLElement;

describe("CommandPalette", () => {
  it("is a labelled modal dialog and lists only the terminal's harness", async () => {
    const dlg = await openPalette("claude");
    expect(dlg.getAttribute("aria-modal")).toBe("true");
    const title = document.getElementById(dlg.getAttribute("aria-labelledby") as string);
    expect(title?.textContent).toBe("Commands");
    expect(within(dlg).getAllByText("/compact").length).toBeGreaterThan(0);
    expect(within(dlg).queryAllByText("/shake")).toEqual([]);
    expect(within(dlg).getByText("Session")).toBeTruthy();
    expect(within(dlg).getByText("With arguments")).toBeTruthy();
  });

  it("lists omp's groups for an omp terminal and flags unverified entries", async () => {
    const dlg = await openPalette("omp");
    expect(within(dlg).getAllByText("/shake").length).toBeGreaterThan(0);
    expect(within(dlg).queryAllByText("/compact")).toEqual([]);
    expect(within(dlg).getByText("unverified")).toBeTruthy();
  });

  it("an unknown harness says so", async () => {
    mountPage("codex");
    fireEvent.click(screen.getByTestId("kb-term-commands-btn"));
    expect(await screen.findByText("No commands for codex")).toBeTruthy();
  });

  it("shows a preview and a paste / run / keys badge per row", async () => {
    await openPalette();
    expect(rowFor("/compact").textContent).toContain("paste");
    expect(rowFor("/compact").textContent).toContain("Summarise the conversation");
    expect(rowFor("/model").textContent).toContain("run");
    expect(rowFor("Rewind").textContent).toContain("keys");
    expect(rowFor("Rewind").querySelector("code")?.textContent).toBe("Esc Esc");
    expect(rowFor("/compact").querySelector("code")?.textContent).toBe("/compact");
  });

  it("search ranks /comp -> /compact first and says No match when empty", async () => {
    const dlg = await openPalette();
    const search = within(dlg).getByRole("combobox");
    fireEvent.change(search, { target: { value: "/comp" } });
    const rows = screen.getAllByTestId("kb-cmdp-row");
    expect(rows[0].textContent).toContain("/compact");
    expect(search.getAttribute("aria-activedescendant")).toBe(rows[0].id);
    fireEvent.change(search, { target: { value: "zzzzqq" } });
    expect(within(dlg).getByText("No match")).toBeTruthy();
  });

  it("Up/Down move the active row", async () => {
    const dlg = await openPalette();
    const search = within(dlg).getByRole("combobox");
    const rows = screen.getAllByTestId("kb-cmdp-row");
    expect(search.getAttribute("aria-activedescendant")).toBe(rows[0].id);
    fireEvent.keyDown(search, { key: "ArrowDown" });
    expect(search.getAttribute("aria-activedescendant")).toBe(rows[1].id);
    fireEvent.keyDown(search, { key: "ArrowUp" });
    fireEvent.keyDown(search, { key: "ArrowUp" });
    expect(search.getAttribute("aria-activedescendant")).toBe(rows[rows.length - 1].id);
  });

  it("a paste-mode entry issues exactly one paste, no Enter, closes and focuses the pane", async () => {
    await openPalette();
    fireEvent.click(rowFor("/compact"));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(rig.drive()).toEqual([{ op: "paste", body: { text: "/compact" } }]);
    expect(document.activeElement).toBe(document.querySelector(".xterm-helper-textarea"));
  });

  it("Enter sends the active row", async () => {
    const dlg = await openPalette();
    const search = within(dlg).getByRole("combobox");
    fireEvent.change(search, { target: { value: "/config" } });
    fireEvent.keyDown(search, { key: "Enter" });
    await waitFor(() => expect(rig.drive()).toEqual([{ op: "paste", body: { text: "/config" } }]));
  });

  it("flattens a multi-line paste-mode text", async () => {
    await openPalette();
    fireEvent.click(rowFor("/multi"));
    await waitFor(() => expect(rig.drive().length).toBe(1));
    expect(rig.drive()[0].body).toEqual({ text: "first second" });
  });

  it("a run-mode entry asks for the Enter in the same paste request", async () => {
    await openPalette();
    fireEvent.click(rowFor("/model"));
    await waitFor(() => expect(rig.drive().length).toBe(1));
    expect(rig.drive()[0]).toEqual({ op: "paste", body: { text: "/model", enter: true } });
  });

  it("a chord goes through the keys route, one call per key", async () => {
    await openPalette();
    fireEvent.click(rowFor("Rewind"));
    await waitFor(() => expect(rig.drive().length).toBe(2));
    expect(rig.drive()).toEqual([
      { op: "keys", body: { keys: ["Esc"] } },
      { op: "keys", body: { keys: ["Esc"] } },
    ]);
  });

  it("a refused Enter toasts the server's reason", async () => {
    await openPalette();
    rig.queueDrive(200, { entered: false, enter_error: { status: 409, detail: "session is waiting on you" } });
    fireEvent.click(rowFor("/model"));
    await waitFor(() => expect(toastErr).toHaveBeenCalledWith("Enter not sent: session is waiting on you"));
  });

  it("a failed request toasts and keeps the dialog open", async () => {
    await openPalette();
    rig.queueDrive(422, { detail: "multi-line-refused" });
    fireEvent.click(rowFor("/compact"));
    await waitFor(() => expect(toastErr).toHaveBeenCalledWith("multi-line-refused"));
    expect(screen.getByRole("dialog")).toBeTruthy();
  });

  it("an entry with args shows a form: Send stays disabled until filled", async () => {
    await openPalette();
    fireEvent.click(rowFor("/add"));
    const send = screen.getByRole("button", { name: "Send" }) as HTMLButtonElement;
    expect(send.disabled).toBe(true);
    expect(rig.drive()).toEqual([]);
    const field = screen.getByPlaceholderText("A file");
    fireEvent.change(field, { target: { value: "a\nb" } });
    expect(send.disabled).toBe(false);
    fireEvent.click(send);
    await waitFor(() => expect(rig.drive().length).toBe(1));
    expect(rig.drive()[0].body).toEqual({ text: "/add a b" });
  });

  it("a run-mode arg with a newline shows the invalid alert and cannot send", async () => {
    await openPalette();
    fireEvent.click(rowFor("/runarg"));
    fireEvent.change(screen.getByPlaceholderText("x"), { target: { value: "a\nb" } });
    expect(screen.getByRole("alert").textContent).toContain("control character");
    expect((screen.getByRole("button", { name: "Send" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("Esc (cancel) closes the dialog and focus returns to the button", async () => {
    const dlg = await openPalette();
    fireEvent(dlg, new Event("cancel", { cancelable: true }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(document.activeElement).toBe(screen.getByTestId("kb-term-commands-btn"));
  });

  it("the query resets on every open", async () => {
    const dlg = await openPalette();
    fireEvent.change(within(dlg).getByRole("combobox"), { target: { value: "zz" } });
    fireEvent(dlg, new Event("cancel", { cancelable: true }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    fireEvent.click(screen.getByTestId("kb-term-commands-btn"));
    expect((within(await screen.findByRole("dialog")).getByRole("combobox") as HTMLInputElement).value).toBe("");
  });

  it("falls back to the shipped commands, with a caption, when the catalogue cannot be read", async () => {
    vi.unstubAllGlobals();
    rig = rigFetch({ commandsStatus: 500 });
    mountPage();
    fireEvent.click(screen.getByTestId("kb-term-commands-btn"));
    const dlg = await screen.findByRole("dialog");
    await waitFor(() => expect(rig.calls.some((c) => c.url.endsWith("/api/terms/commands"))).toBe(true));
    expect(within(dlg).getByText("Showing shipped commands")).toBeTruthy();
    expect(screen.getAllByTestId("kb-cmdp-row").length).toBeGreaterThan(0);
  });

  it("links to the Settings anchor", async () => {
    const dlg = await openPalette();
    expect(within(dlg).getByRole("link", { name: "Edit commands" }).getAttribute("href")).toBe("/settings#terminal-commands");
  });

  it("renders no header button on the phone panel (the rail Commands tab serves it)", () => {
    phone.value = true;
    mountPage();
    expect(screen.queryByTestId("kb-term-commands-btn")).toBeNull();
  });
});

describe("Alt+K", () => {
  const key = (init: KeyboardEventInit): KeyboardEvent => new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init });

  it("opens the palette and the pane never sees it; plain k and Ctrl+K do not open it", async () => {
    mountPage();
    const pane = document.querySelector('[data-testid="kb-terminal"]') as HTMLElement;
    const seen = vi.fn();
    pane.addEventListener("keydown", seen);

    expect(pane.dispatchEvent(key({ code: "KeyK", key: "k" }))).toBe(true);
    expect(pane.dispatchEvent(key({ code: "KeyK", key: "k", ctrlKey: true }))).toBe(true);
    expect(pane.dispatchEvent(key({ code: "KeyK", key: "k", altKey: true, shiftKey: true }))).toBe(true);
    expect(seen).toHaveBeenCalledTimes(3);
    expect(screen.queryByRole("dialog")).toBeNull();

    seen.mockClear();
    expect(pane.dispatchEvent(key({ code: "KeyK", key: "k", altKey: true }))).toBe(false);
    expect(seen).not.toHaveBeenCalled();
    expect(await screen.findByRole("dialog")).toBeTruthy();
  });

  it("stops listening when the page unmounts", () => {
    const { unmount } = mountPage();
    unmount();
    const ev = key({ code: "KeyK", altKey: true });
    window.dispatchEvent(ev);
    expect(ev.defaultPrevented).toBe(false);
  });
});
