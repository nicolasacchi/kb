import { describe, expect, it } from "vitest";
import { isValidElement } from "react";
import "../registerTabs";
import { railTabs } from "../registry";
import { KnowledgeRail } from "./KnowledgeRail";

describe("Knowledge tab registration", () => {
  it("registerTabs puts the real KnowledgeRail in the knowledge slot, not the placeholder", () => {
    const tab = railTabs().find((t) => t.id === "knowledge");
    expect(tab?.label).toBe("Knowledge");
    expect(tab?.badge).toBeUndefined();
    expect(isValidElement(tab?.node) && tab.node.type === KnowledgeRail).toBe(true);
  });
});
