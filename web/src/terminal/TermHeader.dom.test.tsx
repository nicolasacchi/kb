// @vitest-environment jsdom
import { cleanup, render } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { TermHeader } from "./TermHeader";
import { statusChips } from "./statusChips";

afterEach(cleanup);

const q = (c: HTMLElement, id: string): HTMLElement | null => c.querySelector(`[data-testid="${id}"]`);

describe("TermHeader (desktop)", () => {
  it("renders a dot and the phase word with data-phase for every phase", () => {
    for (const phase of ["connecting", "live", "reconnecting", "ended"]) {
      const { container, unmount } = render(<TermHeader compact={false} harness="omp" phase={phase} chips={[]} />);
      const state = q(container, "kb-term-state")!;
      expect(state.dataset.phase).toBe(phase);
      expect(state.textContent).toBe(phase);
      expect(state.querySelector(".kb-term__dot")).not.toBeNull();
      unmount();
    }
  });

  it("renders no chips element when there are none, and plain-text chips with titles otherwise", () => {
    const none = render(<TermHeader compact={false} harness="omp" phase="live" chips={[]} />);
    expect(q(none.container, "kb-term-status-chips")).toBeNull();
    none.unmount();
    const chips = statusChips({ model: "<b>m</b>", branch: "main" });
    const { container } = render(<TermHeader compact={false} harness="omp" phase="live" chips={chips} />);
    const list = q(container, "kb-term-status-chips")!;
    expect(list.tagName).toBe("UL");
    expect(list.querySelectorAll("li")).toHaveLength(2);
    expect(list.querySelector("b")).toBeNull();
    expect(list.querySelector("li")!.title).toContain("harness hook");
  });

  it("puts the actions slot before the pane verbs and keeps the verbs inside the bar", () => {
    const { container } = render(
      <TermHeader compact={false} harness="omp" project="demo" phase="live" chips={[]} actions={<button data-testid="a">a</button>}>
        <button data-testid="kb-term-close">Close</button>
      </TermHeader>,
    );
    const actions = q(container, "kb-term-actions")!;
    const close = q(container, "kb-term-close")!;
    expect(actions.compareDocumentPosition(close) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(q(container, "kb-term-bar")!.contains(close)).toBe(true);
    expect(q(container, "kb-term-project")!.textContent).toBe("demo");
  });

  it("keeps the OSC title as its own text-only element, absent when empty", () => {
    const a = render(<TermHeader compact={false} harness="omp" phase="live" chips={[]} />);
    expect(q(a.container, "kb-term-title")).toBeNull();
    a.unmount();
    const b = render(<TermHeader compact={false} harness="omp" phase="live" chips={[]} progTitle="build" />);
    expect(q(b.container, "kb-term-title")!.textContent).toBe("build");
  });
});

describe("TermHeader (compact / phone)", () => {
  it("keeps the bar and extra-row structure: actions before the host's controls, one End", () => {
    const { container } = render(
      <TermHeader
        compact
        harness="claude"
        project="demo"
        phase="live"
        chips={[]}
        actions={<button data-testid="act">palette</button>}
        extra={<button data-testid="x-end">End</button>}
      >
        <button data-testid="kb-term-close">Close</button>
      </TermHeader>,
    );
    const extra = q(container, "kb-term-phone-extra")!;
    expect(extra.previousElementSibling!.className).toBe("kb-term-phone__bar");
    const order = Array.from(extra.querySelectorAll("button")).map((b) => b.dataset.testid);
    expect(order).toEqual(["act", "x-end"]);
    expect(container.querySelectorAll('[data-testid="x-end"]')).toHaveLength(1);
    expect(q(container, "kb-term-phone-title")!.textContent).toBe("claude");
    expect(q(container, "kb-term-state")!.dataset.phase).toBe("live");
  });

  it("renders no extra row without actions or extra", () => {
    const { container } = render(<TermHeader compact harness="claude" phase="live" chips={[]} />);
    expect(q(container, "kb-term-phone-extra")).toBeNull();
  });
});
