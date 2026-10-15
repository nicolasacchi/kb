// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import Tabs, { type TabSpec } from "./Tabs";

const tabs: TabSpec[] = [
  { id: "overview", label: "Overview", group: "This daemon", body: () => <p>ov</p> },
  { id: "errors", label: "Errors", group: "This daemon", body: () => <p>er</p> },
  { id: "pipeline", label: "Pipeline", group: "Pipeline", body: () => <p>pi</p> },
  { id: "census", label: "Census", group: "You", body: () => <p>ce</p> },
];

beforeEach(() => {
  localStorage.clear();
  window.history.replaceState(null, "", "/settings");
});
afterEach(cleanup);

describe("settings Tabs grouping", () => {
  it("renders one heading per group, in order, inside the tablist", () => {
    const { container } = render(<Tabs tabs={tabs} label="sections" />);
    const heads = [...container.querySelectorAll(".settings__group")].map(
      (n) => n.textContent,
    );
    expect(heads).toEqual(["This daemon", "Pipeline", "You"]);
    expect(screen.getAllByRole("tab")).toHaveLength(4);
  });

  it("offers a grouped select mirroring the tabs for phones", () => {
    const { container } = render(<Tabs tabs={tabs} label="sections" />);
    const labels = [...container.querySelectorAll("select optgroup")].map((n) =>
      n.getAttribute("label"),
    );
    expect(labels).toEqual(["This daemon", "Pipeline", "You"]);
    expect(container.querySelectorAll("select option")).toHaveLength(4);
  });

  it("keeps hash, localStorage and replaceState behaviour (select + click)", () => {
    const { container } = render(<Tabs tabs={tabs} label="sections" />);
    fireEvent.change(container.querySelector("select")!, { target: { value: "census" } });
    expect(window.location.hash).toBe("#census");
    expect(localStorage.getItem("kb:settings:tab")).toBe("census");
    expect(screen.getByRole("tab", { name: "Census" }).getAttribute("aria-selected")).toBe("true");
    fireEvent.click(screen.getByRole("tab", { name: "Pipeline" }));
    expect(window.location.hash).toBe("#pipeline");
  });

  it("a hash deep link lands on its tab", () => {
    window.history.replaceState(null, "", "/settings#pipeline");
    render(<Tabs tabs={tabs} label="sections" />);
    expect(screen.getByRole("tab", { name: "Pipeline" }).getAttribute("aria-selected")).toBe("true");
  });

  it("ArrowDown moves to the next tab across a group boundary", () => {
    render(<Tabs tabs={tabs} label="sections" />);
    fireEvent.keyDown(screen.getByRole("tab", { name: "Errors" }), { key: "ArrowDown" });
    fireEvent.click(screen.getByRole("tab", { name: "Errors" }));
    fireEvent.keyDown(screen.getByRole("tab", { name: "Errors" }), { key: "ArrowDown" });
    expect(screen.getByRole("tab", { name: "Pipeline" }).getAttribute("aria-selected")).toBe("true");
  });
});
