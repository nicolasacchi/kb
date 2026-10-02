import { describe, expect, it } from "vitest";
import { emptyReview } from "../api/client";
import { publicView } from "./publicView";

function withComments(rows: Array<{ id: string; body: string; private?: boolean }>) {
  const f = emptyReview("a", "k");
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  f.comments = rows.map((r) => ({ ...r, replies: [], status: "open" }) as any);
  return f;
}

describe("publicView", () => {
  it("drops private notes and keeps everything else", () => {
    const f = withComments([
      { id: "c1", body: "public" },
      { id: "c2", body: "SECRET NOTE", private: true },
      { id: "c3", body: "explicitly public", private: false },
    ]);
    const v = publicView(f);
    expect(v.comments.map((c) => c.id)).toEqual(["c1", "c3"]);
    expect(JSON.stringify(v)).not.toContain("SECRET NOTE");
    expect(v.artifact).toEqual(f.artifact);
  });

  it("does not mutate its input", () => {
    const f = withComments([{ id: "c2", body: "n", private: true }]);
    publicView(f);
    expect(f.comments).toHaveLength(1);
  });
});
