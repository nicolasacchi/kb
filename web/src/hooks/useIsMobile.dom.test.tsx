// @vitest-environment jsdom
//
// FUPHONEMODE (PHONE-2) - the terminal panel's own phone predicate: the width
// breakpoint OR a coarse pointer with a short viewport height. The real
// predicate is pinned in a browser by spa-terminal-phone.spec.ts; here the
// query text and the hook's reactivity are pinned.
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, renderHook } from "@testing-library/react";
import { MOBILE_MAX_WIDTH, PHONE_PANEL_QUERY, useIsMobile, usePhonePanel } from "./useIsMobile";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
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
