// @vitest-environment jsdom
//
// OL R2 — ONE reading line combining est. read time and "read by you"
// progress; `.kb-pinsp__readby` stays on it.
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render } from "@testing-library/react";
import type { ReadingSummary } from "../../api/reading";
import ReadingLine from "./ReadingLine";

afterEach(cleanup);

const summary = {
  completion_pct: 80,
  read_pct: 55,
  active_ms_total: 75_000,
  visit_count: 2,
  stopped_at: null,
} as unknown as ReadingSummary;

describe("ReadingLine", () => {
  it("puts est., scrolled, read, active and visits on one readby line", () => {
    const { container } = render(
      <ReadingLine readMin={21} progress={{ pct: 55, isDone: false }} summary={summary} />,
    );
    const lines = container.querySelectorAll(".kb-pinsp__readby");
    expect(lines).toHaveLength(1);
    const t = lines[0].textContent!.replace(/\s+/g, " ");
    expect(t).toContain("est. 21 min");
    expect(t).toContain("80% scrolled");
    expect(t).toContain("55% read");
    expect(t).toContain("active 1m 15s");
    expect(t).toContain("2 visits");
    expect(container.textContent).not.toContain("Read by you");
  });

  it("falls back to the live progress when nothing is persisted", () => {
    const { container } = render(
      <ReadingLine readMin={3} progress={{ pct: 12, isDone: false }} summary={null} />,
    );
    const t = container.querySelector(".kb-pinsp__readby")!.textContent!;
    expect(t).toContain("12%");
    expect(t).not.toContain("scrolled");
  });

  it("renders the stop line only when the summary has one", () => {
    const { container } = render(
      <ReadingLine
        readMin={3}
        summary={{ ...summary, stopped_at: { text: "Part two", pct: 40 } } as unknown as ReadingSummary}
      />,
    );
    expect(container.querySelector(".kb-pinsp__readby--stop")!.textContent).toContain(
      "Part two",
    );
  });
});
