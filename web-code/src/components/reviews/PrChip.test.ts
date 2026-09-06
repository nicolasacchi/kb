import { describe, expect, it } from "vitest";
import { prExternalUrl } from "./PrChip";

describe("prExternalUrl", () => {
  it("builds a github.com PR URL from an owner/name slug + number", () => {
    expect(prExternalUrl("acme-shop/root-acme-shop", 15533)).toBe(
      "https://github.com/acme-shop/root-acme-shop/pull/15533",
    );
  });

  it("is null when the slug is absent", () => {
    expect(prExternalUrl(undefined, 15533)).toBeNull();
  });

  it("is null when the PR number is absent", () => {
    expect(prExternalUrl("owner/repo", undefined)).toBeNull();
  });

  it("is null for the degraded 'unknown' slug (non-GitHub origin)", () => {
    expect(prExternalUrl("unknown", 1)).toBeNull();
  });

  it("is null for a raw origin URL slug (not a real owner/name pair)", () => {
    expect(prExternalUrl("https://git.example.com/x.git", 1)).toBeNull();
  });
});
