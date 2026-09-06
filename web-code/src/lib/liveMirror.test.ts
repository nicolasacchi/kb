import { describe, expect, it } from "vitest";
import { decideLiveMirrorAction, eventNamesOpenFile, fileChangedPaneLabel } from "./liveMirror";

describe("decideLiveMirrorAction", () => {
  it("ignores an update for a file that isn't open, regardless of viewer state", () => {
    expect(decideLiveMirrorAction({ fileChanged: false, viewerDirty: false })).toBe("ignore");
    expect(decideLiveMirrorAction({ fileChanged: false, viewerDirty: true })).toBe("ignore");
  });

  it("auto-refreshes the open file when the viewer is pristine", () => {
    expect(decideLiveMirrorAction({ fileChanged: true, viewerDirty: false })).toBe("auto-refresh");
  });

  it("prompts instead of clobbering when the viewer has scrolled or selected", () => {
    expect(decideLiveMirrorAction({ fileChanged: true, viewerDirty: true })).toBe("prompt");
  });
});

describe("eventNamesOpenFile", () => {
  it("matches an exact repo-relative path", () => {
    expect(eventNamesOpenFile(["src/lib.rs", "README.md"], "src/lib.rs")).toBe(true);
  });

  it("does not match when the path isn't in the list", () => {
    expect(eventNamesOpenFile(["README.md"], "src/lib.rs")).toBe(false);
  });

  it("never matches when no file is open", () => {
    expect(eventNamesOpenFile(["src/lib.rs"], undefined)).toBe(false);
    expect(eventNamesOpenFile(["src/lib.rs"], null)).toBe(false);
    expect(eventNamesOpenFile(["src/lib.rs"], "")).toBe(false);
  });

  it("does not partial-match a directory prefix", () => {
    expect(eventNamesOpenFile(["src/lib.rs"], "src")).toBe(false);
  });
});

describe("fileChangedPaneLabel", () => {
  it("labels the pane when the other pane is open (a split is active)", () => {
    expect(fileChangedPaneLabel(1, true)).toBe("Pane 1");
    expect(fileChangedPaneLabel(2, true)).toBe("Pane 2");
  });

  it("omits the label in the single-pane case — no ambiguity to resolve", () => {
    expect(fileChangedPaneLabel(1, false)).toBeUndefined();
    expect(fileChangedPaneLabel(2, false)).toBeUndefined();
  });
});
