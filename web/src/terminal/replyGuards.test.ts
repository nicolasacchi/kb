// T14 — reply guards snapshot: the exact {prefix, intermediates, final}
// tuples registered, and query-vs-set return values.

import { describe, expect, it } from "vitest";
import { installReplyGuards, type ReplyParser } from "./replyGuards";

interface Recorder {
  csi: Array<{ prefix: string; intermediates: string; final: string; handler: (p: number[]) => boolean }>;
  osc: Array<{ code: number; handler: (d: string) => boolean }>;
  dcs: Array<{ prefix: string; intermediates: string; final: string; handler: (p: number[], d: string) => boolean }>;
}

function fakeParser(): { rec: Recorder; parser: ReplyParser } {
  const rec: Recorder = { csi: [], osc: [], dcs: [] };
  const parser: ReplyParser = {
    registerCsiHandler: (prefix, intermediates, final, handler) => {
      rec.csi.push({ prefix, intermediates, final, handler });
      return { dispose: () => {} };
    },
    registerOscHandler: (code, handler) => {
      rec.osc.push({ code, handler });
      return { dispose: () => {} };
    },
    registerDcsHandler: (prefix, intermediates, final, handler) => {
      rec.dcs.push({ prefix, intermediates, final, handler });
      return { dispose: () => {} };
    },
  };
  return { rec, parser };
}

describe("installReplyGuards", () => {
  it("registers the exact CSI tuples from the brief", () => {
    const { rec, parser } = fakeParser();
    installReplyGuards(parser);
    expect(rec.csi.map(({ prefix, intermediates, final }) => ({ prefix, intermediates, final }))).toEqual([
      { prefix: "", intermediates: "", final: "c" }, // DA1
      { prefix: ">", intermediates: "", final: "c" }, // DA2
      { prefix: "=", intermediates: "", final: "c" }, // DA3
      { prefix: "", intermediates: "", final: "n" }, // DSR plain
      { prefix: "?", intermediates: "", final: "n" }, // DSR ?
      { prefix: "", intermediates: "$", final: "p" }, // DECRQM
      { prefix: "?", intermediates: "$", final: "p" }, // DECRQM ?
      { prefix: ">", intermediates: "", final: "q" }, // XTVERSION
      { prefix: "?", intermediates: "", final: "u" }, // kitty keyboard query
      { prefix: "", intermediates: "", final: "x" }, // DECREQTPARM
      { prefix: "", intermediates: "", final: "t" }, // XTWINOPS
      { prefix: "?", intermediates: "", final: "S" }, // XTSMGRAPHICS
    ]);
  });

  it("all query handlers except XTWINOPS return true always", () => {
    const { rec, parser } = fakeParser();
    installReplyGuards(parser);
    for (const entry of rec.csi) {
      if (entry.final === "t") continue;
      expect(entry.handler([1, 2]), `${entry.prefix}${entry.final}`).toBe(true);
    }
    for (const entry of rec.dcs) {
      expect(entry.intermediates === "+" || entry.intermediates === "$" ? entry.handler([], "") : false).toBe(true);
    }
  });

  it("XTWINOPS t returns true only for report params, false for 22/23 and others", () => {
    const { rec, parser } = fakeParser();
    installReplyGuards(parser);
    const t = rec.csi.find((e) => e.final === "t")!;
    expect(t.handler([11])).toBe(true);
    expect(t.handler([13])).toBe(true);
    expect(t.handler([14])).toBe(true);
    expect(t.handler([15])).toBe(true);
    expect(t.handler([16])).toBe(true);
    expect(t.handler([18])).toBe(true);
    expect(t.handler([19])).toBe(true);
    expect(t.handler([20])).toBe(true);
    expect(t.handler([21])).toBe(true);
    expect(t.handler([22])).toBe(false);
    expect(t.handler([23])).toBe(false);
    expect(t.handler([0])).toBe(false);
    expect(t.handler([99])).toBe(false);
    expect(t.handler([])).toBe(false);
  });

  it("registers DCS +q and $q", () => {
    const { rec, parser } = fakeParser();
    installReplyGuards(parser);
    expect(rec.dcs.map(({ prefix, intermediates, final }) => ({ prefix, intermediates, final }))).toEqual([
      { prefix: "", intermediates: "+", final: "q" },
      { prefix: "", intermediates: "$", final: "q" },
    ]);
  });

  it("OSC query payloads (with ?) are swallowed; plain sets pass through", () => {
    const { rec, parser } = fakeParser();
    installReplyGuards(parser);
    expect(rec.osc.map((o) => o.code)).toEqual([4, 5, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 21, 52]);
    for (const o of rec.osc) {
      expect(o.handler("?"), `osc ${o.code} query`).toBe(true);
      expect(o.handler("11;rgb:0000/0000/0000"), `osc ${o.code} set`).toBe(false);
    }
    const osc52 = rec.osc.find((o) => o.code === 52)!;
    expect(osc52.handler("c;?")).toBe(true);
    expect(osc52.handler("c;SGVsbG8=")).toBe(false);
  });
});
