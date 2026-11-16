import { describe, expect, it } from "vitest";
import type { SessionViewResponse } from "../api/generated/SessionViewResponse";
import { extractImageRefs } from "./sessionImages";

const SHA = "d".repeat(64);

function view(turns: unknown[]): Partial<SessionViewResponse> {
  return { turns } as Partial<SessionViewResponse>;
}

const turn = (id: string, ts: string | null, items: unknown[]) => ({
  id,
  ordinal: 1,
  role: "Human",
  ts,
  sidechain: false,
  agent_id: null,
  items,
  raw_lines: [],
});

describe("extractImageRefs", () => {
  it("reads Image items with their turn id and time, in order, ignoring other items", () => {
    const refs = extractImageRefs(
      view([
        turn("t-000000000001", "2026-07-01T09:00:00Z", [
          { item: "Prose", text: "hi" },
          { item: "Image", mime: "image/png", sha256: SHA, inline_bytes: null, width: null, height: null },
        ]),
        turn("t-000000000002", null, [
          { item: "Image", mime: "image/jpeg", sha256: null, inline_bytes: 12, width: null, height: null },
        ]),
      ]),
    );
    expect(refs).toEqual([
      { mime: "image/png", sha256: SHA, turnId: "t-000000000001", ts: "2026-07-01T09:00:00Z" },
      { mime: "image/jpeg", sha256: null, turnId: "t-000000000002", ts: null },
    ]);
  });

  it("a sha that is not lowercase hex64 is dropped to null, never trusted", () => {
    const refs = extractImageRefs(
      view([
        turn("t-000000000001", null, [
          { item: "Image", mime: "image/png", sha256: "../../x", inline_bytes: null, width: null, height: null },
          { item: "Image", mime: "image/png", sha256: SHA.toUpperCase(), inline_bytes: null, width: null, height: null },
        ]),
      ]),
    );
    expect(refs.map((r) => r.sha256)).toEqual([null, null]);
  });

  it("a view without turns yields nothing", () => {
    expect(extractImageRefs({})).toEqual([]);
  });
});
