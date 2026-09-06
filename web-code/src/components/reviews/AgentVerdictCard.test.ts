import { describe, expect, it } from "vitest";
import { severityToken, severityWord } from "./AgentVerdictCard";

describe("severityWord", () => {
  it("capitalizes each server severity verbatim", () => {
    expect(severityWord("blocker")).toBe("Blocker");
    expect(severityWord("concern")).toBe("Concern");
    expect(severityWord("ok")).toBe("OK");
  });

  it("is honest about an unset severity rather than guessing", () => {
    expect(severityWord(undefined)).toBe("Unset");
  });
});

describe("severityToken", () => {
  it("maps blocker to red, concern to warn, ok to green", () => {
    expect(severityToken("blocker")).toBe("var(--red)");
    expect(severityToken("concern")).toBe("var(--warn)");
    expect(severityToken("ok")).toBe("var(--green)");
  });

  it("falls back to a neutral ink token when unset", () => {
    expect(severityToken(undefined)).toBe("var(--ink-dim)");
  });
});
