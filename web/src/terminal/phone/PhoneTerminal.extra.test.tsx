// @vitest-environment jsdom
//
// FUTERMLIST — the phone sheet's title area has a slot for the host's End
// button and terminal switcher (its own row: the title bar keeps its width).
import { describe, expect, it } from "vitest";
import { cleanup, render } from "@testing-library/react";
import { afterEach } from "vitest";
import { PhoneTerminal } from "./PhoneTerminal";

const base = {
  term: null,
  profile: null,
  hostRef: { current: null },
  mode: "watch" as const,
  onMode: () => {},
  stream: { phase: "live", applied: 0, resets: 0, gaps: 0 },
  caption: "live",
};

afterEach(cleanup);

describe("PhoneTerminal headerExtra", () => {
  it("renders the host's controls in their own row, between the title bar and the caption row", () => {
    const { container } = render(<PhoneTerminal {...base} headerExtra={<button data-testid="x-end">End</button>} />);
    const extra = container.querySelector('[data-testid="kb-term-phone-extra"]');
    expect(extra?.querySelector('[data-testid="x-end"]')).toBeTruthy();
    expect(extra?.previousElementSibling?.className).toBe("kb-term-phone__bar");
    expect(extra?.nextElementSibling?.className).toBe("kb-term-phone__sub");
    expect(container.querySelector(".kb-term-phone__bar [data-testid='x-end']")).toBeNull();
  });
  it("renders no extra row without controls", () => {
    const { container } = render(<PhoneTerminal {...base} />);
    expect(container.querySelector('[data-testid="kb-term-phone-extra"]')).toBeNull();
  });
});
