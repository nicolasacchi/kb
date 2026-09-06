// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import InjectionHistogram from "./InjectionHistogram";

afterEach(() => cleanup());

describe("InjectionHistogram", () => {
  it("renders the empty state for an all-zero histogram", () => {
    render(<InjectionHistogram weekly={[0, 0, 0, 0, 0, 0, 0, 0]} />);
    expect(screen.getByTestId("injection-histogram-empty")).toBeTruthy();
    expect(screen.queryByTestId("injection-histogram")).toBeNull();
  });

  it("renders the empty state for an empty array (no data requested)", () => {
    render(<InjectionHistogram weekly={[]} />);
    expect(screen.getByTestId("injection-histogram-empty")).toBeTruthy();
  });

  it("renders one bar per bucket, oldest first", () => {
    render(<InjectionHistogram weekly={[3, 0, 1, 0, 0, 0, 0, 5]} />);
    const bars = screen.getAllByTestId("injection-histogram-bar");
    expect(bars).toHaveLength(8);
    // weekly[7] (oldest, count 5) renders FIRST (leftmost).
    expect(bars[0].className).toContain("on");
    // weekly[0] (this week, count 3) renders LAST (rightmost).
    expect(bars[7].className).toContain("on");
    // A zero bucket carries no "on" modifier.
    expect(bars[1].className).not.toContain("on");
  });

  it("summarises the total count in the title", () => {
    render(<InjectionHistogram weekly={[1, 2, 0, 0, 0, 0, 0, 0]} />);
    expect(screen.getByTestId("injection-histogram").title).toBe(
      "3 injections over the last 8 weeks",
    );
  });

  it("uses singular phrasing for exactly one injection", () => {
    render(<InjectionHistogram weekly={[1, 0, 0, 0, 0, 0, 0, 0]} />);
    expect(screen.getByTestId("injection-histogram").title).toBe(
      "1 injection over the last 8 weeks",
    );
  });

  // CT-C5 — the "N recalls · M referenced" explicit-reference caption.
  it("renders the used-count caption beside the bars", () => {
    render(<InjectionHistogram weekly={[1, 2, 0, 0, 0, 0, 0, 0]} usedCount={2} />);
    expect(screen.getByTestId("injection-histogram-used").textContent).toBe(
      "3 recalls · 2 referenced",
    );
  });

  it("defaults the used count to 0 when the prop is omitted", () => {
    render(<InjectionHistogram weekly={[1, 0, 0, 0, 0, 0, 0, 0]} />);
    expect(screen.getByTestId("injection-histogram-used").textContent).toBe(
      "1 recall · 0 referenced",
    );
  });

  it("carries the explicit-reference caveat in its title", () => {
    render(<InjectionHistogram weekly={[1, 0, 0, 0, 0, 0, 0, 0]} usedCount={1} />);
    const title = screen.getByTestId("injection-histogram-used").title;
    expect(title).toContain("EXPLICIT");
    expect(title).toContain("act on a recalled fact without ever naming it");
  });
});
