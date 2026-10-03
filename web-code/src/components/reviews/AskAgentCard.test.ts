import { describe, expect, it } from "vitest";
import { agentWatchCommand } from "./AskAgentCard";

describe("agentWatchCommand", () => {
  it("names the review id and the --ignore-agents convention", () => {
    expect(agentWatchCommand(7)).toBe("kb-code annotate watch --review 7 --ignore-agents");
  });

  it("interpolates whatever review id is given, not a fixed sample", () => {
    expect(agentWatchCommand(15533)).toBe(
      "kb-code annotate watch --review 15533 --ignore-agents",
    );
  });
});
