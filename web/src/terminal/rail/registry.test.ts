import { afterEach, describe, expect, it } from "vitest";
import { createElement } from "react";
import { RAIL_TAB_ORDER, railTabs, registerRailTab, resetRailTabs, type RailTab } from "./registry";

const IDS = ["now", "conversation", "changes", "files", "knowledge", "board", "commands"];

afterEach(() => resetRailTabs());

describe("rail registry", () => {
  it("ships the seven placeholder tabs in the fixed order", () => {
    expect(railTabs().map((t) => t.id)).toEqual(IDS);
    expect(railTabs().map((t) => t.label)).toEqual([
      "Now",
      "Conversation",
      "Changes",
      "Files",
      "Knowledge",
      "Board",
      "Commands",
    ]);
    expect([...RAIL_TAB_ORDER]).toEqual(IDS);
  });

  it("keeps the fixed order whatever the registration order", () => {
    registerRailTab({ id: "commands", label: "Cmds", node: null });
    registerRailTab({ id: "conversation", label: "Talk", node: null });
    expect(railTabs().map((t) => t.id)).toEqual(IDS);
    expect(railTabs()[1]?.label).toBe("Talk");
  });

  it("a duplicate id replaces the earlier tab", () => {
    registerRailTab({ id: "files", label: "One", node: null });
    registerRailTab({ id: "files", label: "Two", badge: 3, node: createElement("i") });
    const files = railTabs().filter((t) => t.id === "files");
    expect(files).toHaveLength(1);
    expect(files[0]?.label).toBe("Two");
    expect(files[0]?.badge).toBe(3);
  });

  it("every new id accepts a registration", () => {
    for (const id of ["now", "changes", "knowledge", "board"] as const) {
      registerRailTab({ id, label: id, node: null });
    }
    expect(railTabs().map((t) => t.label)).toEqual(["now", "Conversation", "changes", "Files", "knowledge", "board", "Commands"]);
  });

  it("an unknown id throws", () => {
    expect(() => registerRailTab({ id: "nope", label: "x", node: null } as unknown as RailTab)).toThrow(/unknown rail tab/);
  });
});
