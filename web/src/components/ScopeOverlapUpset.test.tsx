// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import ScopeOverlapUpset from "./ScopeOverlapUpset";

afterEach(() => cleanup());

describe("ScopeOverlapUpset", () => {
  it("renders the empty state for no memories (failure path: nothing to aggregate)", () => {
    render(<ScopeOverlapUpset hits={[]} onPivotKb={vi.fn()} />);
    expect(screen.getByTestId("scope-overlap-empty")).toBeTruthy();
    expect(screen.queryByTestId("scope-overlap-row")).toBeNull();
  });

  it("renders one row per distinct scope combination", () => {
    render(
      <ScopeOverlapUpset
        hits={[
          { kb: "alpha", global: false, linked_kbs: [] },
          { kb: "alpha", global: false, linked_kbs: [] },
          { kb: "beta", global: false, linked_kbs: ["alpha"] },
          { kb: "gamma", global: true, linked_kbs: [] },
        ]}
        onPivotKb={vi.fn()}
      />,
    );
    const rows = screen.getAllByTestId("scope-overlap-row");
    expect(rows).toHaveLength(3);
    // Global row sorts first regardless of count.
    expect(rows[0].textContent).toContain("★ global");
    expect(rows.some((r) => r.textContent?.includes("alpha"))).toBe(true);
    expect(rows.some((r) => r.textContent?.includes("alpha + beta"))).toBe(true);
  });

  it("clicking a row pivots to its first member kb", () => {
    const onPivotKb = vi.fn();
    render(
      <ScopeOverlapUpset
        hits={[{ kb: "beta", global: false, linked_kbs: ["alpha"] }]}
        onPivotKb={onPivotKb}
      />,
    );
    fireEvent.click(screen.getByTestId("scope-overlap-row"));
    expect(onPivotKb).toHaveBeenCalledWith("alpha");
  });

  it("clicking the global row pivots to one of its home kbs", () => {
    const onPivotKb = vi.fn();
    render(
      <ScopeOverlapUpset
        hits={[{ kb: "zeta", global: true, linked_kbs: [] }]}
        onPivotKb={onPivotKb}
      />,
    );
    fireEvent.click(screen.getByTestId("scope-overlap-row"));
    expect(onPivotKb).toHaveBeenCalledWith("zeta");
  });

  it("dot matrix marks membership for a combo row", () => {
    render(
      <ScopeOverlapUpset
        hits={[
          { kb: "alpha", global: false, linked_kbs: ["beta"] },
          { kb: "gamma", global: false, linked_kbs: [] },
        ]}
        onPivotKb={vi.fn()}
      />,
    );
    const rows = screen.getAllByTestId("scope-overlap-row");
    const comboRow = rows.find((r) => r.textContent?.includes("alpha + beta"));
    expect(comboRow).toBeTruthy();
    const dots = comboRow!.querySelectorAll(".kb-scopeup__dot");
    // Columns are alpha, beta, gamma (sorted) — alpha+beta row has the
    // first two dots on, the third (gamma) off.
    expect(dots).toHaveLength(3);
    expect(dots[0].className).toContain("is-on");
    expect(dots[1].className).toContain("is-on");
    expect(dots[2].className).not.toContain("is-on");
  });
});
