import { describe, expect, it } from "vitest";
import { renderToString } from "react-dom/server";
import MemoryLinkChips from "./MemoryLinkChips";
import type { RecallHit } from "../api/client";

const hit = (over: Partial<RecallHit>) =>
  ({ kb: "m", id: "i", title: "t", ...over }) as RecallHit;

describe("MemoryLinkChips", () => {
  it("labels the unlinked state in words, not a bare 'none'", () => {
    const html = renderToString(<MemoryLinkChips hit={hit({})} />);
    expect(html).toContain("no kb links");
    expect(html).not.toMatch(/>none</);
  });
  it("labels the global state in words next to the star", () => {
    const html = renderToString(<MemoryLinkChips hit={hit({ global: true })} />);
    expect(html).toContain("global");
  });
});
