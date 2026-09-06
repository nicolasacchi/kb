// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import DecaySparkline from "./DecaySparkline";

afterEach(() => cleanup());

describe("DecaySparkline", () => {
  it("renders the empty placeholder when required data is missing", () => {
    render(
      <DecaySparkline
        salience={null}
        ageDays={null}
        decayK={null}
        floor={0.15}
        pinned={false}
      />,
    );
    expect(screen.getByTestId("decay-sparkline-empty")).toBeTruthy();
    expect(screen.queryByTestId("decay-sparkline")).toBeNull();
  });

  it("a below-floor-now memory gets the danger treatment and an honest 'excluded now' label — never a predicted date", () => {
    render(
      <DecaySparkline
        salience={0.05}
        ageDays={0}
        decayK={0.01}
        floor={0.15}
        pinned={false}
      />,
    );
    const el = screen.getByTestId("decay-sparkline");
    expect(el.className).toContain("kb-decayspark--below-floor");
    const label = screen.getByTestId("decay-sparkline-label");
    expect(label.textContent).toBe(
      "salience 0.05 — below the 0.15 floor, excluded from recall now",
    );
    expect(screen.getByTestId("decay-sparkline-floor")).toBeTruthy();
    expect(screen.getByTestId("decay-sparkline-base")).toBeTruthy();
    // No crossing marker exists any more — the whole concept was removed.
    expect(screen.queryByTestId("decay-sparkline-crossing")).toBeNull();
  });

  it("an above-floor memory shows the TRUE half-life label, not a drop prediction", () => {
    render(
      <DecaySparkline
        salience={0.3}
        ageDays={5}
        decayK={0.1}
        floor={0.15}
        pinned={false}
      />,
    );
    const el = screen.getByTestId("decay-sparkline");
    expect(el.className).not.toContain("kb-decayspark--below-floor");
    const label = screen.getByTestId("decay-sparkline-label");
    expect(label.textContent).toBe("score halves every ~7d");
  });

  it("a high-salience memory NEVER reports an exclusion, no matter how old — the ground truth this revision exists to fix", () => {
    render(
      <DecaySparkline
        salience={0.95}
        // An absurdly large age — under the OLD (wrong) model this would
        // have projected a "drops in Nd" claim; under the fix, age is
        // irrelevant to the floor question entirely.
        ageDays={100_000}
        decayK={0.01}
        floor={0.15}
        pinned={false}
      />,
    );
    const el = screen.getByTestId("decay-sparkline");
    expect(el.className).not.toContain("kb-decayspark--below-floor");
    const label = screen.getByTestId("decay-sparkline-label");
    expect(label.textContent).toBe("score halves every ~69d");
    expect(label.textContent).not.toMatch(/drop/i);
  });

  it("renders pinned memories distinctly — exempt, no floor line, never below-floor styling", () => {
    render(
      <DecaySparkline
        salience={0.02}
        ageDays={500}
        decayK={0.1}
        floor={0.15}
        pinned
      />,
    );
    const el = screen.getByTestId("decay-sparkline");
    expect(el.className).toContain("kb-decayspark--pinned");
    expect(el.className).not.toContain("kb-decayspark--below-floor");
    expect(screen.getByTestId("decay-sparkline-label").textContent).toBe(
      "pinned — exempt from floor",
    );
    expect(screen.queryByTestId("decay-sparkline-floor")).toBeNull();
  });

  it("a Loose policy (floor=null) reports no-floor, never a below-floor state", () => {
    render(
      <DecaySparkline
        salience={0.01}
        ageDays={9999}
        decayK={0.1}
        floor={null}
        pinned={false}
      />,
    );
    const el = screen.getByTestId("decay-sparkline");
    expect(el.className).not.toContain("kb-decayspark--below-floor");
    expect(screen.queryByTestId("decay-sparkline-floor")).toBeNull();
  });

  it("renders a SECOND stability-adjusted curve only when stability is provided", () => {
    const { rerender } = render(
      <DecaySparkline salience={0.5} ageDays={5} decayK={0.05} floor={0.15} pinned={false} />,
    );
    expect(screen.queryByTestId("decay-sparkline-stable")).toBeNull();

    rerender(
      <DecaySparkline
        salience={0.5}
        ageDays={5}
        decayK={0.05}
        stability={1.8}
        floor={0.15}
        pinned={false}
      />,
    );
    expect(screen.getByTestId("decay-sparkline-stable")).toBeTruthy();
  });

  it("the tooltip carries BOTH the floor state and the half-life, even when the label only shows one", () => {
    render(
      <DecaySparkline salience={0.4} ageDays={2} decayK={0.01} floor={0.15} pinned={false} />,
    );
    const el = screen.getByTestId("decay-sparkline");
    expect(el.title).toBe("salience 0.40 — above the 0.15 floor — score halves every ~69d");
  });
});
