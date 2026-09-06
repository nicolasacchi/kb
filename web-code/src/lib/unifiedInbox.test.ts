import { describe, expect, it } from "vitest";
import {
  annotationReaderUrl,
  computeInboxBadge,
  groupByRepo,
  kbArtifactUrl,
  kbCommentUrl,
  kbLaneState,
  truncationCaption,
} from "./unifiedInbox";

describe("computeInboxBadge", () => {
  it("sums reviews + annotations when kb is unavailable", () => {
    const n = computeInboxBadge({
      reviews: [1, 2, 3],
      annotations: [1],
      kb: { available: false, desk: null },
    });
    expect(n).toBe(4);
  });

  it("adds kb desk attention when kb is available", () => {
    const n = computeInboxBadge({
      reviews: [1, 2],
      annotations: [],
      kb: { available: true, desk: { items: [], attention: 5 } },
    });
    expect(n).toBe(7);
  });

  it("does not add attention when kb is available but desk is null", () => {
    const n = computeInboxBadge({
      reviews: [1],
      annotations: [1, 2],
      kb: { available: true, desk: null },
    });
    expect(n).toBe(3);
  });

  it("returns 0 for an entirely empty inbox", () => {
    const n = computeInboxBadge({ reviews: [], annotations: [], kb: { available: false, desk: null } });
    expect(n).toBe(0);
  });
});

describe("kbLaneState", () => {
  it("is available when kb.available is true, regardless of reason", () => {
    expect(kbLaneState({ available: true, reason: null })).toEqual({ kind: "available" });
  });

  it("labels the disabled reason", () => {
    expect(kbLaneState({ available: false, reason: "disabled" })).toEqual({
      kind: "unavailable",
      reason: "disabled",
      label: "kb integration disabled",
    });
  });

  it("labels the unreachable reason", () => {
    expect(kbLaneState({ available: false, reason: "unreachable" })).toEqual({
      kind: "unavailable",
      reason: "unreachable",
      label: "kb unreachable",
    });
  });

  it("labels the sibling_mismatch reason", () => {
    expect(kbLaneState({ available: false, reason: "sibling_mismatch" })).toEqual({
      kind: "unavailable",
      reason: "sibling_mismatch",
      label: "kb/kb-code version mismatch",
    });
  });

  it("degrades a null reason to unreachable rather than throwing", () => {
    expect(kbLaneState({ available: false, reason: null })).toEqual({
      kind: "unavailable",
      reason: "unreachable",
      label: "kb unreachable",
    });
  });

  it("degrades an unknown reason string to an honest fallback label", () => {
    // A future daemon's new reason value — exercised via an `unknown` cast
    // (the type is a closed vocab today; the wire is not).
    const futureReason = "something_new" as unknown as Parameters<typeof kbLaneState>[0]["reason"];
    const state = kbLaneState({ available: false, reason: futureReason });
    expect(state).toEqual({ kind: "unavailable", reason: "something_new", label: "kb unavailable (something_new)" });
  });
});

describe("truncationCaption", () => {
  it("renders the default cap when truncated", () => {
    expect(truncationCaption(true)).toBe("showing first 50");
  });

  it("renders a custom cap when truncated", () => {
    expect(truncationCaption(true, 10)).toBe("showing first 10");
  });

  it("is null when not truncated", () => {
    expect(truncationCaption(false)).toBeNull();
  });

  it("is null when truncated is absent", () => {
    expect(truncationCaption(undefined)).toBeNull();
  });
});

describe("groupByRepo", () => {
  it("groups in first-appearance order, preserving within-group order", () => {
    const rows = [
      { repo: "acme/a", id: 1 },
      { repo: "acme/b", id: 2 },
      { repo: "acme/a", id: 3 },
      { repo: "acme/c", id: 4 },
      { repo: "acme/b", id: 5 },
    ];
    expect(groupByRepo(rows)).toEqual([
      { repo: "acme/a", rows: [rows[0], rows[2]] },
      { repo: "acme/b", rows: [rows[1], rows[4]] },
      { repo: "acme/c", rows: [rows[3]] },
    ]);
  });

  it("returns an empty array for no rows", () => {
    expect(groupByRepo([])).toEqual([]);
  });
});

describe("annotationReaderUrl", () => {
  it("links to the reader with a line param when line is set", () => {
    expect(annotationReaderUrl({ repo: "acme/widgets", path: "src/lib.rs", line: 42 })).toBe(
      "/r/acme%2Fwidgets/src/lib.rs?line=42",
    );
  });

  it("omits the line param when line is null", () => {
    expect(annotationReaderUrl({ repo: "acme/widgets", path: "src/lib.rs", line: null })).toBe(
      "/r/acme%2Fwidgets/src/lib.rs",
    );
  });

  it("omits the line param when line is undefined", () => {
    expect(annotationReaderUrl({ repo: "acme/widgets", path: "src/lib.rs", line: undefined })).toBe(
      "/r/acme%2Fwidgets/src/lib.rs",
    );
  });
});

describe("kbArtifactUrl", () => {
  it("builds the /a/<kb>/<rel> permalink against the given base", () => {
    expect(kbArtifactUrl("https://kb.example.com", "research", "ideas/foo bar.html")).toBe(
      "https://kb.example.com/a/research/ideas/foo%20bar.html",
    );
  });

  it("trims a trailing slash on the base so the join never double-slashes", () => {
    expect(kbArtifactUrl("https://kb.example.com/", "research", "foo.html")).toBe(
      "https://kb.example.com/a/research/foo.html",
    );
  });

  it("percent-encodes the kb name and drops empty path segments", () => {
    expect(kbArtifactUrl("http://127.0.0.1:4000", "my kb", "//a//b.html")).toBe(
      "http://127.0.0.1:4000/a/my%20kb/a/b.html",
    );
  });
});

describe("kbCommentUrl", () => {
  it("appends ?panel=comments to the artifact permalink", () => {
    expect(kbCommentUrl("https://kb.example.com", "research", "ideas/foo.html")).toBe(
      "https://kb.example.com/a/research/ideas/foo.html?panel=comments",
    );
  });
});
