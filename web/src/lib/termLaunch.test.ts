import { describe, expect, it } from "vitest";
import {
  BOUND_DEADLINE_MS,
  BOUND_POLL_MS,
  CWD_MAX_BYTES,
  LAUNCH_HARNESSES,
  cwdSuggestions,
  isLaunchHarness,
  launchBody,
  pickLaunchHarness,
  validateCwd,
} from "./termLaunch";

describe("launch harness set", () => {
  it("is the closed five, with grok and opencode experimental", () => {
    expect(LAUNCH_HARNESSES.map((h) => h.id)).toEqual(["claude", "codex", "omp", "opencode", "grok"]);
    expect(LAUNCH_HARNESSES.filter((h) => h.experimental).map((h) => h.id)).toEqual([
      "opencode",
      "grok",
    ]);
  });
  it("refuses anything outside the set and degrades a stale choice to claude", () => {
    expect(isLaunchHarness("kimi")).toBe(false);
    expect(isLaunchHarness("")).toBe(false);
    expect(isLaunchHarness("codex")).toBe(true);
    expect(pickLaunchHarness("kimi")).toBe("claude");
    expect(pickLaunchHarness(null)).toBe("claude");
    expect(pickLaunchHarness("omp")).toBe("omp");
  });
});

describe("validateCwd (mirrors the server's free_text + absolute rule)", () => {
  it("accepts an absolute path", () => {
    expect(validateCwd("/srv/project")).toBeNull();
  });
  it("refuses empty, relative, control-character and oversize paths", () => {
    expect(validateCwd("")).not.toBeNull();
    expect(validateCwd("project/sub")).not.toBeNull();
    expect(validateCwd("~/project")).not.toBeNull();
    expect(validateCwd("/a\nb")).not.toBeNull();
    expect(validateCwd("/a\u0000b")).not.toBeNull();
    expect(validateCwd("/a\u007fb")).not.toBeNull();
  });
  it("counts BYTES, not characters, against the 512 limit", () => {
    expect(validateCwd("/" + "a".repeat(CWD_MAX_BYTES - 1))).toBeNull();
    expect(validateCwd("/" + "a".repeat(CWD_MAX_BYTES))).not.toBeNull();
    // 200 two-byte characters = 400 bytes + "/" fits; 260 of them is 520 bytes.
    expect(validateCwd("/" + "é".repeat(200))).toBeNull();
    expect(validateCwd("/" + "é".repeat(260))).not.toBeNull();
  });
});

describe("launchBody", () => {
  it("New carries no resume_sid; Resume carries exactly the sid", () => {
    expect(launchBody("claude", "/w")).toEqual({ harness: "claude", cwd: "/w" });
    expect("resume_sid" in launchBody("claude", "/w")).toBe(false);
    expect(launchBody("omp", "/w", "abc")).toEqual({ harness: "omp", cwd: "/w", resume_sid: "abc" });
  });
});

describe("cwdSuggestions", () => {
  it("puts recent first, drops invalid and duplicates, and caps", () => {
    expect(cwdSuggestions(["/b", "/a"], ["/a", "rel", null, undefined, "/c", "/b"])).toEqual([
      "/b",
      "/a",
      "/c",
    ]);
    expect(cwdSuggestions([], ["/1", "/2", "/3"], 2)).toEqual(["/1", "/2"]);
  });
});

describe("binding poll bounds (docs/kb-term.md says 2 s and 2 min)", () => {
  it("are 2 seconds and 2 minutes", () => {
    expect(BOUND_POLL_MS).toBe(2_000);
    expect(BOUND_DEADLINE_MS).toBe(120_000);
  });
});
