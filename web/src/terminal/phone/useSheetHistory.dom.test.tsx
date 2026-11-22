// @vitest-environment jsdom
//
// FUPHONEMODE (PHONE-4) - where the platform has no CloseWatcher (iOS Safari)
// the sheet owns ONE history entry while open: Back closes it and the page
// stays; closing it by button pops that entry; with a CloseWatcher, or with
// nothing open, history is untouched.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render } from "@testing-library/react";
import { StrictMode } from "react";
import { useSheetHistory } from "./useSheetHistory";

let popOwnEntry: () => boolean = () => false;
function Probe(props: { active: boolean; onClose: () => void }) {
  popOwnEntry = useSheetHistory(props.active, props.onClose);
  return null;
}

const tick = () => new Promise<void>((r) => setTimeout(r, 30));
const sheetKey = () => (window.history.state as { kbSheet?: string } | null)?.kbSheet;

beforeEach(() => {
  window.history.replaceState({ idx: 4 }, "", "/sessions/abc");
});
afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  delete (window as unknown as { CloseWatcher?: unknown }).CloseWatcher;
});

describe("useSheetHistory", () => {
  it("opening pushes one entry that keeps the router state; Back closes the sheet and the page stays", async () => {
    const onClose = vi.fn();
    const push = vi.spyOn(window.history, "pushState");
    render(<Probe active onClose={onClose} />);
    expect(push).toHaveBeenCalledTimes(1);
    expect(sheetKey()).toBeTruthy();
    expect((window.history.state as { idx: number }).idx).toBe(4);
    await act(async () => {
      window.history.back();
      await tick();
    });
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(window.location.pathname).toBe("/sessions/abc");
    expect(sheetKey()).toBeUndefined();
  });

  it("closing from the sheet's own button pops the entry and never calls onClose", async () => {
    const onClose = vi.fn();
    const push = vi.spyOn(window.history, "pushState");
    const r = render(<Probe active onClose={onClose} />);
    expect(push).toHaveBeenCalledTimes(1);
    expect(sheetKey()).toBeTruthy();
    // React flushes the effect cleanup when act exits: rerender first, wait after.
    act(() => {
      r.rerender(<Probe active={false} onClose={onClose} />);
    });
    await act(async () => {
      await tick();
    });
    expect(sheetKey()).toBeUndefined();
    expect(onClose).not.toHaveBeenCalled();
    expect(window.location.pathname).toBe("/sessions/abc");
  });

  it("with a CloseWatcher nothing is pushed (the shared hook already answers Back)", () => {
    (window as unknown as { CloseWatcher: unknown }).CloseWatcher = class {};
    const before = window.history.length;
    render(<Probe active onClose={() => {}} />);
    expect(window.history.length).toBe(before);
    expect(sheetKey()).toBeUndefined();
  });

  it("with no sheet open Back is unchanged and onClose is never called", async () => {
    const onClose = vi.fn();
    window.history.pushState({ idx: 5 }, "", "/sessions/abc");
    const before = window.history.length;
    render(<Probe active={false} onClose={onClose} />);
    expect(window.history.length).toBe(before);
    await act(async () => {
      window.history.back();
      await tick();
    });
    expect(onClose).not.toHaveBeenCalled();
  });

  it("StrictMode's effect replay keeps exactly one entry", async () => {
    const push = vi.spyOn(window.history, "pushState");
    render(
      <StrictMode>
        <Probe active onClose={() => {}} />
      </StrictMode>,
    );
    await act(async () => {
      await tick();
    });
    expect(push).toHaveBeenCalledTimes(1);
    expect(sheetKey()).toBeTruthy();
  });

  it("an entry the page already left (a route change) is not popped", async () => {
    const r = render(<Probe active onClose={() => {}} />);
    window.history.pushState({ idx: 9 }, "", "/other");
    const len = window.history.length;
    act(() => {
      r.unmount();
    });
    await act(async () => {
      await tick();
    });
    expect(window.history.length).toBe(len);
    expect(window.location.pathname).toBe("/other");
  });

  // FUTERMLIST interaction: opening a terminal pushes `?term=<id>` (entry below);
  // the sheet's own entry sits on top with the same URL.
  it("the close button pops the sheet entry so the ?term= entry below is replaced, not left to re-open the sheet", async () => {
    window.history.replaceState({ idx: 4 }, "", "/sessions?term=t1");
    const entryBelow = window.history.length;
    // The page's close handler replaces ?term= on whichever entry is current.
    const onClose = vi.fn(() => window.history.replaceState({ idx: 4 }, "", "/sessions"));
    render(<Probe active onClose={onClose} />);
    expect(sheetKey()).toBeTruthy();
    expect(window.history.length).toBe(entryBelow + 1);
    let popped = false;
    await act(async () => {
      popped = popOwnEntry();
      await tick();
    });
    expect(popped).toBe(true);
    expect(onClose).toHaveBeenCalledTimes(1);
    // Back from here must not land on a ?term= entry: the one below was rewritten.
    expect(window.location.search).toBe("");
    expect(sheetKey()).toBeUndefined();
    // With the entry gone, a second request is a no-op the caller closes itself.
    expect(popOwnEntry()).toBe(false);
  });

  it("a second Close before the popstate does not call history.back() again", async () => {
    const onClose = vi.fn();
    render(<Probe active onClose={onClose} />);
    const back = vi.spyOn(window.history, "back");
    let r: boolean[] = [];
    await act(async () => {
      r = [popOwnEntry(), popOwnEntry()];
      await tick();
    });
    expect(r).toEqual([true, true]);
    expect(back).toHaveBeenCalledTimes(1);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  // FUMINPHONE: switching between terminals remounts the sheet's hook (new key)
  // while the old entry is still the current one. The new owner must adopt it:
  // two stacked entries would need two Backs to leave one sheet.
  it("switching to another terminal keeps ONE entry: a single Back closes the new sheet once", async () => {
    const closeA = vi.fn();
    const closeB = vi.fn();
    const push = vi.spyOn(window.history, "pushState");
    const r = render(<Probe key="a" active onClose={closeA} />);
    expect(push).toHaveBeenCalledTimes(1);
    act(() => {
      r.rerender(<Probe key="b" active onClose={closeB} />);
    });
    await act(async () => {
      await tick();
    });
    // The new sheet adopted A's entry: still exactly one push in total.
    expect(push).toHaveBeenCalledTimes(1);
    expect(sheetKey()).toBeTruthy();
    await act(async () => {
      window.history.back();
      await tick();
    });
    expect(closeB).toHaveBeenCalledTimes(1);
    expect(closeA).not.toHaveBeenCalled();
    expect(sheetKey()).toBeUndefined();
  });

  it("after a switch the new sheet's own Close pops the adopted entry exactly once", async () => {
    const closeB = vi.fn();
    const r = render(<Probe key="a" active onClose={() => {}} />);
    act(() => {
      r.rerender(<Probe key="b" active onClose={closeB} />);
    });
    await act(async () => {
      await tick();
    });
    const back = vi.spyOn(window.history, "back");
    let popped = false;
    await act(async () => {
      popped = popOwnEntry();
      await tick();
    });
    expect(popped).toBe(true);
    expect(back).toHaveBeenCalledTimes(1);
    expect(closeB).toHaveBeenCalledTimes(1);
    expect(sheetKey()).toBeUndefined();
  });

  it("a reload with the sheet's entry still current adopts it instead of pushing a second", async () => {
    window.history.replaceState({ idx: 4, kbSheet: "left-over" }, "", "/sessions?term=t1");
    const push = vi.spyOn(window.history, "pushState");
    const onClose = vi.fn();
    render(<Probe active onClose={onClose} />);
    expect(push).not.toHaveBeenCalled();
    await act(async () => {
      popOwnEntry();
      await tick();
    });
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
