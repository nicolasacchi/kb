import { describe, it, expect } from "vitest";
import { sessionDisplayName } from "./sessionDisplayName";
import type { SessionRow } from "../api/sessions";

function row(over: Partial<SessionRow>): SessionRow {
  return {
    id: "s",
    kb: "kb",
    artifact_id: "a",
    session_id: "abcdef0123456789",
    started_at: 0,
    ended_at: 0,
    duration_ms: 0,
    message_count: 0,
    memory_count: 0,
    source_relative: "x.html",
    display_name: "",
    files_read_count: 0,
    files_edited_count: 0,
    ...over,
  } as SessionRow;
}

describe("sessionDisplayName", () => {
  it("prefers the server display_name", () => {
    expect(
      sessionDisplayName(row({ display_name: "Evolve sessions", title: "T" })),
    ).toBe("Evolve sessions");
  });

  it("falls back to title then first prompt then short id", () => {
    expect(sessionDisplayName(row({ display_name: "", title: "The Title" }))).toBe(
      "The Title",
    );
    expect(
      sessionDisplayName(
        row({ display_name: "", title: undefined, first_user_prompt: "do it" }),
      ),
    ).toBe("do it");
    expect(
      sessionDisplayName(
        row({ display_name: "   ", title: undefined, first_user_prompt: undefined }),
      ),
    ).toBe("session abcdef01");
  });

  // W3.C/R4 — husk display_name override: a trivial session with no real
  // user prompt reads as "(empty session)" regardless of what title/
  // display_name the server computed (a generic "Session transcript …"
  // fallback), so a dimmed gallery card / list row never promises content
  // that doesn't exist.
  it("overrides a trivial husk's title/display_name with a deterministic label", () => {
    expect(
      sessionDisplayName(
        row({
          substance: "trivial",
          display_name: "Session transcript 2026-07-29",
          title: "Session transcript 2026-07-29",
          first_user_prompt: undefined,
        }),
      ),
    ).toBe("(empty session)");
    expect(
      sessionDisplayName(row({ substance: "trivial", first_user_prompt: "   " })),
    ).toBe("(empty session)");
  });

  it("does not override a trivial session that DOES have a real prompt", () => {
    expect(
      sessionDisplayName(
        row({ substance: "trivial", first_user_prompt: "quick check" }),
      ),
    ).toBe("quick check");
  });

  it("does not override a non-trivial (or unbackfilled) session", () => {
    expect(
      sessionDisplayName(
        row({ substance: "substantive", display_name: "Real work" }),
      ),
    ).toBe("Real work");
    expect(
      sessionDisplayName(row({ substance: undefined, display_name: "Unbackfilled" })),
    ).toBe("Unbackfilled");
  });
});
