import { describe, it, expect } from "vitest";
import { isOtherUser, authorAttributionLabel } from "./userAttribution";

describe("isOtherUser", () => {
  it("false when user absent/empty", () => {
    expect(isOtherUser(undefined, "alice")).toBe(false);
    expect(isOtherUser(null, "alice")).toBe(false);
    expect(isOtherUser("", "alice")).toBe(false);
    expect(isOtherUser("  ", "alice")).toBe(false);
  });

  it("false when user === me", () => {
    expect(isOtherUser("alice", "alice")).toBe(false);
  });

  it("true when user differs from me", () => {
    expect(isOtherUser("bob", "alice")).toBe(true);
  });

  it("false when me is unresolved — no chip flash before /api/identity lands", () => {
    expect(isOtherUser("bob", null)).toBe(false);
    expect(isOtherUser("bob", undefined)).toBe(false);
  });
});

describe("authorAttributionLabel", () => {
  it("own comments return the role only", () => {
    expect(authorAttributionLabel("you", "alice", "alice")).toBe("you");
    expect(authorAttributionLabel("claude", "alice", "alice")).toBe("claude");
    expect(authorAttributionLabel("you", undefined, "alice")).toBe("you");
  });

  it("beside mode (default) keeps the role for teammates", () => {
    expect(authorAttributionLabel("you", "bob", "alice")).toBe("you");
    expect(authorAttributionLabel("claude", "bob", "alice")).toBe("claude");
  });

  it("combined mode: teammate human → name; agent → claude · name", () => {
    expect(
      authorAttributionLabel("you", "bob", "alice", { combined: true }),
    ).toBe("bob");
    expect(
      authorAttributionLabel("claude", "bob", "alice", { combined: true }),
    ).toBe("claude · bob");
  });
});
