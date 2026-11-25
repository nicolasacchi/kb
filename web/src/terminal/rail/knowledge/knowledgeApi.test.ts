import { afterEach, describe, expect, it, vi } from "vitest";
import { KnowledgeHttpError, fetchKnowledgeRecalls, fetchKnowledgeSimilar, knowledgeRecall } from "./knowledgeApi";

function stub(status: number, json: () => Promise<unknown>) {
  const f = vi.fn(async () => ({ ok: status >= 200 && status < 300, status, statusText: "x", json }));
  vi.stubGlobal("fetch", f);
  return f;
}
afterEach(() => vi.unstubAllGlobals());

describe("knowledgeApi", () => {
  it("maps 404 to not-captured", async () => {
    stub(404, async () => ({}));
    expect(await fetchKnowledgeRecalls("s1")).toEqual({ kind: "not-captured" });
  });
  it("returns the body on 200", async () => {
    stub(200, async () => ({ recalls: [] }));
    expect(await fetchKnowledgeRecalls("s1")).toEqual({ kind: "ok", data: { recalls: [] } });
  });
  it("throws a typed error on 500", async () => {
    stub(500, async () => ({}));
    await expect(fetchKnowledgeRecalls("s1")).rejects.toBeInstanceOf(KnowledgeHttpError);
  });
  it("throws on bad JSON", async () => {
    stub(200, async () => {
      throw new SyntaxError("bad");
    });
    await expect(fetchKnowledgeRecalls("s1")).rejects.toBeInstanceOf(KnowledgeHttpError);
  });
  it("similar sends q, project and limit 5", async () => {
    const f = stub(200, async () => ({ sessions: [], ms: 1 }));
    await fetchKnowledgeSimilar("hello world", "proj");
    const url = String((f.mock.calls[0] as unknown[])[0]);
    expect(url).toContain("/api/sessions/recollect?");
    expect(url).toContain("limit=5");
    expect(url).toContain("project=proj");
  });
  it("search carries scope and limit 20 and never asks for the weekly ledger", async () => {
    const f = stub(200, async () => ({ hits: [], ms: 1 }));
    await knowledgeRecall("abc", "project");
    const url = String((f.mock.calls[0] as unknown[])[0]);
    expect(url).toContain("scope=project");
    expect(url).toContain("limit=20");
    expect(url).not.toContain("with_weekly");
  });
});
