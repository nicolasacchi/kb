// @vitest-environment jsdom
// CONVRAIL req 3: the shell itself imports the registration site (no mock), so
// deleting that import fails here rather than only in Playwright.
import { describe, expect, it } from "vitest";
import { TermShell } from "./TermShell";
import { railTabs } from "./rail/registry";

describe("TermShell registration", () => {
  it("importing the shell registers the real Conversation tab", () => {
    expect(typeof TermShell).toBe("function");
    expect(railTabs().some((t) => t.id === "conversation")).toBe(true);
  });
});
