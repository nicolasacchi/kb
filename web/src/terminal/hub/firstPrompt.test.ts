import { describe, expect, it } from "vitest";
import { FIRST_PROMPT_MAX, firstPromptIssue, firstPromptPresent, firstPromptText } from "./firstPrompt";

describe("firstPromptIssue", () => {
  it("refuses an interior LF, CRLF or lone CR with the Enter caption", () => {
    for (const t of ["a\nb", "a\r\nb", "a\rb", "a\n\nb"]) {
      expect(firstPromptIssue(t)).toMatch(/line break would press Enter/);
    }
  });
  it("accepts one trailing newline and strips it from the pasted text", () => {
    expect(firstPromptIssue("echo hi\n")).toBeNull();
    expect(firstPromptIssue("echo hi\r\n")).toBeNull();
    expect(firstPromptText("echo hi\n")).toBe("echo hi");
    expect(firstPromptIssue("a\n\n")).not.toBeNull();
  });
  it("refuses control characters but allows a tab", () => {
    expect(firstPromptIssue("a\u001bb")).toMatch(/Control/);
    expect(firstPromptIssue("a\u0000b")).toMatch(/Control/);
    expect(firstPromptIssue("a\u007fb")).toMatch(/Control/);
    expect(firstPromptIssue("a\tb")).toBeNull();
  });
  it("refuses over the maximum", () => {
    expect(firstPromptIssue("x".repeat(FIRST_PROMPT_MAX))).toBeNull();
    expect(firstPromptIssue("x".repeat(FIRST_PROMPT_MAX + 1))).toMatch(/Too long/);
  });
  it("blank is no prompt, not an issue", () => {
    for (const t of ["", "   ", "\n"]) {
      expect(firstPromptIssue(t)).toBeNull();
      expect(firstPromptPresent(t)).toBe(false);
    }
    expect(firstPromptPresent("x")).toBe(true);
  });
});
