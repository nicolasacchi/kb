import { describe, expect, it } from "vitest";
import { UI_PROBE } from "./probe";

describe("ui probe constant", () => {
  it("is ui/1", () => {
    expect(UI_PROBE).toBe("ui/1");
  });
});
