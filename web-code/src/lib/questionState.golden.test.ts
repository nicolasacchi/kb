// The kbc-question-state/1 LOCK-STEP golden — the SPA half.
//
// `crates/kb-code-server/grammar/question-state.golden.json` is read by the
// daemon's `review_queue` test (`awaiting_agent`, which backs the agent
// queue's lane 1) and by this file (`questionChipState`). Neither generates
// the other: a change to either implementation that moves one case fails the
// side that moved, naming the case. Reading across the crate boundary is fine
// in a vitest (same as agentAuthors.golden.test.ts); only the bundle may
// never import across it.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import { questionChipState, type QuestionThreadInput } from "./questionState";

const GOLDEN_PATH = fileURLToPath(
  new URL("../../../crates/kb-code-server/grammar/question-state.golden.json", import.meta.url),
);

interface Case extends QuestionThreadInput {
  name: string;
  chip: "awaiting-agent" | "awaiting-you" | null;
}

describe("kbc-question-state/1 golden", () => {
  const fx = JSON.parse(readFileSync(GOLDEN_PATH, "utf8")) as { schema: string; cases: Case[] };

  it("is the version both sides pin", () => {
    expect(fx.schema).toBe("kbc-question-state/1");
    expect(fx.cases.length).toBeGreaterThanOrEqual(8);
  });

  for (const c of fx.cases) {
    it(c.name, () => {
      expect(questionChipState(c)).toBe(c.chip);
    });
  }
});
