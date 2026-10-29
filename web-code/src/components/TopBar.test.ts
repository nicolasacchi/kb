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
    ...o,
  };
}

function refs(claims: DocRefClaim[]): { data: DocRefsOut } {
  return { data: { schema: "doc-refs/1", repo: "x", path: "y.rs", live: true, claims } };
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
});
