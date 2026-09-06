import { describe, expect, it } from "vitest";

import { sessionExportUrl } from "./download";

describe("sessionExportUrl", () => {
  it("builds the daemon session-export path", () => {
    // Default daemon base is same-origin ("") in the test env.
    expect(sessionExportUrl("abc-123")).toBe("/api/sessions/abc-123/export");
  });

  it("percent-encodes an unsafe session id", () => {
    expect(sessionExportUrl("a/b?c")).toBe("/api/sessions/a%2Fb%3Fc/export");
  });
});
