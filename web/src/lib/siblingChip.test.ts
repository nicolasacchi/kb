import { describe, expect, it } from "vitest";
import type {
  DocLensOut,
  DocLensScorecardOut,
  ResolvedCodeRef,
  ScorecardRepoRow,
} from "../api/doclens";
import type { CodeRefGroupOut } from "../api/generated/CodeRefGroupOut";
import type { CodeRefOut } from "../api/generated/CodeRefOut";
import {
  httpCodeUrl,
  orderCandidates,
  siblingChipState,
  type SiblingChipInput,
} from "./siblingChip";

const CODE = "https://code.example.test";

function ref(o: Partial<CodeRefOut> & { ordinal: number }): CodeRefOut {
  return {
    group: null,
    kind: "path_line",
    raw: "raw",
    path_hint: null,
    line_start: null,
    line_end: null,
    line_spans: null,
    symbol_container: null,
    symbol_member: null,
    context: "",
    context_tokens: [],
    declared: false,
    ...o,
  };
}

function group(key: string, ordinal: number): CodeRefGroupOut {
  return { ordinal, key, label: key, anchor: key };
}

function lensRef(o: Partial<ResolvedCodeRef> & { ordinal: number }): ResolvedCodeRef {
  return {
    group: null,
    kind: "path_line",
    raw: "raw",
    declared: false,
    path_hint: null,
    line_hint: null,
    line_hint_end: null,
    symbol_container: null,
    symbol_member: null,
    context: null,
    path_state: "present",
    resolved_path: null,
    candidate_count: 0,
    candidates: [],
    issue: null,
    line_state: "unverifiable",
    line_evidence: "none",
    confirm_token: null,
    token_line: null,
    resolved_line: null,
    line_hint_delta: null,
    file_lines: 0,
    line_reason: null,
    spans: [],
    symbol_state: "no_symbol",
    symbol_hit_count: 0,
    symbol_hits: [],
    reader: null,
    search: null,
    note: null,
    ...o,
  };
}

function lens(refs: ResolvedCodeRef[], drifted = 0): DocLensOut {
  return {
    schema: "codelens/1",
    kb: "k",
    doc_id: "d",
    moved_from: null,
    doc_path: null,
    doc_href: null,
    doc_hash: null,
    doc_title: null,
    doc_extracted_at: null,
    doc_code_rev: null,
    never_scanned: false,
    repo: {
      name: "app",
      root: "/r",
      state: "ready",
      head_sha: null,
      head_branch: null,
      dirty: null,
      source: "param",
    },
    resolved_unix: 1_800_000_000,
    truncated: false,
    partial: false,
    partial_reason: null,
    counts: {
      total: refs.length,
      resolved: refs.length,
      present: refs.length,
      ambiguous: 0,
      absent: 0,
      external: 0,
      confirmed: 0,
      drifted,
      unverifiable: 0,
      line_absent: 0,
      declared_but_absent: 0,
    },
    ungrouped_count: 0,
    groups: [],
    refs,
    note: "",
  };
}

function row(name: string, present: number | null): ScorecardRepoRow {
  return {
    name,
    root: `/r/${name}`,
    state: present === null ? "indexing" : "ready",
    head_sha: null,
    head_branch: null,
    dirty: null,
    present,
    ambiguous: present === null ? null : 0,
    absent: present === null ? null : 0,
    external: present === null ? null : 0,
    partial: false,
    reason: null,
  };
}

function scorecard(repos: ScorecardRepoRow[], pinned: string | null = null): DocLensScorecardOut {
  return {
    schema: "codelens-scorecard/1",
    kb: "k",
    doc_id: "d",
    doc_hash: null,
    doc_title: null,
    never_scanned: false,
    resolved_unix: 1_800_000_000,
    pinned_repo: pinned,
    counted_refs: 2,
    truncated: false,
    repos,
    note: "",
  };
}

const REFS = [
  ref({ ordinal: 0, group: "intro", path_hint: "src/a.rs", line_start: 10 }),
  ref({ ordinal: 1, group: "design", path_hint: "crates/kb-core/src/coderefs.rs", line_start: 120 }),
  ref({ ordinal: 2, group: "design", path_hint: "src/b.rs", line_start: 5 }),
];
const GROUPS = [group("intro", 0), group("design", 1)];

