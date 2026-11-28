// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { act, cleanup, render } from "@testing-library/react";
import {
  JUMP_TTL_MS,
  clearPendingJump,
  hasPendingJump,
  requestPendingJump,
  resetPendingJumpForTests,
  usePendingJumpTick,
} from "./pendingJump";

function Probe() {
  return <div data-testid="tick">{usePendingJumpTick()}</div>;
}

afterEach(() => {
  cleanup();
  resetPendingJumpForTests();
  window.localStorage.clear();
});

describe("pendingJump", () => {
  it("each request bumps the tick and notifies subscribers", () => {
    const { getByTestId } = render(<Probe />);
    expect(getByTestId("tick").textContent).toBe("0");
    act(() => requestPendingJump());
    expect(getByTestId("tick").textContent).toBe("1");
    act(() => requestPendingJump());
    expect(getByTestId("tick").textContent).toBe("2");
  });

  it("the request is live until cleared or expired", () => {
    expect(hasPendingJump(1000)).toBe(false);
    requestPendingJump(1000);
    expect(hasPendingJump(1000 + JUMP_TTL_MS)).toBe(true);
    expect(hasPendingJump(1000 + JUMP_TTL_MS + 1)).toBe(false);
    clearPendingJump();
    expect(hasPendingJump(1000)).toBe(false);
  });

  it("persists nothing", () => {
    requestPendingJump();
    expect(window.localStorage.length).toBe(0);
    expect(window.sessionStorage.length).toBe(0);
  });
});
