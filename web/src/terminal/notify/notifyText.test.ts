import { describe, expect, it } from "vitest";
import { eventText, TITLE_MAX } from "./notifyText";

describe("notifyText", () => {
  it("clips long titles and projects to TITLE_MAX characters with an ellipsis", () => {
    const t = eventText({ kind: "asks", termId: "a", title: "x".repeat(200), project: "p".repeat(100) });
    const [title, proj] = /^(.*) \((.*)\) needs you$/.exec(t)!.slice(1);
    expect(Array.from(title)).toHaveLength(TITLE_MAX);
    expect(title.endsWith("…")).toBe(true);
    expect(Array.from(proj)).toHaveLength(TITLE_MAX);
  });
  it("leaves a title of exactly TITLE_MAX untouched", () => {
    const title = "y".repeat(TITLE_MAX);
    expect(eventText({ kind: "finished", termId: "a", title, project: "p" })).toBe(`${title} (p) finished`);
  });
  it("has three summary wordings", () => {
    expect(eventText({ kind: "summary", count: 4, asks: 4, finished: 0 })).toBe("4 terminals need attention");
    expect(eventText({ kind: "summary", count: 5, asks: 0, finished: 5 })).toBe("5 terminals finished");
    expect(eventText({ kind: "summary", count: 6, asks: 2, finished: 4 })).toBe(
      "6 terminals need attention or finished",
    );
  });
});
