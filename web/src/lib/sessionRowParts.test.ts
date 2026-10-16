import { describe, expect, it } from "vitest";
import { PROMPT_TITLE_MAX, sessionMetaParts, sessionRowTitle } from "./sessionRowParts";
import type { SessionRow } from "../api/sessions";

function row(over: Partial<SessionRow>): SessionRow {
  return {
    session_id: "abcdef0123456789",
    display_name: "",
    duration_ms: 0,
    memory_count: 0,
    commit_count: 0,
    error_count: 0,
    folder: "",
    harness: "claude",
    ...over,
  } as SessionRow;
}

describe("sessionRowTitle", () => {
  it("keeps a real name untouched and not flagged", () => {
    const t = sessionRowTitle(row({ display_name: "Evolve sessions", first_user_prompt: "do the thing" }));
    expect(t).toEqual({ text: "Evolve sessions", isPromptFallback: false });
  });
  it("flags and truncates the first-prompt fallback at 80 chars", () => {
    const prompt = "x".repeat(200);
    const t = sessionRowTitle(row({ first_user_prompt: prompt }));
    expect(t.isPromptFallback).toBe(true);
    expect(t.text.length).toBe(PROMPT_TITLE_MAX);
    expect(t.text.endsWith("…")).toBe(true);
  });
  it("does not truncate a short fallback prompt", () => {
    const t = sessionRowTitle(row({ first_user_prompt: "fix\nthe  build" }));
    expect(t).toEqual({ text: "fix the build", isPromptFallback: true });
  });
});

describe("sessionMetaParts", () => {
  it("joins project, harness, duration, memories", () => {
    const p = sessionMetaParts(row({ folder: "widget", harness: "codex", duration_ms: 3_600_000, memory_count: 2 }));
    expect(p[0]).toBe("widget");
    expect(p[1]).toBe("codex");
    expect(p).toContain("2 memories");
  });
  it("omits absent facts and singularises", () => {
    expect(sessionMetaParts(row({ memory_count: 1 }))).toEqual(["claude", "1 memory"]);
  });
});
