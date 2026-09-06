import { describe, expect, it } from "vitest";
import { isAgentHotHumanCold, orderUnverifiedFirst, tensionLabel } from "./attention";

// CT-B6 → CT-E4 — the ONE agent-hot-human-cold condition, shared by the
// dossier's Attention section, the /memory row badge, and the
// ?sort=unverified ordering. These pins are the SPA half of the lock-step
// with the server's `never_opened_by_human` + `unverified_cmp`
// (crates/kb-server/src/routes/memory.rs — see its
// `census_unverified_cmp_*` twins).

describe("isAgentHotHumanCold", () => {
  it("true only when recalled at least once AND never opened (read_pct absent or 0)", () => {
    expect(isAgentHotHumanCold({ recall_count: 14 })).toBe(true);
    expect(isAgentHotHumanCold({ recall_count: 14, read_pct: null })).toBe(true);
    expect(isAgentHotHumanCold({ recall_count: 14, read_pct: 0 })).toBe(true);
    expect(isAgentHotHumanCold({ recall_count: 1, read_pct: undefined })).toBe(true);
  });

  it("false once the human has opened it, however often agents recalled it", () => {
    expect(isAgentHotHumanCold({ recall_count: 14, read_pct: 1 })).toBe(false);
    expect(isAgentHotHumanCold({ recall_count: 14, read_pct: 90 })).toBe(false);
  });

  it("false when never recalled — nothing is agent-hot", () => {
    expect(isAgentHotHumanCold({ recall_count: 0 })).toBe(false);
    expect(isAgentHotHumanCold({ recall_count: 0, read_pct: 0 })).toBe(false);
  });
});

describe("tensionLabel", () => {
  it("is the CT-B6 wording, byte-for-byte", () => {
    expect(tensionLabel(14)).toBe("Recalled 14× by agents · never opened by you");
    expect(tensionLabel(1)).toBe("Recalled 1× by agents · never opened by you");
  });
});

describe("orderUnverifiedFirst", () => {
  const row = (id: string, recall_count: number, read_pct?: number | null) => ({
    id,
    recall_count,
    read_pct,
  });

  it("buckets agent-hot-human-cold rows first, recall_count DESC within and after", () => {
    const out = orderUnverifiedFirst([
      row("a", 0), //          cold: never recalled
      row("b", 5, 90), //      cold: recalled but opened
      row("c", 2), //          HOT
      row("d", 9, 0), //       HOT (read_pct 0 counts as never opened)
      row("e", 9, 40), //      cold: recalled but opened
    ]);
    expect(out.map((r) => r.id)).toEqual(["d", "c", "e", "b", "a"]);
  });

  it("the bucket dominates the count: one hot recall beats many opened ones", () => {
    const out = orderUnverifiedFirst([row("opened", 7, 80), row("hot", 1)]);
    expect(out.map((r) => r.id)).toEqual(["hot", "opened"]);
  });

  it("ties keep the INPUT order (the recall score ranking) — deterministic, stable", () => {
    const out = orderUnverifiedFirst([
      row("first", 3),
      row("second", 3),
      row("third", 0),
      row("fourth", 0),
    ]);
    expect(out.map((r) => r.id)).toEqual(["first", "second", "third", "fourth"]);
  });

  it("no signals at all ⇒ the input order untouched (a pure pass-through), input never mutated", () => {
    const input = [row("x", 0), row("y", 0), row("z", 0)];
    const snapshot = input.map((r) => r.id);
    const out = orderUnverifiedFirst(input);
    expect(out.map((r) => r.id)).toEqual(["x", "y", "z"]);
    expect(input.map((r) => r.id)).toEqual(snapshot);
    expect(out).not.toBe(input);
  });
});