function input(o: Partial<SiblingChipInput> = {}): SiblingChipInput {
  return {
    codeUrl: CODE,
    neverScanned: false,
    refCount: 3,
    extractedAt: 1_800_000_000,
    truncated: false,
    refs: REFS,
    groups: GROUPS,
    sec: null,
    repoParam: null,
    scorecard: undefined,
    lens: undefined,
    crossErr: null,
    nowMs: 1_800_000_100_000,
    ...o,
  };
}

describe("httpCodeUrl", () => {
  it("accepts http(s) only", () => {
    expect(httpCodeUrl(CODE)).toBe(CODE);
    expect(httpCodeUrl("javascript:alert(1)")).toBeNull();
    expect(httpCodeUrl("/relative")).toBeNull();
    expect(httpCodeUrl(null)).toBeNull();
  });
});

describe("orderCandidates", () => {
  it("prefers the group whose heading is the live section", () => {
    const o = orderCandidates(REFS, GROUPS, "design");
    expect(o.map((r) => r.ordinal)).toEqual([1, 2, 0]);
  });

  it("falls back to the nearest ref-bearing heading above a ref-less one", () => {
    const toc = ["intro", "design", "design-detail", "appendix"];
    const o = orderCandidates(REFS, GROUPS, "design-detail", toc);
    expect(o[0].ordinal).toBe(1);
  });

  it("is ordinal order with no section, and drops issue/external/hintless refs", () => {
    const mixed = [
      ref({ ordinal: 3, kind: "issue", path_hint: "o/r", line_start: 9 }),
      ref({ ordinal: 4, kind: "external", path_hint: "gem/x.rb" }),
      ref({ ordinal: 5, path_hint: null }),
      ...REFS,
    ];
    expect(orderCandidates(mixed, GROUPS, null).map((r) => r.ordinal)).toEqual([0, 1, 2]);
  });
});

describe("siblingChipState - none", () => {
  it("is none without a valid http(s) code_url", () => {
    expect(siblingChipState(input({ codeUrl: null })).kind).toBe("none");
    expect(siblingChipState(input({ codeUrl: "javascript:alert(1)" })).kind).toBe("none");
  });
  it("is none for zero refs and for never_scanned", () => {
    expect(siblingChipState(input({ refCount: 0, refs: [] })).kind).toBe("none");
    expect(siblingChipState(input({ neverScanned: true })).kind).toBe("none");
  });
});

describe("siblingChipState - hints", () => {
  it("no repo chosen: destination label, kb-code search link, 'no repo chosen' in the title", () => {
    const s = siblingChipState(input());
    expect(s.kind).toBe("hints");
    expect(s.label).toBe("→ search a.rs");
    expect(s.primary).toEqual({
      kind: "link",
      href: `${CODE}/search?q=src%2Fa.rs`,
    });
    expect(s.title).toContain("cannot check them against the code");
    expect(s.title).toContain("no repo chosen");
  });

  it("a repo from ?repo= builds the reader link at the live section's ref", () => {
    const s = siblingChipState(input({ sec: "design", repoParam: "my repo" }));
    expect(s.label).toBe("→ coderefs.rs:120");
    expect(s.primary).toEqual({
      kind: "link",
      href: `${CODE}/r/my%20repo/crates/kb-core/src/coderefs.rs?line=120`,
    });
    expect(s.kind).toBe("hints");
  });

  it("the doc's pin is used when ?repo= is absent, and is never auto-selected otherwise", () => {
    const pinned = scorecard([row("app", 2), row("web", 1)], "web");
    const s = siblingChipState(input({ scorecard: pinned }));
    expect(s.primary).toEqual({ kind: "link", href: `${CODE}/r/web/src/a.rs?line=10` });
    const unpinned = scorecard([row("app", 2), row("web", 1)]);
    const u = siblingChipState(input({ scorecard: unpinned }));
    expect(u.primary).toEqual({ kind: "link", href: `${CODE}/search?q=src%2Fa.rs` });
  });

  it("exactly one checkout present and the rest 0: reader link captioned as not pinned", () => {
    const sc = scorecard([row("app", 3), row("web", 0), row("old", 0)]);
    const s = siblingChipState(input({ scorecard: sc }));
    expect(s.primary).toEqual({ kind: "link", href: `${CODE}/r/app/src/a.rs?line=10` });
    expect(s.title).toContain("present in 1 of 3 checkouts (not pinned)");
    expect(s.kind).toBe("hints");
  });

  it("two present checkouts, or an unmeasured one, fall back to search", () => {
    for (const repos of [
      [row("app", 3), row("web", 1)],
      [row("app", 3), row("web", null)],
    ]) {
      const s = siblingChipState(input({ scorecard: scorecard(repos) }));
      expect(s.primary).toEqual({ kind: "link", href: `${CODE}/search?q=src%2Fa.rs` });
    }
  });

  it("only issue refs: the target is the Links tab", () => {
    const only = [ref({ ordinal: 0, kind: "issue", path_hint: "o/r", line_start: 4 })];
    const s = siblingChipState(input({ refs: only, groups: [], refCount: 1 }));
    expect(s.primary).toEqual({ kind: "inspect" });
    expect(s.label).toBe("1 code ref");
  });

  it("renders N+ when the extraction was truncated", () => {
    const only = [ref({ ordinal: 0, kind: "issue", path_hint: "o/r", line_start: 4 })];
    const s = siblingChipState(input({ refs: only, groups: [], refCount: 50, truncated: true }));
    expect(s.label).toBe("50+ code refs");
  });
});

