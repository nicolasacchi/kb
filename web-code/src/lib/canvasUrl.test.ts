import { describe, expect, it } from "vitest";
import { canvasUrl, parseCanvasSearch } from "./canvasUrl";

describe("canvasUrl / parseCanvasSearch", () => {
  it("builds the sentinel with optional id and review", () => {
    expect(canvasUrl("kb")).toBe("/r/kb/~canvas");
    expect(canvasUrl("kb", { id: 3 })).toBe("/r/kb/~canvas?id=3");
    expect(canvasUrl("kb", { id: 3, review: 9 })).toBe("/r/kb/~canvas?id=3&review=9");
    expect(canvasUrl("my repo")).toBe("/r/my%20repo/~canvas");
  });

  it("parses id/review from search params", () => {
    expect(parseCanvasSearch(new URLSearchParams("id=12&review=4"))).toEqual({
      id: 12,
      review: 4,
    });
    expect(parseCanvasSearch(new URLSearchParams(""))).toEqual({ id: null, review: null });
    expect(parseCanvasSearch(new URLSearchParams("id=nope"))).toEqual({ id: null, review: null });
  });
});
