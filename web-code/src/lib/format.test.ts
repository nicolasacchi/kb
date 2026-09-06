import { describe, expect, it } from "vitest";
import { relativeTime, shortSha } from "./format";

const NOW = 1_700_100_000_000; // a fixed "now" in millis for deterministic tests

describe("relativeTime", () => {
  it("renders a moment ago as 'just now'", () => {
    expect(relativeTime(NOW / 1000, NOW)).toBe("just now");
    expect(relativeTime(NOW / 1000 - 4, NOW)).toBe("just now");
  });

  it("renders seconds", () => {
    expect(relativeTime(NOW / 1000 - 30, NOW)).toBe("30 seconds ago");
  });

  it("renders singular units without an 's'", () => {
    expect(relativeTime(NOW / 1000 - 60, NOW)).toBe("1 minute ago");
    expect(relativeTime(NOW / 1000 - 3600, NOW)).toBe("1 hour ago");
    expect(relativeTime(NOW / 1000 - 86400, NOW)).toBe("1 day ago");
  });

  it("renders minutes", () => {
    expect(relativeTime(NOW / 1000 - 5 * 60, NOW)).toBe("5 minutes ago");
  });

  it("renders hours", () => {
    expect(relativeTime(NOW / 1000 - 3 * 3600, NOW)).toBe("3 hours ago");
  });

  it("renders days", () => {
    expect(relativeTime(NOW / 1000 - 2 * 86400, NOW)).toBe("2 days ago");
  });

  it("renders weeks", () => {
    expect(relativeTime(NOW / 1000 - 14 * 86400, NOW)).toBe("2 weeks ago");
  });

  it("renders months", () => {
    expect(relativeTime(NOW / 1000 - 90 * 86400, NOW)).toBe("3 months ago");
  });

  it("renders years", () => {
    expect(relativeTime(NOW / 1000 - 400 * 86400, NOW)).toBe("1 year ago");
    expect(relativeTime(NOW / 1000 - 800 * 86400, NOW)).toBe("2 years ago");
  });

  it("degrades a future timestamp (clock skew) to 'just now'", () => {
    expect(relativeTime(NOW / 1000 + 500, NOW)).toBe("just now");
  });

  it("defaults `now` to the real clock when omitted", () => {
    expect(relativeTime(Math.floor(Date.now() / 1000))).toBe("just now");
  });
});

describe("shortSha", () => {
  it("still truncates to 7 chars by default", () => {
    expect(shortSha("abcdefgh12345")).toBe("abcdefg");
  });
});
