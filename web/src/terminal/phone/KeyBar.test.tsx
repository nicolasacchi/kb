// @vitest-environment jsdom
// T18 — the key bar: never takes focus, holds for Ctrl-C and the slash menu,
// sends key NAMES, and Esc on it never closes the Sheet.

import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { Sheet } from "@kb/ui/Sheet";
import { toast } from "../../lib/toast";
import { terminalOwnsEscape } from "./escape";
import { CAPTION } from "./planSend";
import { KeyBar, moreToRight } from "./KeyBar";
import { byId, click, maybeById, mkDrive, mount, press, rigFetch, settle, type FetchRig, type Mounted } from "./driveTestkit";
import type { Drive } from "./driveApi";
import { DirectInputContext, type DirectInput } from "./directInput";
import { createDirectLine } from "./directLine";
import { createStickyMods } from "./stickyMods";
import { wrap } from "../commands/palette/paletteTestkit";

// CMDSHEET: the sheet's list reads the identity gate; not admitted = the shipped catalogue, no fetch.
vi.mock("../../hooks/useArtifactHost", async (orig) => ({
  ...(await orig<typeof import("../../hooks/useArtifactHost")>()),
  useIdentity: () => ({ terminal_admitted: false }),
}));

let m: Mounted | null = null;
let rig: FetchRig;

beforeEach(() => {
  rig = rigFetch();
});

