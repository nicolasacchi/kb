// @vitest-environment jsdom
//
// OL R2 — Topics is a collapsible section, collapsed by default, header
// "Topics (N)", open state persisted in localStorage.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@testing-library/react";
import TopicsSection, { TOPICS_OPEN_KEY } from "./TopicsSection";

const items = [
  { id: "a", text: "Alpha", level: 1 },
  { id: "b", text: "Beta", level: 2 },
];

beforeEach(() => localStorage.clear());
afterEach(cleanup);

describe("TopicsSection", () => {
  it("renders nothing without headings", () => {
    const { container } = render(
      <TopicsSection items={[]} activeId={null} onJump={() => {}} />,
    );
    expect(container.innerHTML).toBe("");
  });

  it("is collapsed by default with a counted header", () => {
    const { container, getByText } = render(
      <TopicsSection items={items} activeId={null} onJump={() => {}} />,
    );
    expect(getByText("Topics (2)")).toBeTruthy();
    expect(container.querySelector(".kb-pinsp__toc")).toBeNull();
    expect(
      container.querySelector("[data-kb-act=topics-toggle]")!.getAttribute("aria-expanded"),
    ).toBe("false");
  });

  it("opens on click, persists, and jumps", () => {
    const onJump = vi.fn();
    const { container } = render(
      <TopicsSection items={items} activeId="b" onJump={onJump} />,
    );
    fireEvent.click(container.querySelector("[data-kb-act=topics-toggle]")!);
    expect(localStorage.getItem(TOPICS_OPEN_KEY)).toBe("1");
    const rows = container.querySelectorAll(".kb-pinsp__toc-row");
    expect(rows).toHaveLength(2);
    expect(rows[1].className).toContain("is-on");
    fireEvent.click(rows[0]);
    expect(onJump).toHaveBeenCalledWith("a");
  });

  it("starts open when the persisted state says so", () => {
    localStorage.setItem(TOPICS_OPEN_KEY, "1");
    const { container } = render(
      <TopicsSection items={items} activeId={null} onJump={() => {}} />,
    );
    expect(container.querySelector(".kb-pinsp__toc")).not.toBeNull();
  });
});
