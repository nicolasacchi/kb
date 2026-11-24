import { afterEach, describe, expect, it } from "vitest";
import { createElement } from "react";
import { railTabs, registerRailTab, resetRailTabs, type RailTab } from "./registry";

afterEach(() => resetRailTabs());

describe("rail registry", () => {
  it("ships the three placeholder tabs in the fixed order", () => {
    expect(railTabs().map((t) => t.id)).toEqual(["conversation", "files", "commands"]);
    expect(railTabs().map((t) => t.label)).toEqual(["Conversation", "Files", "Commands"]);
  });

  it("keeps the fixed order whatever the registration order", () => {
    registerRailTab({ id: "commands", label: "Cmds", node: null });
    registerRailTab({ id: "conversation", label: "Talk", node: null });
    expect(railTabs().map((t) => t.id)).toEqual(["conversation", "files", "commands"]);
    expect(railTabs()[0]?.label).toBe("Talk");
  });

  it("a duplicate id replaces the earlier tab", () => {
    registerRailTab({ id: "files", label: "One", node: null });
    registerRailTab({ id: "files", label: "Two", badge: 3, node: createElement("i") });
    const files = railTabs().filter((t) => t.id === "files");
    expect(files).toHaveLength(1);
    expect(files[0]?.label).toBe("Two");
    expect(files[0]?.badge).toBe(3);
  });

  it("an unknown id throws", () => {
    expect(() => registerRailTab({ id: "nope", label: "x", node: null } as unknown as RailTab)).toThrow(/unknown rail tab/);
  });
});