describe("siblingChipState - computed", () => {
  const r = "repo-a";
  it("verified: the lens reader wins, joined by ordinal, never by raw text", () => {
    const l = lens([
      lensRef({
        ordinal: 1,
        raw: "something else entirely",
        path_state: "present",
        reader: { repo: r, path: "crates/kb-core/src/coderefs.rs", line: 130 },
      }),
    ]);
    const s = siblingChipState(input({ sec: "design", repoParam: r, lens: l }));
    expect(s.kind).toBe("verified");
    expect(s.label).toBe("→ coderefs.rs:130");
    expect(s.primary).toEqual({
      kind: "link",
      href: `${CODE}/r/repo-a/crates/kb-core/src/coderefs.rs?line=130`,
    });
  });

  it("drifted adds the moved count to the label and a warn tone", () => {
    const l = lens(
      [lensRef({ ordinal: 0, reader: { repo: r, path: "src/a.rs", line: 12 } })],
      2,
    );
    const s = siblingChipState(input({ repoParam: r, lens: l }));
    expect(s.kind).toBe("drifted");
    expect(s.label).toBe("→ a.rs:12 · 2 moved");
    expect(s.tone).toBe("warn");
  });

  it("absent refs are skipped as primary; ambiguous goes to search", () => {
    const l = lens([
      lensRef({ ordinal: 0, path_state: "absent" }),
      lensRef({
        ordinal: 1,
        path_state: "ambiguous",
        search: { q: "coderefs.rs", repo: r },
      }),
    ]);
    const s = siblingChipState(input({ repoParam: r, lens: l }));
    expect(s.primary).toEqual({
      kind: "link",
      href: `${CODE}/search?q=coderefs.rs&repo=repo-a`,
    });
  });

  it("every ref absent: inspect", () => {
    const l = lens([
      lensRef({ ordinal: 0, path_state: "absent" }),
      lensRef({ ordinal: 1, path_state: "absent" }),
      lensRef({ ordinal: 2, path_state: "external" }),
    ]);
    const s = siblingChipState(input({ repoParam: r, lens: l }));
    expect(s.primary).toEqual({ kind: "inspect" });
  });
});

describe("siblingChipState - never fake-fresh", () => {
  const failures: [string, unknown][] = [
    ["TypeError (CORS / down)", new TypeError("Failed to fetch")],
    ["AbortError (timeout)", new DOMException("aborted", "AbortError")],
    ["plain Error, no reason", new Error("boom")],
    ["undefined-ish string", "network"],
  ];
  for (const [name, err] of failures) {
    it(`${name} is degraded, never verified, even with a stale lens passed in`, () => {
      const s = siblingChipState(
        input({
          repoParam: "repo-a",
          crossErr: err,
          lens: lens([lensRef({ ordinal: 0, reader: { repo: "repo-a", path: "x.rs", line: 1 } })]),
        }),
      );
      expect(s.kind).toBe("degraded");
      expect(s.kind).not.toBe("verified");
      expect(s.title).toContain("cannot check them against the code");
    });
  }

  it("a server-authored refusal (.reason) is a refusal, not a verdict", () => {
    const e = Object.assign(new Error("repo is indexing"), { reason: "repo_indexing" });
    const s = siblingChipState(input({ repoParam: "repo-a", crossErr: e }));
    expect(s.kind).toBe("refusal");
    expect(s.title).toContain("repo is indexing");
  });

  it("no lens payload (loading) stays hints", () => {
    expect(siblingChipState(input({ repoParam: "repo-a", lens: undefined })).kind).toBe("hints");
  });
});
