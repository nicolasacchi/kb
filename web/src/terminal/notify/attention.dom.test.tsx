// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { act, cleanup, render } from "@testing-library/react";
import { __resetMarks, peekMarks, setMarks, useTermAttention } from "./attention";

afterEach(() => {
  cleanup();
  __resetMarks();
});

describe("useTermAttention", () => {
  it("renders the current marks and updates on change", () => {
    let seen: ReturnType<typeof useTermAttention> | null = null;
    function C() {
      seen = useTermAttention();
      return null;
    }
    render(<C />);
    expect(seen).toEqual({});
    act(() => setMarks({ a: { asks: true, finishedUnseen: false } }));
    expect(seen).toEqual({ a: { asks: true, finishedUnseen: false } });
    act(() => setMarks({}));
    expect(seen).toEqual({});
  });
  it("keeps the snapshot reference on an equal write", () => {
    setMarks({ a: { asks: false, finishedUnseen: true } });
    const r1 = peekMarks();
    setMarks({ a: { asks: false, finishedUnseen: true } });
    expect(peekMarks()).toBe(r1);
  });
});
