// The kbc-agent-authors/1 LOCK-STEP golden — the SPA half.
//
// `crates/kb-code-server/grammar/agent-authors.golden.json` is read by the
// daemon's `review_timeline` test (`AGENT_AUTHOR_NAMES`) and by this file
// (`questionState.ts`'s `AGENT_AUTHOR_NAMES`). Neither generates the other;
// a name added on one side fails the other naming the difference. Reading
// across the crate boundary is fine in a vitest (same as the hunk-id
// golden); only the bundle may never import across it.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import { AGENT_AUTHOR_NAMES, isAgentAuthorName } from "./questionState";

const GOLDEN_PATH = fileURLToPath(
  new URL(
    "../../../crates/kb-code-server/grammar/agent-authors.golden.json",
    import.meta.url,
  ),
);

describe("kbc-agent-authors/1 golden", () => {
  const fx = JSON.parse(readFileSync(GOLDEN_PATH, "utf8")) as {
    schema: string;
    names: string[];
  };
  it("the SPA's agent-author set equals the daemon's, in order", () => {
    expect(fx.schema).toBe("kbc-agent-authors/1");
    expect([...AGENT_AUTHOR_NAMES]).toEqual(fx.names);
  });
  it("every fixture name classifies as an agent; 'you' does not", () => {
    for (const n of fx.names) expect(isAgentAuthorName(n)).toBe(true);
    expect(isAgentAuthorName("you")).toBe(false);
  });
});
