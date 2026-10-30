// @vitest-environment jsdom
// OL-SIBL1 - the TopBar's Spine `crossing` slot: the Cited-by chip appears on
// a reader file route with claims, is absent elsewhere and with zero claims,
// and its open-strip mode expands CitedBy.
import { afterEach, describe, expect, it, vi } from "vitest";
import { createElement, Fragment } from "react";
import { createRoot, type Root } from "react-dom/client";
import { act } from "react-dom/test-utils";
import { MemoryRouter } from "react-router";
import type { DocRefClaim, DocRefsOut } from "../api/types";

type LiveLookup = DocRefsOut["live_lookup"];

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

const h = vi.hoisted(() => ({ refs: null as null | { data: unknown } }));

vi.mock("./RepoPill", () => ({ default: () => null }));
vi.mock("./RefChip", () => ({ default: () => null }));
vi.mock("./CurrentReviewChip", () => ({ default: () => null }));
vi.mock("./NavSheet", () => ({ default: () => null }));
vi.mock("./trail/TrailIndicator", () => ({ default: () => null }));
vi.mock("../commands/learn", () => ({ SpaceHint: () => null }));
vi.mock("../hooks/useActiveRepo", () => ({ useExplicitRepo: () => "x" }));
vi.mock("../hooks/useRepos", () => ({ useRepos: () => ({ data: { repos: [{ name: "x" }] } }) }));
vi.mock("../hooks/useIsMobile", () => ({ useIsMobile: () => false }));
vi.mock("../hooks/useDocRefs", () => ({ useDocRefs: (repo?: string, path?: string) =>
    repo && path ? (h.refs ?? { data: undefined }) : { data: undefined } }));

import TopBar from "./TopBar";
import CitedBy from "./lens/CitedBy";

function claim(o: Partial<DocRefClaim>): DocRefClaim {
  return {
    kb: "platform",
    doc_id: "doc1",
    doc_title: "Fixture Doc",
    doc_path: "docs/fixture-doc.html",
    doc_public_href: "https://kb.example/a/platform/docs/fixture-doc.html",
    group_anchor: "kb-h-a",
    doc_section_href: "https://kb.example/a/platform/docs/fixture-doc.html?sec=kb-h-a",
    group_label: "A",
    raw_hint: "y.rs:2",
    line_start: 2,
    line_end: 2,
    line_state: "confirmed",
    seen_at: 1,
    source: "pinned",
    ...o,
  };
}

const OK_LOOKUP: LiveLookup = { state: "ok", kbs: ["platform"], failed: [] };

function refs(claims: DocRefClaim[], live_lookup: LiveLookup = OK_LOOKUP): { data: DocRefsOut } {
  return { data: { schema: "doc-refs/1", repo: "x", path: "y.rs", live: true, claims, live_lookup } };
}

let root: Root | null = null;
let host: HTMLElement | null = null;

function mount(url: string) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => {
    root!.render(
      createElement(
        MemoryRouter,
        { initialEntries: [url] },
        createElement(Fragment, null, createElement(TopBar), createElement(CitedBy, { repo: "x", path: "y.rs" })),
      ),
    );
  });
  return host;
}

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = null;
  host = null;
  h.refs = null;
});

