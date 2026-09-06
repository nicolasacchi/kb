// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import RecallQuadrantScatter from "./RecallQuadrantScatter";
import type { QuadrantInput } from "../lib/recallQuadrant";

const NOW = 1_700_000_000;
const DAY = 86_400;

function row(overrides: Partial<QuadrantInput> = {}): QuadrantInput {
  return {
    kb: "notes",
    id: "aaaaaaaaaaaa",
    title: "A Fact",
    sourceRelative: "a.html",
    salience: 0.5,
    recallCount: 0,
    lastRecalledAt: null,
    ...overrides,
  };
}

// FIX2 — the wire-supplied constants (kb_core::triage::
// HIGH_SALIENCE_THRESHOLD/DORMANT_DAYS), now required props rather than
// module-level defaults.
function renderScatter(rows: QuadrantInput[]) {
  return render(
    <MemoryRouter>
      <RecallQuadrantScatter
        rows={rows}
        nowUnix={NOW}
        highSalienceThreshold={0.7}
        dormantDays={60}
      />
    </MemoryRouter>,
  );
}

afterEach(() => cleanup());

describe("RecallQuadrantScatter", () => {
  it("renders an empty state for zero rows", () => {
    renderScatter([]);
    expect(screen.getByText("no memories to plot.")).toBeTruthy();
    expect(screen.queryByTestId("recall-quadrant-point")).toBeNull();
  });

  it("plots one point per row", () => {
    renderScatter([row({ id: "a" }), row({ id: "b" })]);
    expect(screen.getAllByTestId("recall-quadrant-point")).toHaveLength(2);
  });

  it("tags each point with its quadrant classification", () => {
    renderScatter([
      row({ id: "dead", salience: 0.9, lastRecalledAt: null }),
      row({ id: "healthy", salience: 0.9, lastRecalledAt: NOW - DAY }),
    ]);
    const points = screen.getAllByTestId("recall-quadrant-point");
    const dead = points.find((p) => p.getAttribute("data-quadrant") === "dead-weight");
    const active = points.find((p) => p.getAttribute("data-quadrant") === "healthy-active");
    expect(dead).toBeTruthy();
    expect(active).toBeTruthy();
  });

  it("clicking a point focuses it and shows a link to the artifact", () => {
    renderScatter([row({ id: "aaaaaaaaaaaa", title: "Focus Target", sourceRelative: "ft.html" })]);
    expect(screen.queryByTestId("recall-quadrant-focus")).toBeNull();

    fireEvent.click(screen.getByTestId("recall-quadrant-point"));

    const focus = screen.getByTestId("recall-quadrant-focus");
    expect(focus.textContent).toContain("Focus Target");
    const link = focus.querySelector("a");
    expect(link?.getAttribute("href")).toContain("ft.html");
  });

  // === FIX4 — a11y: keyboard + screen-reader operability =================

  it("each point is a keyboard-focusable button with an accessible name identifying the memory", () => {
    renderScatter([row({ id: "aaaaaaaaaaaa", title: "Focus Target", salience: 0.42 })]);
    const point = screen.getByTestId("recall-quadrant-point");
    expect(point.getAttribute("role")).toBe("button");
    expect(point.getAttribute("tabindex")).toBe("0");
    expect(point.getAttribute("aria-label")).toContain("Focus Target");
    expect(point.getAttribute("aria-label")).toContain("0.42");
  });

  it("pressing Enter on a focused point activates it, same as a click", () => {
    renderScatter([row({ id: "aaaaaaaaaaaa", title: "Keyboard Target", sourceRelative: "kt.html" })]);
    expect(screen.queryByTestId("recall-quadrant-focus")).toBeNull();

    const point = screen.getByTestId("recall-quadrant-point");
    point.focus();
    fireEvent.keyDown(point, { key: "Enter" });

    const focus = screen.getByTestId("recall-quadrant-focus");
    expect(focus.textContent).toContain("Keyboard Target");
  });

  it("pressing Space on a focused point also activates it", () => {
    renderScatter([row({ id: "aaaaaaaaaaaa", title: "Space Target" })]);
    const point = screen.getByTestId("recall-quadrant-point");
    point.focus();
    fireEvent.keyDown(point, { key: " " });

    expect(screen.getByTestId("recall-quadrant-focus").textContent).toContain("Space Target");
  });

  it("an unrelated key does not activate the point", () => {
    renderScatter([row({ id: "aaaaaaaaaaaa", title: "Untouched" })]);
    const point = screen.getByTestId("recall-quadrant-point");
    point.focus();
    fireEvent.keyDown(point, { key: "a" });

    expect(screen.queryByTestId("recall-quadrant-focus")).toBeNull();
  });
});
