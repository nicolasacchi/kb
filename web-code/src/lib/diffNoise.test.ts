import { describe, expect, it } from "vitest";
import { parseUnifiedDiff, type ParsedDiff } from "./diff";
import {
  buildMovedIndex,
  classifyFile,
  classifyHunk,
  generatedReason,
  isWhitespaceOnlyHunk,
  LARGE_FILE_LINES,
  LARGE_HUNK_LINES,
  movedCounterpart,
  MOVED_INDEX_HUNK_CAP,
  noiseCensus,
  noiseCensusText,
  noiseCollapses,
  noiseSummary,
  NOISE_RULES,
  parseNoiseMode,
  ruleText,
} from "./diffNoise";

function hunkOf(text: string) {
  return parseUnifiedDiff(text).hunks[0];
}

function parsedOf(text: string): ParsedDiff {
  return parseUnifiedDiff(text);
}

describe("NOISE_RULES — the golden table", () => {
  // Pinned so a silent widening (a new path pattern, a moved threshold)
  // reads as a golden diff a reviewer sees, not as a behaviour change.
  it("is exactly five classes, in render order, each with its rule stated", () => {
    expect(NOISE_RULES.map((r) => r.id)).toEqual([
      "generated",
      "rename-only",
      "whitespace-only",
      "moved",
      "large",
    ]);
    expect(NOISE_RULES.map((r) => r.rule)).toEqual([
      "the path matches a generated-output pattern (lockfile, *.gen.*/*.generated.*, schema dump, minified bundle, test snapshot)",
      "the file's status is a rename and the patch adds and removes zero lines",
      "removing every space and tab makes the hunk's added text identical to its removed text",
      "this hunk's removed block is byte-identical to an added block in another file already loaded on this page",
      "the hunk changes more than 120 lines (a file is large past 800 changed lines)",
    ]);
  });

  it("the rule text names the SAME thresholds the code uses", () => {
    expect(ruleText("large")).toContain(String(LARGE_HUNK_LINES));
    expect(ruleText("large")).toContain(String(LARGE_FILE_LINES));
  });
});

describe("generatedReason — the closed path table", () => {
  const hits: Array<[string, string]> = [
    ["Cargo.lock", "lockfile: Cargo.lock"],
    ["web/package-lock.json", "lockfile: package-lock.json"],
    ["go.sum", "lockfile: go.sum"],
    ["db/schema.rb", "schema dump: db/schema.rb"],
    ["src/commands/registry.gen.ts", "generated-marker filename: registry.gen.ts"],
    ["api/user_pb2.py", "protobuf/codegen filename: user_pb2.py"],
    ["rpc/service.pb.go", "protobuf/codegen filename: service.pb.go"],
    ["dist/app.min.js", "minified or source-map bundle: app.min.js"],
    ["dist/app.js.map", "minified or source-map bundle: app.js.map"],
    ["src/__snapshots__/x.aria.yml", "test snapshot: src/__snapshots__/x.aria.yml"],
    ["src/x.test.ts.snap", "test snapshot: src/x.test.ts.snap"],
  ];
  for (const [path, detail] of hits) {
    it(`${path} → ${detail}`, () => {
      expect(generatedReason(path)).toBe(detail);
    });
  }

  const misses = [
    "src/lib.rs",
    "crates/kb-code-server/migrations/V0031__review_hunk_viewed.sql",
    "db/migrate/20260101_add_x.rb",
    "src/generator.ts",
    "Cargo.toml",
  ];
  for (const path of misses) {
    it(`${path} is NOT generated`, () => {
      // A hand-authored migration is the CHANGE, not a rendering of it —
      // this file's header states why the schema-dump rule stops short of
      // `migrations/`.
      expect(generatedReason(path)).toBeNull();
    });
  }
});

