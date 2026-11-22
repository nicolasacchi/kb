// @vitest-environment jsdom
//
// FUPHONEMODE (PHONE-2) - the terminal panel's own phone predicate: the width
// breakpoint OR a coarse pointer with a short viewport height. The real
// predicate is pinned in a browser by spa-terminal-phone.spec.ts; here the
// query text and the hook's reactivity are pinned.
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, renderHook } from "@testing-library/react";
import { MOBILE_MAX_WIDTH, PHONE_PANEL_QUERY, useIsMobile, usePhonePanel } from "./useIsMobile";

const realInner = Object.getOwnPropertyDescriptor(window, "innerHeight");
function setViewport(height: number, screen?: { availWidth: number; availHeight: number }) {
  Object.defineProperty(window, "innerHeight", { configurable: true, value: height });
  vi.stubGlobal("screen", screen ?? { availWidth: 0, availHeight: 0 });
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  if (realInner) Object.defineProperty(window, "innerHeight", realInner);
});

const handlers = new Set<() => void>();
const asked: string[] = [];
function stubMedia(match: (q: string) => boolean) {
  asked.length = 0;
  handlers.clear();
  vi.stubGlobal("matchMedia", (q: string) => {
    asked.push(q);
    return {
      get matches() {
        return match(q);
      },
      addEventListener: (_: string, h: () => void) => handlers.add(h),
      removeEventListener: (_: string, h: () => void) => handlers.delete(h),
    };
  });
}

describe("usePhonePanel", () => {
  it("asks for the width breakpoint OR a coarse pointer with a short height, nothing else", () => {
    expect(PHONE_PANEL_QUERY).toContain(`(max-width: ${MOBILE_MAX_WIDTH}px)`);
    expect(PHONE_PANEL_QUERY).toContain("(pointer: coarse) and (max-height: 599px)");
    expect(PHONE_PANEL_QUERY.split(",").length).toBe(2);
  });

  it("is true when that query matches, and follows its change events", () => {
    let on = false;
    setViewport(400);
    stubMedia((q) => q === PHONE_PANEL_QUERY && on);
    const { result } = renderHook(() => usePhonePanel());
    expect(result.current).toBe(false);
    on = true;
    act(() => handlers.forEach((h) => h()));
    expect(result.current).toBe(true);
  });

  it("useIsMobile (the app shell) keeps the width-only query", () => {
    stubMedia(() => false);
    renderHook(() => useIsMobile());
    expect(asked.length).toBeGreaterThan(0);
    expect(asked.every((q) => q === `(max-width: ${MOBILE_MAX_WIDTH}px)`)).toBe(true);
  });
});

// FUMINPHONE (FUPHONEMODE follow-up) - a soft keyboard shrinks the viewport
// height, which must not turn a coarse landscape TABLET into a phone. The
// height arm of the query is latched against the device's own short side
// (screen.availWidth / availHeight) and the viewport height.
describe("usePhonePanel: the soft keyboard cannot flip a tablet to the phone branch", () => {
  // Only the combined query matches; the plain width query does not (width > 860).
  const heightArmOnly = () => stubMedia((q) => q === PHONE_PANEL_QUERY);

  it("a landscape tablet (screen 1280x800) whose viewport shrinks to 400 stays on the desktop branch", () => {
    setViewport(400, { availWidth: 1280, availHeight: 800 });
    heightArmOnly();
    const { result } = renderHook(() => usePhonePanel());
    expect(result.current).toBe(false);
  });

  it("a landscape phone (screen 915x412) is on the phone branch", () => {
    setViewport(380, { availWidth: 915, availHeight: 412 });
    heightArmOnly();
    const { result } = renderHook(() => usePhonePanel());
    expect(result.current).toBe(true);
  });

  it("the boundary: a device short side of 599 is a phone, 600 is not (keyboard open in both)", () => {
    setViewport(300, { availWidth: 1000, availHeight: 599 });
    heightArmOnly();
    expect(renderHook(() => usePhonePanel()).result.current).toBe(true);
    cleanup();
    setViewport(300, { availWidth: 1000, availHeight: 600 });
    heightArmOnly();
    expect(renderHook(() => usePhonePanel()).result.current).toBe(false);
  });

  it("without screen information the viewport height decides: 599 is a phone, 600 is not", () => {
    setViewport(599);
    heightArmOnly();
    expect(renderHook(() => usePhonePanel()).result.current).toBe(true);
    cleanup();
    setViewport(600);
    heightArmOnly();
    expect(renderHook(() => usePhonePanel()).result.current).toBe(false);
  });

  it("the width arm is never latched away: a narrow window is the phone branch whatever the screen says", () => {
    setViewport(900, { availWidth: 2560, availHeight: 1440 });
    stubMedia(() => true);
    expect(renderHook(() => usePhonePanel()).result.current).toBe(true);
  });

  it("a fine-pointer window (the query does not match) stays desktop", () => {
    setViewport(300, { availWidth: 400, availHeight: 300 });
    stubMedia(() => false);
    expect(renderHook(() => usePhonePanel()).result.current).toBe(false);
  });
});
