import { describe, expect, it } from "vitest";
import type { SessionFileOut } from "../../../api/generated/SessionFileOut";
import { commitHref, type CommitItem } from "../../../api/sessionCard";
import { codeSearchUrl } from "../../../lib/codeLensUrl";
import {
  COMMIT_CAP,
  capCommits,
  commitDoor,
  dirOf,
  fileDoor,
  freshnessText,
  groupFiles,
  matchCodeItem,
  readCount,
  sessionPageHref,
  tierFor,
} from "./changesModel";

const f = (path: string, action: string, over: Partial<SessionFileOut> = {}): SessionFileOut => ({
  path,
  basename: path.slice(path.lastIndexOf("/") + 1),
  action,
  in_corpus: false,
  via_subagent: false,
  ...over,
});
const item = (sha: string, over: Partial<CommitItem> = {}): CommitItem => ({
  repo: "r",
  sha,
  visible: true,
  confidence: "exact",
  via: "x",
  attributed_here: true,
  ...over,
});

describe("dirOf", () => {
  it("root, nested and absolute paths", () => {
    expect(dirOf("a.txt")).toBe(".");
    expect(dirOf("src/a.txt")).toBe("src");
    expect(dirOf("/a.txt")).toBe("/");
    expect(dirOf("/w/src/a.txt")).toBe("/w/src");
  });
});

describe("groupFiles", () => {
  const files = [
    f("src/z.ts", "read"),
    f("src/b.ts", "edit"),
    f("src/a.ts", "write"),
    f("top.md", "edit"),
    f("lib/x.ts", "read"),
    f("src/b.ts", "edit"),
    f("src/b.ts", "edit", { via_subagent: true }),
  ];
  it("sorts directories, root group, edited before read, de-duplicates", () => {
    const g = groupFiles(files, true);
    expect(g.map((d) => d.dir)).toEqual([".", "lib", "src"].sort());
    const src = g.find((d) => d.dir === "src")!;
    expect(src.rows.map((r) => `${r.basename}:${r.action}:${r.viaSubagent}`)).toEqual([
      "a.ts:write:false",
      "b.ts:edit:false",
      "b.ts:edit:true",
      "z.ts:read:false",
    ]);
  });
  it("hides reads (and emptied directories) by default", () => {
    const g = groupFiles(files, false);
    expect(g.map((d) => d.dir)).toEqual([".", "src"]);
    expect(g.flatMap((d) => d.rows).every((r) => r.edited)).toBe(true);
    expect(readCount(files)).toBe(2);
  });
});

describe("tier + doors", () => {
  const items = [item("abcdef0123456789", { confidence: "fuzzy" }), item("1111111222", { visible: false })];
  it("matches by sha prefix, either direction; no match = no tier", () => {
    expect(tierFor(matchCodeItem("abcdef0", items))).toBe("fuzzy");
    expect(tierFor(matchCodeItem("abcdef0123456789aaaa", items))).toBe("fuzzy");
    expect(tierFor(matchCodeItem("ffffff0", items))).toBeNull();
    expect(tierFor(matchCodeItem("abc", items))).toBeNull();
    expect(tierFor(matchCodeItem(undefined, items))).toBeNull();
    expect(tierFor(matchCodeItem("abcdef0", undefined))).toBeNull();
  });
  it("commit door: null without code_url or when not visible; else commitHref", () => {
    const hit = matchCodeItem("abcdef0", items);
    expect(commitDoor(null, hit)).toBeNull();
    expect(commitDoor("https://code.test", null)).toBeNull();
    expect(commitDoor("https://code.test", matchCodeItem("1111111", items))).toBeNull();
    expect(commitDoor("https://code.test/", hit)).toBe(commitHref("https://code.test/", "r", "abcdef0123456789"));
  });
  it("file door equals codeSearchUrl and is null without code_url", () => {
    expect(fileDoor(null, "a/b.ts")).toBeNull();
    expect(fileDoor("https://code.test", "a/b.ts")).toBe(codeSearchUrl("https://code.test", undefined, "a/b.ts"));
  });
  it("session page href encodes the sid", () => {
    expect(sessionPageHref("a b")).toBe("/sessions/a%20b");
  });
});

describe("capCommits", () => {
  const many = Array.from({ length: 25 }, (_, i) => i);
  it("caps at 20 until show-all", () => {
    expect(COMMIT_CAP).toBe(20);
    expect(capCommits(many, false)).toHaveLength(20);
    expect(capCommits(many, true)).toHaveLength(25);
    expect(capCommits(many.slice(0, 5), false)).toHaveLength(5);
  });
});

describe("freshnessText", () => {
  it("scales and never claims liveness", () => {
    const now = 1_700_000_000_000;
    expect(freshnessText(1_700_000_000 - 300, now)).toBe("As of last capture, 5m ago");
    expect(freshnessText(1_700_000_000 - 5, now)).toBe("As of last capture, just now");
    expect(freshnessText(0, now)).toBe("Capture time unknown");
    expect(freshnessText(undefined, now)).toBe("Capture time unknown");
  });
});
