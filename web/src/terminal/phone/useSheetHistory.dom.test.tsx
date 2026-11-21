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

function Probe(props: { active: boolean; onClose: () => void }) {
  useSheetHistory(props.active, props.onClose);
  return null;
}

const tick = () => new Promise<void>((r) => setTimeout(r, 30));
const sheetKey = () => (window.history.state as { kbSheet?: string } | null)?.kbSheet;

beforeEach(() => {
  window.history.replaceState({ idx: 4 }, "", "/sessions/abc");
});
afterEach(() => {
  cleanup();
  delete (window as unknown as { CloseWatcher?: unknown }).CloseWatcher;
});

describe("useSheetHistory", () => {
  it("opening pushes one entry that keeps the router state; Back closes the sheet and the page stays", async () => {
    const onClose = vi.fn();
    const before = window.history.length;
    render(<Probe active onClose={onClose} />);
    expect(window.history.length).toBe(before + 1);
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
    const before = window.history.length;
    const r = render(<Probe active onClose={onClose} />);
    expect(window.history.length).toBe(before + 1);
    await act(async () => {
      r.rerender(<Probe active={false} onClose={onClose} />);
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
    const before = window.history.length;
    render(
      <StrictMode>
        <Probe active onClose={() => {}} />
      </StrictMode>,
    );
    await act(async () => {
      await tick();
    });
    expect(window.history.length).toBe(before + 1);
    expect(sheetKey()).toBeTruthy();
  });

  it("an entry the page already left (a route change) is not popped", async () => {
    const r = render(<Probe active onClose={() => {}} />);
    window.history.pushState({ idx: 9 }, "", "/other");
    const len = window.history.length;
    await act(async () => {
      r.unmount();
      await tick();
    });
    expect(window.history.length).toBe(len);
    expect(window.location.pathname).toBe("/other");
  });
});