describe("whitespace-only", () => {
  it("catches a pure re-indent", () => {
    expect(
      isWhitespaceOnlyHunk(
        hunkOf(`@@ -1,2 +1,2 @@
-  let x = 1;
+    let x = 1;
`),
      ),
    ).toBe(true);
  });

  it("catches a reflow that moves a token across lines", () => {
    expect(
      isWhitespaceOnlyHunk(
        hunkOf(`@@ -1,1 +1,2 @@
-fn a(b: u8, c: u8) {}
+fn a(b: u8,
+     c: u8) {}
`),
      ),
    ).toBe(true);
  });

  it("does NOT fire on a real edit", () => {
    expect(
      isWhitespaceOnlyHunk(
        hunkOf(`@@ -1,1 +1,1 @@
-let x = 1;
+let x = 2;
`),
      ),
    ).toBe(false);
  });

  it("does NOT fire on a pure addition or a pure deletion", () => {
    expect(isWhitespaceOnlyHunk(hunkOf("@@ -0,0 +1,1 @@\n+added\n"))).toBe(false);
    expect(isWhitespaceOnlyHunk(hunkOf("@@ -1,1 +0,0 @@\n-gone\n"))).toBe(false);
  });

  it("does NOT fire when both sides strip to nothing", () => {
    expect(
      isWhitespaceOnlyHunk(
        hunkOf(`@@ -1,1 +1,1 @@
-
+
`),
      ),
    ).toBe(false);
  });
});

describe("moved", () => {
  const BLOCK = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta"];
  const removedFrom = parsedOf(
    `@@ -10,6 +10,0 @@\n${BLOCK.map((l) => `-${l}`).join("\n")}\n`,
  );
  const addedTo = parsedOf(`@@ -1,0 +1,6 @@\n${BLOCK.map((l) => `+${l}`).join("\n")}\n`);

  it("names the counterpart file", () => {
    const index = buildMovedIndex(new Map([["dst.rs", addedTo]]));
    expect(movedCounterpart("src.rs", removedFrom.hunks[0], index)).toBe("dst.rs");
  });

  it("does NOT report an intra-file reorder", () => {
    const index = buildMovedIndex(new Map([["src.rs", addedTo]]));
    expect(movedCounterpart("src.rs", removedFrom.hunks[0], index)).toBeNull();
  });

  it("ignores blocks shorter than the stated minimum", () => {
    const shortRemoved = parsedOf("@@ -1,2 +1,0 @@\n-alpha\n-beta\n");
    const shortAdded = parsedOf("@@ -1,0 +1,2 @@\n+alpha\n+beta\n");
    const index = buildMovedIndex(new Map([["dst.rs", shortAdded]]));
    expect(movedCounterpart("src.rs", shortRemoved.hunks[0], index)).toBeNull();
  });

  it("matches across a change of indentation (lines are trimmed)", () => {
    const indented = parsedOf(
      `@@ -1,0 +1,6 @@\n${BLOCK.map((l) => `+        ${l}`).join("\n")}\n`,
    );
    const index = buildMovedIndex(new Map([["dst.rs", indented]]));
    expect(movedCounterpart("src.rs", removedFrom.hunks[0], index)).toBe("dst.rs");
  });

  it("reports its own census, including the cap", () => {
    const index = buildMovedIndex(new Map([["dst.rs", addedTo]]));
    expect(index).toMatchObject({ scanned: 1, capped: false });
    // The cap is a real bound, stated rather than implicit.
    expect(MOVED_INDEX_HUNK_CAP).toBe(600);
  });
});

