import { describe, expect, it } from "vitest";
import type { ConvRow } from "../../api/generated/ConvRow";
import {
  captureMtime,
  copyText,
  excerpt,
  filterRows,
  freshnessText,
  jumpHref,
  windowCaption,
} from "./conversationRows";

const row = (n: number, over: Partial<ConvRow> = {}): ConvRow => ({
  n,
  id: `t-${String(n).padStart(12, "0")}`,
  ts: null,
  prompt: `prompt ${n}`,
  answer: `answer ${n}`,
  asks: [],
  commits: [],
  files_touched: 0,
  errors: 0,
  subagents: { n: 0, errors: 0 },
  ...over,
});
const ask = { kind: "ask", q: "Which?", options: ["a", "b"], answer: null, pending: true };

describe("filterRows", () => {
  const rows = [row(1), row(2, { asks: [ask] }), row(3, { answer: null })];
  it("all keeps everything", () => expect(filterRows(rows, "all").map((r) => r.n)).toEqual([1, 2, 3]));
  it("questions keeps rows with asks", () => expect(filterRows(rows, "questions").map((r) => r.n)).toEqual([2]));
  it("answers keeps rows with a non-null answer", () => expect(filterRows(rows, "answers").map((r) => r.n)).toEqual([1, 2]));
  it("an empty-string answer still counts as an answer", () =>
    expect(filterRows([row(4, { answer: "" })], "answers")).toHaveLength(1));
});

describe("excerpt", () => {
  it("leaves short text alone and collapses whitespace", () => expect(excerpt("a  b\n c")).toBe("a b c"));
  it("cuts long text at a word edge with an ellipsis", () => {
    const out = excerpt("word ".repeat(100), 50);
    expect(out.endsWith("…")).toBe(true);
    expect(out.length).toBeLessThanOrEqual(51);
    expect(out).not.toMatch(/wor…$/);
  });
});

describe("freshness", () => {
  const T = 1_700_000_000;
  const at = (secs: number) => (T + secs) * 1000;
  it("parses unix seconds out of capture_gen", () => {
    expect(captureMtime(`${T}-12`)).toBe(T);
    expect(captureMtime("garbage")).toBeNull();
    expect(captureMtime("0-5")).toBeNull();
    expect(captureMtime("12-")).toBeNull();
  });
  it("scales just now / minutes / hours / days at the boundaries", () => {
    expect(freshnessText(`${T}-3`, at(0))).toBe("As of last capture, just now");
    expect(freshnessText(`${T}-3`, at(59))).toBe("As of last capture, just now");
    expect(freshnessText(`${T}-3`, at(60))).toBe("As of last capture, 1 min ago");
    expect(freshnessText(`${T}-3`, at(3599))).toBe("As of last capture, 59 min ago");
    expect(freshnessText(`${T}-3`, at(3600))).toBe("As of last capture, 1 h ago");
    expect(freshnessText(`${T}-3`, at(86_399))).toBe("As of last capture, 23 h ago");
    expect(freshnessText(`${T}-3`, at(86_400))).toBe("As of last capture, 1 d ago");
  });
  it("a capture time in the future clamps to just now; unparsable says so", () => {
    expect(freshnessText(`${T}-3`, at(-500))).toBe("As of last capture, just now");
    expect(freshnessText("nope", at(0))).toBe("Capture time unknown");
  });
});

describe("windowCaption / copyText / jumpHref", () => {
  it("captions only a narrowed window", () => {
    expect(windowCaption(250, 100)).toBe("Showing last 100 of 250 turns");
    expect(windowCaption(3, 3)).toBeNull();
  });
  it("copy text is prompt, blank line, answer; prompt alone without an answer", () => {
    expect(copyText(row(1))).toBe("prompt 1\n\nanswer 1");
    expect(copyText(row(1, { answer: null }))).toBe("prompt 1");
  });
  it("the jump href carries the verbatim t- id and encodes the path", () => {
    expect(jumpHref("kb one", "sessions/a b/x.html", "t-0123456789ab")).toBe(
      "/a/kb%20one/sessions/a%20b/x.html?turn=t-0123456789ab",
    );
  });
});
