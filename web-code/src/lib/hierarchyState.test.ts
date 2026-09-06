import { describe, expect, it } from "vitest";
import {
  buildCalleesTree,
  buildCallersTree,
  buildTypesTree,
  flattenTree,
  HIERARCHY_DEPTH_CAP,
  hierarchyReducer,
  initialHierarchyState,
  isTypeIshKind,
  locKey,
  type HierarchyNode,
} from "./hierarchyState";
import type {
  HierarchyCalleesOut,
  HierarchyCallersOut,
  HierarchyTypesOut,
} from "../api/types";

describe("isTypeIshKind", () => {
  it("accepts common type kinds", () => {
    expect(isTypeIshKind("trait")).toBe(true);
    expect(isTypeIshKind("class")).toBe(true);
    expect(isTypeIshKind("interface")).toBe(true);
    expect(isTypeIshKind("struct")).toBe(true);
    expect(isTypeIshKind("function")).toBe(false);
    expect(isTypeIshKind(null)).toBe(false);
  });
});

describe("buildCallersTree", () => {
  it("builds a root + caller rows with class badges", () => {
    const out: HierarchyCallersOut = {
      schema: "hierarchy/1",
      function: { name: "paint", kind: "function", path: "a.rs", line: 1 },
      callers: [
        {
          path: "b.rs",
          enclosing: { name: "draw", kind: "function", line: 10 },
          sites: [{ line: 12, col: 4, class: "likely" }],
        },
      ],
      truncated: false,
    };
    const roots = buildCallersTree(out);
    expect(roots).toHaveLength(1);
    expect(roots[0].name).toBe("paint");
    expect(roots[0].expanded).toBe(true);
    expect(roots[0].children).toHaveLength(1);
    expect(roots[0].children[0].name).toBe("draw");
    expect(roots[0].children[0].class).toBe("likely");
    const flat = flattenTree(roots);
    expect(flat.map((n) => n.name)).toEqual(["paint", "draw"]);
  });

  it("appends a truncation sentinel when truncated", () => {
    const out: HierarchyCallersOut = {
      schema: "hierarchy/1",
      function: { name: "f", kind: "function", path: "a.rs", line: 1 },
      callers: [],
      truncated: true,
    };
    const roots = buildCallersTree(out);
    expect(roots[0].children.some((c) => c.truncatedNote)).toBe(true);
  });
});

describe("buildCalleesTree", () => {
  it("maps callees with class", () => {
    const out: HierarchyCalleesOut = {
      schema: "hierarchy/1",
      function: { name: "use_it", kind: "function", path: "a.rs", line: 1 },
      callees: [
        {
          name: "paint",
          line: 3,
          col: 4,
          class: "exact",
          target: { path: "a.rs", line: 10, class: "exact", precision: "locals" },
        },
      ],
    };
    const roots = buildCalleesTree(out);
    expect(roots[0].children[0].name).toBe("paint");
    expect(roots[0].children[0].class).toBe("exact");
    expect(roots[0].children[0].line).toBe(10);
  });
});

describe("buildTypesTree", () => {
  it("groups supers and subs under section headers", () => {
    const out: HierarchyTypesOut = {
      schema: "hierarchy/1",
      name: "Drawable",
      supertypes: [],
      subtypes: [
        {
          name: "Circle",
          kind: "struct",
          via: { path: "shapes.rs", line: 20 },
          class: "likely",
          target: { path: "shapes.rs", line: 20 },
        },
      ],
    };
    const roots = buildTypesTree(out, "shapes.rs");
    expect(roots[0].name).toBe("Drawable");
    expect(roots[0].children[0].name).toMatch(/Subtypes/);
    expect(roots[0].children[0].children[0].name).toBe("Circle");
    expect(roots[0].children[0].children[0].class).toBe("likely");
  });
});

describe("hierarchyReducer", () => {
  it("opens loading then sets tree", () => {
    let s = hierarchyReducer(initialHierarchyState, {
      type: "OPEN",
      mode: "callers",
      title: "f",
    });
    expect(s.open).toBe(true);
    expect(s.loading).toBe(true);
    const root: HierarchyNode = {
      id: "r",
      name: "f",
      path: "a.rs",
      line: 1,
      col: 0,
      class: "exact",
      depth: 0,
      dir: "callers",
      cycle: false,
      depthCapped: false,
      expanded: true,
      loading: false,
      children: [],
    };
    s = hierarchyReducer(s, { type: "SET_TREE", roots: [root] });
    expect(s.loading).toBe(false);
    expect(s.flat).toHaveLength(1);
  });
});

describe("locKey / depth cap", () => {
  it("keys path:line", () => {
    expect(locKey("a.rs", 3)).toBe("a.rs:3");
  });
  it("depth cap is 5", () => {
    expect(HIERARCHY_DEPTH_CAP).toBe(5);
  });
});
