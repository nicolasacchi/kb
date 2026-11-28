// @vitest-environment jsdom
// CMDSHEET - the phone Commands sheet: the palette's list and send rules inside
// a floating panel that never takes focus from the input proxy.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

const toastErr = vi.hoisted(() => vi.fn());
vi.mock("../../lib/toast", () => ({ toast: { err: toastErr, ok: vi.fn(), info: vi.fn() } }));
vi.mock("../../hooks/useArtifactHost", () => ({ useIdentity: () => ({ terminal_admitted: true }) }));

import { rigFetch, wrap, type Rig } from "../commands/palette/paletteTestkit";
import { CommandSheet } from "./CommandSheet";
import type { Drive } from "./driveApi";

let rig: Rig;
beforeEach(() => {
  toastErr.mockReset();
  rig = rigFetch();
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  document.body.innerHTML = "";
});

const drive = (over: Partial<Drive> = {}): Drive => ({ termId: "t9", harness: "claude", viewer: "ws-abc1234567", confirm: vi.fn(async () => true), ...over });

function mountSheet(d: Drive = drive(), onClose = vi.fn(), onTyped = vi.fn()) {
  // The proxy stub lives outside the sheet, as in the dock.
  const proxy = document.createElement("textarea");
  proxy.setAttribute("data-kb-term-input", "");
  document.body.appendChild(proxy);
  proxy.focus();
  render(wrap(<CommandSheet drive={d} onClose={onClose} onTyped={onTyped} />));
  return { proxy, onClose, onTyped };
}

const ready = () => waitFor(() => expect(screen.queryByText("Showing shipped commands")).toBeNull());

describe("CommandSheet", () => {
  it("renders only the harness's groups, as 44 px sheet rows, with no autofocused search", async () => {
    const { proxy } = mountSheet(drive({ harness: "omp" }));
    await ready();
    expect(screen.getAllByText("/shake").length).toBeGreaterThan(0);
    expect(screen.queryAllByText("/compact")).toEqual([]);
    expect(screen.queryAllByText("/config")).toEqual([]);
    expect(document.querySelector(".kb-cmdp--sheet")).not.toBeNull();
    for (const r of screen.getAllByTestId("kb-cmdp-row")) expect(r.classList.contains("kb-cmdp__row")).toBe(true);
    expect(document.activeElement).toBe(proxy);
    expect(document.activeElement).not.toBe(document.querySelector(".kb-cmdp__search"));
  });

  it("an unknown harness shows the empty text", async () => {
    mountSheet(drive({ harness: "nope" }));
    await ready();
    expect(screen.getByText("No commands for nope")).toBeTruthy();
  });

  it("a paste-mode row issues exactly one paste without Enter, with the viewer, and keeps proxy focus", async () => {
    const { proxy, onClose, onTyped } = mountSheet();
    await ready();
    fireEvent.click(screen.getAllByText("/compact")[0]);
    await waitFor(() => expect(rig.drive().length).toBe(1));
    expect(rig.drive()[0]).toEqual({ op: "paste", body: { text: "/compact", viewer: "ws-abc1234567" } });
    expect(rig.drive().some((c) => c.op === "enter")).toBe(false);
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(onTyped).toHaveBeenCalled();
    expect(document.activeElement).toBe(proxy);
  });

  it("a row press prevents mousedown so it can never take focus", async () => {
    mountSheet();
    await ready();
    expect(fireEvent.mouseDown(screen.getAllByTestId("kb-cmdp-row")[0])).toBe(false);
  });

  it("a multi-line paste entry is flattened to one line", async () => {
    mountSheet();
    await ready();
    fireEvent.click(screen.getByText("/multi"));
    await waitFor(() => expect(rig.drive().length).toBe(1));
    expect(rig.drive()[0].body?.text).toBe("first second");
  });

  it("a run-mode row carries enter and a refused Enter toasts", async () => {
    rig.queueDrive(200, { entered: false, enter_error: { status: 409, detail: "session is waiting on you" } });
    mountSheet();
    await ready();
    fireEvent.click(screen.getByText("/model"));
    await waitFor(() => expect(rig.drive().length).toBeGreaterThan(0));
    expect(rig.drive()[0].body).toMatchObject({ text: "/model", enter: true });
    await waitFor(() => expect(toastErr).toHaveBeenCalledWith("Enter not sent: session is waiting on you"));
  });

  it("an args entry shows the form, Send stays disabled until filled, and focus returns to the proxy after the send", async () => {
    const { proxy } = mountSheet();
    await ready();
    fireEvent.click(screen.getByText("/add"));
    const send = screen.getByRole("button", { name: "Send" }) as HTMLButtonElement;
    expect(send.disabled).toBe(true);
    const ta = document.querySelector(".kb-cmdp__field textarea") as HTMLTextAreaElement;
    ta.focus();
    expect(document.activeElement).toBe(ta);
    fireEvent.change(ta, { target: { value: "src/a.ts\nb" } });
    expect(send.disabled).toBe(false);
    send.focus();
    fireEvent.click(send);
    await waitFor(() => expect(rig.drive().length).toBe(1));
    expect(rig.drive()[0].body?.text).toBe("/add src/a.ts b");
    await waitFor(() => expect(document.activeElement).toBe(proxy));
  });

  it("Close and Esc both call onClose; the Close button never takes focus", async () => {
    const { proxy, onClose } = mountSheet();
    await ready();
    const close = screen.getByTestId("kb-drive-cmdsheet-close");
    expect(fireEvent.mouseDown(close)).toBe(false);
    expect(close.tabIndex).toBe(-1);
    act(() => {
      close.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, cancelable: true }));
    });
    expect(onClose).toHaveBeenCalledTimes(1);
    fireEvent.keyDown(screen.getByTestId("kb-drive-cmdsheet"), { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(2);
    expect(document.activeElement).toBe(proxy);
  });
});
