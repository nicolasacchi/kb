import { describe, it, expect } from "vitest";
import { canonicalArtifactLocation } from "./canonicalArtifactLocation";

describe("canonicalArtifactLocation", () => {
  it("returns null when the URL already names the live path", () => {
    expect(canonicalArtifactLocation("kb", "a/b.html", "a/b.html", "?p=2", "#x")).toBeNull();
  });
  it("returns null while the doc has not resolved", () => {
    expect(canonicalArtifactLocation("kb", "a/b.html", undefined, "", "")).toBeNull();
  });
  it("rewrites a moved path, preserving query and hash", () => {
    expect(
      canonicalArtifactLocation("kb", "old/b.html", "new/b.html", "?p=two&sec=s1", "#frag"),
    ).toBe("/a/kb/new/b.html?p=two&sec=s1#frag");
  });
  it("builds the path with artifactHref encoding", () => {
    expect(canonicalArtifactLocation("my kb", "x.html", "moved/has space.html", "", "")).toBe(
      "/a/my%20kb/moved/has%20space.html",
    );
  });
  it("tolerates a leading slash / backslashes in the requested rel", () => {
    expect(canonicalArtifactLocation("kb", "/a\\b.html", "a/b.html", "", "")).toBeNull();
  });
});