describe("TopBar crossing chip", () => {
  it("renders on a reader file route as a link to the section href", () => {
    h.refs = refs([claim({})]);
    const el = mount("/r/x/y.rs");
    const slot = el.querySelector("[data-kb-spine-slot=crossing]");
    expect(slot).not.toBeNull();
    const chip = slot!.querySelector<HTMLAnchorElement>("[data-testid=sibling-doc-chip]");
    expect(chip?.tagName).toBe("A");
    expect(chip?.getAttribute("href")).toBe(
      "https://kb.example/a/platform/docs/fixture-doc.html?sec=kb-h-a",
    );
  });

  it("is absent on /search even when claims exist", () => {
    h.refs = refs([claim({})]);
    const el = mount("/search");
    expect(el.querySelector("[data-testid=sibling-doc-chip]")).toBeNull();
    expect(el.querySelector("[data-kb-spine-slot=crossing]")).toBeNull();
  });

  it("is absent on a reader tool route (a ~segment is not a file)", () => {
    h.refs = refs([claim({})]);
    const el = mount("/r/x/~prs");
    expect(el.querySelector("[data-testid=sibling-doc-chip]")).toBeNull();
  });

  it("is absent with zero claims, and leaves no empty slot", () => {
    h.refs = refs([]);
    const el = mount("/r/x/y.rs");
    expect(el.querySelector("[data-testid=sibling-doc-chip]")).toBeNull();
    expect(el.querySelector("[data-kb-spine-slot=crossing]")).toBeNull();
  });

  it("open-strip (two docs) is a button that expands the Cited-by strip", () => {
    h.refs = refs([claim({}), claim({ doc_id: "doc4", doc_title: "Other" })]);
    const el = mount("/r/x/y.rs");
    const chip = el.querySelector<HTMLButtonElement>("[data-testid=sibling-doc-chip]");
    expect(chip?.tagName).toBe("BUTTON");
    expect(el.querySelector("[data-kbc-citedby-list]")).toBeNull();
    act(() => chip!.click());
    expect(el.querySelector("[data-kbc-citedby-list]")).not.toBeNull();
  });

  it("CitedBy rows link to the section href when the claim has one", () => {
    h.refs = refs([claim({})]);
    const el = mount("/r/x/y.rs");
    act(() => el.querySelector<HTMLButtonElement>("[data-kbc-citedby-toggle]")!.click());
    expect(el.querySelector("[data-kbc-citedby-link]")?.getAttribute("href")).toBe(
      "https://kb.example/a/platform/docs/fixture-doc.html?sec=kb-h-a",
    );
  });

  // OL-SIBL3 - three distinct absences, each its own muted line.
  it("zero claims after kb answered says no document cites the file", () => {
    h.refs = refs([]);
    const el = mount("/r/x/y.rs");
    const note = el.querySelector("[data-kbc-citedby-absence]");
    expect(note?.getAttribute("data-kbc-citedby-absence")).toBe("none");
    expect(note?.textContent).toBe("No document cites this file");
    expect(el.querySelector("[data-kbc-citedby]")).toBeNull();
  });

  it("zero claims with kb unreachable is not the same sentence", () => {
    h.refs = refs([], { state: "failed", kbs: ["platform"], failed: [{ kb: "platform", reason: "unreachable" }] });
    const el = mount("/r/x/y.rs");
    const note = el.querySelector("[data-kbc-citedby-absence]");
    expect(note?.getAttribute("data-kbc-citedby-absence")).toBe("unreachable");
    expect(note?.textContent).toContain("did not answer");
  });

  it("a sibling mismatch is named as such", () => {
    h.refs = refs([], { state: "failed", kbs: ["platform"], failed: [{ kb: "platform", reason: "sibling_mismatch" }] });
    const el = mount("/r/x/y.rs");
    expect(el.querySelector("[data-kbc-citedby-absence]")?.getAttribute("data-kbc-citedby-absence")).toBe("mismatch");
  });

  it("with the live lookup off the line says only pinned documents were checked", () => {
    h.refs = refs([], { state: "off", kbs: [], failed: [] });
    const el = mount("/r/x/y.rs");
    const note = el.querySelector("[data-kbc-citedby-absence]");
    expect(note?.getAttribute("data-kbc-citedby-absence")).toBe("no-pinned");
    expect(note?.textContent).toBe("No pinned document cites this file");
  });

  it("claims plus a failed kb keep the strip and add an incomplete-list note", () => {
    h.refs = refs([claim({})], { state: "partial", kbs: ["platform", "b"], failed: [{ kb: "b", reason: "timeout" }] });
    const el = mount("/r/x/y.rs");
    expect(el.querySelector("[data-kbc-citedby]")).not.toBeNull();
    expect(el.querySelector("[data-kbc-citedby-note]")?.textContent).toContain("may be incomplete");
  });

  it("a kb-live row is labelled unpinned; a pinned row is not", () => {
    h.refs = refs([claim({ source: "kb-live", line_state: null }), claim({ doc_id: "doc2", source: "pinned" })]);
    const el = mount("/r/x/y.rs");
    act(() => el.querySelector<HTMLButtonElement>("[data-kbc-citedby-toggle]")!.click());
    expect(el.querySelectorAll("[data-kbc-citedby-source=kb-live]").length).toBe(1);
  });
});
