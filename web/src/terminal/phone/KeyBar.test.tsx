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
import { byId, maybeById, mkDrive, mount, press, rigFetch, settle, type FetchRig, type Mounted } from "./driveTestkit";
import type { Drive } from "./driveApi";

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

  describe("(c) the slash menu", () => {
    const slash = (): HTMLElement => byId(m!.host, "kb-drive-key-Slash");

    it("a held / opens the harness's menu; picking an entry pastes it and never enters", async () => {
      vi.useFakeTimers();
      bar(mkDrive({ harness: "omp" }));
      expect(maybeById(m!.host, "kb-drive-slashmenu")).toBeNull();
      press(slash(), "pointerdown");
      await act(async () => {
        vi.advanceTimersByTime(449);
      });
      expect(maybeById(m!.host, "kb-drive-slashmenu")).toBeNull();
      await act(async () => {
        vi.advanceTimersByTime(1);
      });
      press(slash(), "pointerup");
      const menu = byId(m!.host, "kb-drive-slashmenu");
      expect([...menu.querySelectorAll("button")].map((b) => b.textContent)).toEqual([
        "/compact",
        "/shake",
        "/tree",
        "Close",
      ]);
      press(byId(m!.host, "kb-drive-slash-shake"));
      await settle();
      expect(rig.urls()).toEqual(["/api/terms/abcd1234/paste"]);
      expect(rig.calls[0].json?.text).toBe("/shake");
      expect(maybeById(m!.host, "kb-drive-slashmenu")).toBeNull();
      // The hold's release sent no slash key either.
      expect(rig.calls).toHaveLength(1);
    });

    it("a tap on / sends the literal Slash key and opens no menu", async () => {
      vi.useFakeTimers();
      bar(mkDrive({ harness: "omp" }));
      press(slash(), "pointerdown");
      press(slash(), "pointerup");
      await settle();
      expect(maybeById(m!.host, "kb-drive-slashmenu")).toBeNull();
      expect(rig.urls()).toEqual(["/api/terms/abcd1234/keys"]);
      expect(keyBody(0)).toEqual(["Slash"]);
    });

    it("a harness with no menu treats / as an immediate key press", async () => {
      bar(mkDrive({ harness: "codex" }));
      press(slash(), "pointerdown");
      await settle();
      expect(keyBody(0)).toEqual(["Slash"]);
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