afterEach(() => {
  m?.unmount();
  m = null;
  vi.useRealTimers();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

function bar(drive: Drive, onCaption = vi.fn(), onTyped = vi.fn()) {
  m = mount(<KeyBar drive={drive} onCaption={onCaption} onTyped={onTyped} />);
  return { onCaption, onTyped };
}

function sheetBar(drive: Drive) {
  m = mount(wrap(<KeyBar drive={drive} onCaption={vi.fn()} onTyped={vi.fn()} />));
}

const keyBody = (i: number): unknown => rig.calls[i].json?.keys;

describe("KeyBar", () => {
  it("(a) a press is prevented on pointerdown AND mousedown, the buttons are tabIndex -1, and focus stays on the textarea", () => {
    const ta = document.createElement("textarea");
    document.body.appendChild(ta);
    ta.focus();
    bar(mkDrive());
    const buttons = [...m!.host.querySelectorAll("button")];
    expect(buttons.length).toBeGreaterThanOrEqual(11);
    for (const b of buttons) {
      expect(b.tabIndex, b.dataset.testid).toBe(-1);
      expect(press(b, "mousedown").defaultPrevented, `${b.dataset.testid} mousedown`).toBe(true);
    }
    const tab = byId(m!.host, "kb-drive-key-Tab");
    expect(press(tab, "pointerdown").defaultPrevented).toBe(true);
    expect(document.activeElement).toBe(ta);
    ta.remove();
  });

  it("sends each plain key as its NAME, once, on pointerdown", async () => {
    bar(mkDrive());
    for (const name of ["Esc", "Tab", "S-Tab", "Up", "Down", "Left", "Right", "C-u"]) {
      press(byId(m!.host, `kb-drive-key-${name}`));
    }
    await settle();
    expect(rig.urls().every((u) => u === "/api/terms/abcd1234/keys")).toBe(true);
    expect(rig.calls.map((c) => (c.json?.keys as string[])[0])).toEqual([
      "Esc",
      "Tab",
      "S-Tab",
      "Up",
      "Down",
      "Left",
      "Right",
      "C-u",
    ]);
    for (const c of rig.calls) expect(c.json?.viewer).toBe("ws-abc1234567");
  });

  it("FUKEYBAR: Backspace sends the name BSpace on pointerdown and never takes focus", async () => {
    const ta = document.createElement("textarea");
    document.body.appendChild(ta);
    ta.focus();
    bar(mkDrive());
    const b = byId(m!.host, "kb-drive-key-BSpace");
    expect(b.tabIndex).toBe(-1);
    expect(press(b, "mousedown").defaultPrevented).toBe(true);
    press(b);
    await settle();
    expect(rig.urls()).toEqual(["/api/terms/abcd1234/keys"]);
    expect(keyBody(0)).toEqual(["BSpace"]);
    expect(document.activeElement).toBe(ta);
    ta.remove();
  });

  it("FUKEYBAR: Ctrl-D is a hold (a tap says so and sends nothing; 600 ms sends one C-d)", async () => {
    vi.useFakeTimers();
    const { onCaption } = bar(mkDrive());
    const d = (): HTMLElement => byId(m!.host, "kb-drive-key-C-d");
    press(d(), "pointerdown");
    press(d(), "pointerup");
    await act(async () => {
      vi.advanceTimersByTime(2000);
    });
    expect(rig.calls).toHaveLength(0);
    expect(onCaption).toHaveBeenLastCalledWith(CAPTION.holdCtrlD);
    press(d(), "pointerdown");
    await act(async () => {
      vi.advanceTimersByTime(600);
    });
    await settle();
    press(d(), "pointerup");
    expect(rig.calls).toHaveLength(1);
    expect(keyBody(0)).toEqual(["C-d"]);
  });

  it("the Enter button is the enter op (not a key), and a 409 shows its reason as the caption", async () => {
    rig.reply(409, { type: "urn:kb:errors:terminal-conflict", detail: "output never went quiet" });
    const { onCaption } = bar(mkDrive());
    press(byId(m!.host, "kb-drive-key-Enter"));
    await settle();
    expect(rig.urls()).toEqual(["/api/terms/abcd1234/enter"]);
    expect(onCaption).toHaveBeenLastCalledWith(CAPTION.enterRefused("output never went quiet"));
  });

  it("a failed key is a toast, never a silent catch", async () => {
    const err = vi.spyOn(toast, "err");
    rig.reply(503, { type: "urn:kb:errors:terminal-runner-unavailable", detail: "terminal runner unavailable" });
    bar(mkDrive());
    press(byId(m!.host, "kb-drive-key-Tab"));
    await settle();
    expect(err).toHaveBeenCalledWith("terminal runner unavailable");
  });

  describe("(b) Ctrl-C is a hold", () => {
    const ctrlC = (): HTMLElement => byId(m!.host, "kb-drive-key-C-c");

    it("a quick tap sends nothing and says to hold", async () => {
      vi.useFakeTimers();
      const { onCaption } = bar(mkDrive());
      press(ctrlC(), "pointerdown");
      press(ctrlC(), "pointerup");
      await act(async () => {
        vi.advanceTimersByTime(2000);
      });
      expect(rig.calls).toHaveLength(0);
      expect(onCaption).toHaveBeenLastCalledWith("Hold to send Ctrl-C");
    });

    it("releasing just before 600 ms sends nothing", async () => {
      vi.useFakeTimers();
      bar(mkDrive());
      press(ctrlC(), "pointerdown");
      await act(async () => {
        vi.advanceTimersByTime(599);
      });
      press(ctrlC(), "pointerup");
      await act(async () => {
        vi.advanceTimersByTime(2000);
      });
      expect(rig.calls).toHaveLength(0);
    });

    it("holding 600 ms sends exactly one C-c, and the release adds nothing", async () => {
      vi.useFakeTimers();
      const { onTyped } = bar(mkDrive());
      press(ctrlC(), "pointerdown");
      await act(async () => {
        vi.advanceTimersByTime(600);
      });
      await settle();
      expect(rig.calls).toHaveLength(1);
      expect(keyBody(0)).toEqual(["C-c"]);
      press(ctrlC(), "pointerup");
      await act(async () => {
        vi.advanceTimersByTime(5000);
      });
      await settle();
      expect(rig.calls).toHaveLength(1);
      expect(onTyped).toHaveBeenCalledTimes(1);
    });

    it("dragging off the button, or a system cancel, ends the hold without sending", async () => {
      vi.useFakeTimers();
      bar(mkDrive());
      for (const type of ["pointerout", "pointercancel"]) {
        press(ctrlC(), "pointerdown");
        act(() => {
          // React derives `onPointerLeave` from `pointerout` (target left, no related target).
          ctrlC().dispatchEvent(new PointerEvent(type, { bubbles: true, pointerId: 1, relatedTarget: null }));
        });
        await act(async () => {
          vi.advanceTimersByTime(2000);
        });
      }
      expect(rig.calls).toHaveLength(0);
    });
  });

  describe("(c) the command sheet", () => {
    const slash = (): HTMLElement => byId(m!.host, "kb-drive-key-Slash");
    const cmds = (): HTMLElement => byId(m!.host, "kb-drive-key-cmds");
    const sheet = (): HTMLElement | null => maybeById(m!.host, "kb-drive-cmdsheet");

    it("the Cmds key toggles the sheet on pointerdown, never takes focus, and keeps the proxy focused", () => {
      const ta = document.createElement("textarea");
      document.body.appendChild(ta);
      ta.focus();
      sheetBar(mkDrive({ harness: "omp" }));
      expect(cmds().tabIndex).toBe(-1);
      expect(cmds().getAttribute("aria-expanded")).toBe("false");
      expect(press(cmds(), "mousedown").defaultPrevented).toBe(true);
      expect(sheet()).toBeNull();
      expect(press(cmds(), "pointerdown").defaultPrevented).toBe(true);
      expect(sheet()).not.toBeNull();
      expect(cmds().getAttribute("aria-expanded")).toBe("true");
      expect(document.activeElement).toBe(ta);
      press(cmds(), "pointerdown");
      expect(sheet()).toBeNull();
      expect(document.activeElement).toBe(ta);
      ta.remove();
    });

    it("the sheet lists the bound harness's catalogue and a tapped row pastes it, never entering", async () => {
      const ta = document.createElement("textarea");
      document.body.appendChild(ta);
      ta.focus();
      sheetBar(mkDrive({ harness: "omp" }));
      press(cmds());
      const rows = [...sheet()!.querySelectorAll('[data-testid="kb-cmdp-row"]')];
      expect(rows.map((r) => r.querySelector(".kb-cmdp__label")?.textContent)).toContain("/shake");
      expect(sheet()!.textContent).not.toContain("/help");
      const shake = rows.find((r) => r.querySelector(".kb-cmdp__label")?.textContent === "/shake") as HTMLElement;
      expect(press(shake, "mousedown").defaultPrevented).toBe(true);
      click(shake);
      await settle();
      expect(rig.urls()).toEqual(["/api/terms/abcd1234/paste"]);
      expect(rig.calls[0].json).toEqual({ text: "/shake", viewer: "ws-abc1234567" });
      expect(sheet()).toBeNull();
      expect(document.activeElement).toBe(ta);
      ta.remove();
    });

    it("the Close button closes the sheet without taking focus", () => {
      const ta = document.createElement("textarea");
      document.body.appendChild(ta);
      ta.focus();
      sheetBar(mkDrive());
      press(cmds());
      const close = byId(m!.host, "kb-drive-cmdsheet-close");
      expect(press(close, "mousedown").defaultPrevented).toBe(true);
      expect(press(close).defaultPrevented).toBe(true);
      expect(sheet()).toBeNull();
      expect(document.activeElement).toBe(ta);
      ta.remove();
    });

    it("a held / opens the same sheet and the hold's release sends no key", async () => {
      vi.useFakeTimers();
      sheetBar(mkDrive({ harness: "omp" }));
      expect(sheet()).toBeNull();
      press(slash(), "pointerdown");
      await act(async () => {
        vi.advanceTimersByTime(449);
      });
      expect(sheet()).toBeNull();
      await act(async () => {
        vi.advanceTimersByTime(1);
      });
      press(slash(), "pointerup");
      expect(sheet()).not.toBeNull();
      expect(rig.calls).toHaveLength(0);
    });

    it("a tap on / sends the literal Slash key and opens no sheet", async () => {
      vi.useFakeTimers();
      sheetBar(mkDrive({ harness: "omp" }));
      press(slash(), "pointerdown");
      press(slash(), "pointerup");
      await settle();
      expect(sheet()).toBeNull();
      expect(rig.urls()).toEqual(["/api/terms/abcd1234/keys"]);
      expect(keyBody(0)).toEqual(["Slash"]);
    });

    it("the old hard-coded slash menu is gone", () => {
      sheetBar(mkDrive({ harness: "omp" }));
      press(cmds());
      expect(maybeById(m!.host, "kb-drive-slashmenu")).toBeNull();
    });
  });

  describe("(e) the Claude guard on Left", () => {
    it("asks first, and sends only once confirmed", async () => {
      const confirm = vi.fn(async () => {
        expect(rig.calls, "confirm runs before the request").toHaveLength(0);
        return true;
      });
      bar(mkDrive({ harness: "claude", confirm }));
      press(byId(m!.host, "kb-drive-key-Left"));
      await settle();
      expect(confirm).toHaveBeenCalledTimes(1);
      expect(keyBody(0)).toEqual(["Left"]);
    });

    it("declining sends nothing", async () => {
      const confirm = vi.fn(async () => false);
      bar(mkDrive({ harness: "claude", confirm }));
      press(byId(m!.host, "kb-drive-key-Left"));
      await settle();
      expect(rig.calls).toHaveLength(0);
    });

    it("asks on every Left (the SPA cannot see the prompt)", async () => {
      const confirm = vi.fn(async () => true);
      bar(mkDrive({ harness: "claude", confirm }));
      press(byId(m!.host, "kb-drive-key-Left"));
      press(byId(m!.host, "kb-drive-key-Left"));
      await settle();
      expect(confirm).toHaveBeenCalledTimes(2);
    });
  });

  it("(d) Esc sends the Escape key and does not close the Sheet, even with focus in the composer", async () => {
    const onClose = vi.fn();
    m = mount(
      <Sheet
        open
        detent="full"
        onDetentChange={() => {}}
        onClose={onClose}
        title="Terminal"
        ignoreEscape={terminalOwnsEscape}
      >
        <div data-kb-term-root="">
          <KeyBar drive={mkDrive()} onCaption={() => {}} />
          <textarea data-testid="ta" />
        </div>
      </Sheet>,
    );
    press(byId(document.body, "kb-drive-key-Esc"));
    await settle();
    expect(keyBody(0)).toEqual(["Esc"]);
    expect(onClose).not.toHaveBeenCalled();

    // An Escape KEYBOARD event from the composer is the terminal's, not the Sheet's.
    const ta = byId(document.body, "ta");
    ta.focus();
    act(() => {
      ta.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    });
    expect(onClose).not.toHaveBeenCalled();

    // Control: the same keystroke on the sheet itself, outside the terminal root, still closes it.
    const panel = document.querySelector("[data-kb-sheet]") as HTMLElement;
    act(() => {
      panel.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }));
    });
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});

describe("FUTERMPOLISH W6d: the key row that scrolls says so", () => {
  it("moreToRight: true only while keys sit past the right edge", () => {
    expect(moreToRight({ scrollLeft: 0, clientWidth: 360, scrollWidth: 600 })).toBe(true);
    expect(moreToRight({ scrollLeft: 100, clientWidth: 360, scrollWidth: 600 })).toBe(true);
    expect(moreToRight({ scrollLeft: 240, clientWidth: 360, scrollWidth: 600 })).toBe(false);
    expect(moreToRight({ scrollLeft: 0, clientWidth: 600, scrollWidth: 600 })).toBe(false);
    // not laid out yet (jsdom, a hidden tree): no fade
    expect(moreToRight({ scrollLeft: 0, clientWidth: 0, scrollWidth: 600 })).toBe(false);
  });

  it("the row carries data-more=right while it overflows, and loses it at the end of the scroll", () => {
    const sw = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollWidth");
    const cw = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientWidth");
    Object.defineProperty(HTMLElement.prototype, "scrollWidth", { configurable: true, get: () => 600 });
    Object.defineProperty(HTMLElement.prototype, "clientWidth", { configurable: true, get: () => 360 });
    try {
      bar(mkDrive());
      const keys = byId(m!.host, "kb-drive-keys");
      expect(keys.getAttribute("data-more")).toBe("right");
      keys.scrollLeft = 240;
      act(() => {
        keys.dispatchEvent(new Event("scroll"));
      });
      expect(keys.getAttribute("data-more")).toBeNull();
    } finally {
      if (sw) Object.defineProperty(HTMLElement.prototype, "scrollWidth", sw);
      else delete (HTMLElement.prototype as unknown as Record<string, unknown>).scrollWidth;
      if (cw) Object.defineProperty(HTMLElement.prototype, "clientWidth", cw);
      else delete (HTMLElement.prototype as unknown as Record<string, unknown>).clientWidth;
    }
  });
});

// PHIN2 - the key bar over the proxy's WS path, with the sticky Ctrl.
describe("KeyBar over the direct-typing WS path (PHIN2)", () => {
  function directBar(over: Partial<DirectInput> = {}, drive: Drive = mkDrive()) {
    let clock = 0;
    const send = vi.fn((_d: string) => true);
    const line = createDirectLine();
    line.configure(drive.harness, drive.confirm);
    const direct: DirectInput = {
      send,
      mods: createStickyMods(() => clock),
      appCursor: () => false,
      harness: drive.harness,
      confirm: drive.confirm,
      line,
      ...over,
    };
    const onTyped = vi.fn();
    m = mount(
      wrap(
        <DirectInputContext.Provider value={direct}>
          <KeyBar drive={drive} onCaption={vi.fn()} onTyped={onTyped} />
        </DirectInputContext.Provider>,
      ),
    );
    return { direct, send, onTyped, tick: (ms: number) => (clock += ms) };
  }
  const ctrl = (): HTMLElement => byId(m!.host, "kb-drive-key-ctrl");
  const tap = (name: string) => press(byId(m!.host, `kb-drive-key-${name}`));

  it("a key is its raw bytes on the WS, with no REST call", async () => {
    const { send, onTyped } = directBar();
    for (const name of ["Esc", "Tab", "S-Tab", "Up", "BSpace", "C-u"]) tap(name);
    // `/` is a hold key: a tap is pointerdown then pointerup.
    press(byId(m!.host, "kb-drive-key-Slash"), "pointerdown");
    press(byId(m!.host, "kb-drive-key-Slash"), "pointerup");
    await settle();
    expect(send.mock.calls.map((c) => c[0])).toEqual(["\x1b", "\t", "\x1b[Z", "\x1b[A", "\x7f", "\x15", "/"]);
    expect(rig.calls).toHaveLength(0);
    expect(onTyped).toHaveBeenCalledTimes(7);
  });

  it("the arrows follow application cursor-keys mode", () => {
    const { send } = directBar({ appCursor: () => true });
    tap("Up");
    tap("Left");
    expect(send.mock.calls.map((c) => c[0])).toEqual(["\x1bOA", "\x1bOD"]);
  });

  it("without the context the bar is the REST bar (no Ctrl button)", async () => {
    bar(mkDrive());
    expect(maybeById(m!.host, "kb-drive-key-ctrl")).toBeNull();
    tap("Esc");
    await settle();
    expect(rig.urls()).toEqual(["/api/terms/abcd1234/keys"]);
  });

  it("Ctrl is first in the bar and aria-pressed goes false, true (armed), true (locked), false", () => {
    const { tick } = directBar();
    expect(byId(m!.host, "kb-drive-keys").firstElementChild).toBe(ctrl());
    expect(ctrl().getAttribute("aria-pressed")).toBe("false");
    expect(ctrl().textContent).toBe("Ctrl");
    press(ctrl());
    expect(ctrl().getAttribute("aria-pressed")).toBe("true");
    expect(ctrl().dataset.mod).toBe("armed");
    tick(100);
    press(ctrl());
    expect(ctrl().getAttribute("aria-pressed")).toBe("true");
    expect(ctrl().dataset.mod).toBe("locked");
    expect(ctrl().textContent).toBe("Ctrl lock");
    press(ctrl());
    expect(ctrl().getAttribute("aria-pressed")).toBe("false");
  });

  it("an armed Ctrl takes the next bar key once: Esc and y go plain, Up becomes Ctrl+Up", () => {
    const { send } = directBar();
    press(ctrl());
    tap("Esc");
    expect(ctrl().getAttribute("aria-pressed")).toBe("false");
    press(ctrl());
    tap("Up");
    press(ctrl());
    tap("Right");
    tap("Right");
    expect(send.mock.calls.map((c) => c[0])).toEqual(["\x1b", "\x1b[1;5A", "\x1b[1;5C", "\x1b[C"]);
  });

  it("a key Ctrl does not change consumes the latch and goes plain", () => {
    const { send } = directBar();
    press(ctrl());
    tap("Tab");
    tap("Tab");
    expect(send.mock.calls.map((c) => c[0])).toEqual(["\t", "\t"]);
    expect(ctrl().getAttribute("aria-pressed")).toBe("false");
  });

  it("a locked Ctrl keeps applying until tapped off", () => {
    const { send, tick } = directBar();
    press(ctrl());
    tick(100);
    press(ctrl());
    tap("Up");
    tap("Down");
    expect(send.mock.calls.map((c) => c[0])).toEqual(["\x1b[1;5A", "\x1b[1;5B"]);
    expect(ctrl().dataset.mod).toBe("locked");
  });

  it("a refused WS send toasts and sends no REST fallback", async () => {
    const err = vi.spyOn(toast, "err");
    directBar({ send: vi.fn(() => false) });
    tap("Esc");
    await settle();
    expect(err).toHaveBeenCalled();
    expect(rig.calls).toHaveLength(0);
  });

  it("the Ctrl-C 600 ms hold still works and sends the byte over the WS", async () => {
    vi.useFakeTimers();
    const { send } = directBar();
    const c = (): HTMLElement => byId(m!.host, "kb-drive-key-C-c");
    press(c(), "pointerdown");
    press(c(), "pointerup");
    await act(async () => {
      vi.advanceTimersByTime(2000);
    });
    expect(send).not.toHaveBeenCalled();
    press(c(), "pointerdown");
    await act(async () => {
      vi.advanceTimersByTime(600);
    });
    expect(send.mock.calls.map((x) => x[0])).toEqual(["\x03"]);
  });

  it("the Left guard still confirms on claude, and declining sends nothing", async () => {
    const confirm = vi.fn(async () => false);
    const { send } = directBar({}, mkDrive({ harness: "claude", confirm }));
    tap("Left");
    await settle();
    expect(confirm).toHaveBeenCalledTimes(1);
    expect(send).not.toHaveBeenCalled();
    confirm.mockResolvedValueOnce(true);
    tap("Left");
    await settle();
    expect(send.mock.calls.map((x) => x[0])).toEqual(["\x1b[D"]);
  });

  it("Enter is a CR on the WS; a typed /bg on claude asks first and only a yes sends it", async () => {
    const confirm = vi.fn(async () => false);
    const { send, direct } = directBar({}, mkDrive({ harness: "claude", confirm }));
    tap("Enter");
    expect(send.mock.calls.map((x) => x[0])).toEqual(["\r"]);
    expect(rig.calls).toHaveLength(0);
    send.mockClear();
    direct.line.feed("/bg");
    tap("Enter");
    await settle();
    expect(confirm).toHaveBeenCalledTimes(1);
    expect(send).not.toHaveBeenCalled();
    confirm.mockResolvedValueOnce(true);
    tap("Enter");
    await settle();
    expect(send.mock.calls.map((x) => x[0])).toEqual(["\r"]);
  });

  it("a bar tap never blurs the focused proxy and is default-prevented", () => {
    const ta = document.createElement("textarea");
    document.body.appendChild(ta);
    ta.focus();
    directBar();
    const ev = press(byId(m!.host, "kb-drive-key-Esc"), "pointerdown");
    expect(ev.defaultPrevented).toBe(true);
    expect(press(ctrl(), "pointerdown").defaultPrevented).toBe(true);
    expect(press(ctrl(), "mousedown").defaultPrevented).toBe(true);
    expect(ctrl().tabIndex).toBe(-1);
    expect(document.activeElement).toBe(ta);
    ta.remove();
  });

  it("direct typing: a sheet entry is still PASTED over REST, never entered", async () => {
    directBar();
    press(byId(m!.host, "kb-drive-key-cmds"));
    const row = [...m!.host.querySelectorAll('[data-testid="kb-cmdp-row"]')].find(
      (r) => r.querySelector(".kb-cmdp__label")?.textContent === "/compact",
    ) as HTMLElement;
    click(row);
    await settle();
    // Drive calls only: an unrelated identity read may share the stubbed fetch.
    const drive = rig.calls.filter((c) => c.url.startsWith("/api/terms/"));
    expect(drive.map((c) => c.url)).toEqual(["/api/terms/abcd1234/paste"]);
    expect(drive[0].json).toMatchObject({ text: "/compact" });
    expect(drive[0].json?.enter).toBeUndefined();
  });
});