describe("classifyFile / classifyHunk", () => {
  it("labels a lockfile generated, with its own detail", () => {
    const labels = classifyFile({ path: "Cargo.lock", status: "M", additions: 4, deletions: 2 });
    expect(labels.map((l) => l.cls)).toEqual(["generated"]);
    expect(labels[0].detail).toBe("lockfile: Cargo.lock");
    expect(labels[0].rule).toBe(ruleText("generated"));
  });

  it("labels a pure rename", () => {
    const labels = classifyFile({ path: "b.rs", status: "R100", additions: 0, deletions: 0 });
    expect(labels.map((l) => l.cls)).toEqual(["rename-only"]);
  });

  it("does NOT call a rename-with-edits rename-only", () => {
    const labels = classifyFile({ path: "b.rs", status: "R80", additions: 3, deletions: 1 });
    expect(labels).toEqual([]);
  });

  it("labels a large file at its own, higher threshold", () => {
    const labels = classifyFile({ path: "b.rs", status: "M", additions: 900, deletions: 0 });
    expect(labels.map((l) => l.cls)).toEqual(["large"]);
    expect(labels[0].detail).toContain("900 changed lines");
  });

  it("a hunk INHERITS the file's non-large labels and re-tests large itself", () => {
    const fileLabels = classifyFile({
      path: "Cargo.lock",
      status: "M",
      additions: 900,
      deletions: 0,
    });
    const small = hunkOf("@@ -1,1 +1,1 @@\n-a\n+b\n");
    const labels = classifyHunk("Cargo.lock", small, fileLabels, null);
    // `generated` rides down; the file's `large` does NOT (a small hunk of
    // a big file is not a big hunk).
    expect(labels.map((l) => l.cls)).toEqual(["generated"]);
  });

  it("labels a large hunk", () => {
    const body = Array.from({ length: LARGE_HUNK_LINES + 1 }, (_, i) => `+line ${i}`).join("\n");
    const big = hunkOf(`@@ -0,0 +1,${LARGE_HUNK_LINES + 1} @@\n${body}\n`);
    const labels = classifyHunk("b.rs", big, [], null);
    expect(labels.map((l) => l.cls)).toEqual(["large"]);
  });
});

describe("noise is a LABEL, never a filter", () => {
  it("`shown` never collapses anything, whatever the labels", () => {
    const labels = classifyFile({ path: "Cargo.lock", status: "M", additions: 1, deletions: 0 });
    expect(noiseCollapses("shown", labels)).toBe(false);
    expect(noiseCollapses("shown", [])).toBe(false);
  });

  it("`collapsed` collapses only LABELLED hunks", () => {
    const labels = classifyFile({ path: "Cargo.lock", status: "M", additions: 1, deletions: 0 });
    expect(noiseCollapses("collapsed", labels)).toBe(true);
    expect(noiseCollapses("collapsed", [])).toBe(false);
  });

  it("parseNoiseMode is TOTAL and defaults to shown", () => {
    expect(parseNoiseMode("collapsed")).toBe("collapsed");
    expect(parseNoiseMode("shown")).toBe("shown");
    expect(parseNoiseMode(null)).toBe("shown");
    expect(parseNoiseMode("hidden")).toBe("shown");
    expect(parseNoiseMode("")).toBe("shown");
  });
});

describe("the census", () => {
  it("counts labelled hunks per class and reports the total", () => {
    const gen = classifyFile({ path: "Cargo.lock", status: "M", additions: 1, deletions: 0 });
    const census = noiseCensus([gen, [], gen]);
    expect(census.total).toBe(3);
    expect(census.labelled).toBe(2);
    expect(census.byClass.get("generated")).toBe(2);
    expect(noiseCensusText(census)).toBe("2/3 hunks labelled · generated 2");
  });

  it("says so honestly when nothing is labelled, and when nothing is loaded", () => {
    expect(noiseCensusText(noiseCensus([[], []]))).toBe("2 hunks · none labelled noise");
    expect(noiseCensusText(noiseCensus([]))).toBe("no hunks loaded");
  });

  it("summarises multiple labels in NOISE_RULES order", () => {
    const gen = classifyFile({ path: "Cargo.lock", status: "M", additions: 900, deletions: 0 });
    expect(noiseSummary(gen)).toBe("generated, large");
  });
});
