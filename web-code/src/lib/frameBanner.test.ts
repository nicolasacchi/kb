import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { frameBanner, type FrameRow } from "./frameBanner";

const GOLDEN_PATH = fileURLToPath(
  new URL("../../../crates/kb-code-server/tests/fixtures/frames.golden.json", import.meta.url),
);

interface FramesGolden {
  frames: FrameRow[];
}

const TABLE = JSON.parse(readFileSync(GOLDEN_PATH, "utf8")) as FramesGolden;

describe("frameBanner", () => {
  it("is silent on the working tree for every lane", () => {
    for (const row of TABLE.frames) {
      expect(frameBanner(row.lane, row, null)).toBeNull();
      expect(frameBanner(row.lane, row, undefined)).toBeNull();
    }
  });

  it("walks the table × lanes: every off-HEAD banner is generated, none hand-written", () => {
    const at = "HEAD~1";
    const byLane: Record<string, string> = {};
    for (const row of TABLE.frames) {
      const text = frameBanner(row.lane, row, at);
      expect(text, row.lane).toBeTruthy();
      byLane[row.lane] = text as string;
    }
    expect(byLane.file_at_ref).toBe("file at ref: ODB");
    expect(byLane.tree).toBe("tree at ref: ODB (ceiling exact)");
    expect(byLane.blame).toBe("blame at ref: git (ceiling exact)");
    expect(byLane.lsp_live.startsWith("lsp_live: refused —")).toBe(true);
    expect(byLane.text).toBe("text: answers the checkout (working tree)");
    expect(byLane.symbols).toBe("symbols: answers the checkout (working tree)");
    expect(byLane.files).toBe("files: answers the checkout (working tree)");
    expect(byLane.usages).toBe("usages: answers the checkout (working tree)");
    expect(byLane.framework_edges).toBe("framework_edges: answers the checkout (working tree)");
  });
});
