// V72-I2 — the Rails atom table behind the reader's hover card.
//
// Two properties matter more than any single case: the module picks edges by
// their own `src_line` and NEVER resolves anything, and nothing it produces
// can auto-navigate (D5 — rails-lens/1 has no exact tier, by construction).
import { describe, expect, it } from "vitest";
import type { FrameworkEdgeOut } from "../api/types";
import {
  actionsTargetOf,
  atomKindLabel,
  atomsForLine,
  isRouteHelper,
  neverAutoNavigates,
  routeHelperAtom,
} from "./railsAtoms";

const REPO = "acme-app";
const SRC = "app/models/order.rb";

function edge(p: Partial<FrameworkEdgeOut>): FrameworkEdgeOut {
  return {
    kind: "association",
    src_path: SRC,
    src_line: 6,
    src_symbol: null,
    dst_kind: null,
    dst_path: null,
    dst_symbol: null,
    trust: "likely",
    extra_json: null,
    direction: "src",
    ...p,
  };
}

const EDGES: FrameworkEdgeOut[] = [
  // `has_many :line_items` on line 6.
  edge({ kind: "association", src_line: 6, dst_path: "app/models/line_item.rb", dst_symbol: "LineItem" }),
  // A second association on the same line (`has_many :a, :b` style).
  edge({ kind: "association", src_line: 6, dst_path: "app/models/audit_entry.rb", dst_symbol: "AuditEntry", trust: "candidate" }),
  // `render "row"` in a view, line 2.
  edge({
    kind: "render_partial",
    src_path: "app/views/orders/index.html.erb",
    src_line: 2,
    dst_path: "app/views/orders/_row.html.erb",
  }),
  // `t("orders.index.title")`, line 1.
  edge({
    kind: "i18n_key",
    src_path: "app/views/orders/index.html.erb",
    src_line: 1,
    dst_path: "config/locales/en.yml",
    dst_symbol: "orders.index.title",
  }),
  // `ExportJob.perform_later`, line 10.
  edge({ kind: "job_enqueue", src_line: 10, dst_path: "app/jobs/export_job.rb", dst_symbol: "ExportJob" }),
  // A scope — a SYMBOL with no file. Absent, never guessed.
  edge({ kind: "scope", src_line: 8, dst_symbol: "recent" }),
  // An INBOUND edge on the same line: a fact about another file, not this cursor.
  edge({ kind: "concern_include", src_line: 6, direction: "dst", dst_path: "app/models/order.rb" }),
];

describe("atomsForLine", () => {
  it("returns nothing for a line with no edges, an unknown line, or no data", () => {
    expect(atomsForLine(REPO, EDGES, 99)).toEqual([]);
    expect(atomsForLine(REPO, EDGES, 0)).toEqual([]);
    expect(atomsForLine(REPO, undefined, 6)).toEqual([]);
  });

  it("groups the edges on ONE line by kind, keeping every target", () => {
    const atoms = atomsForLine(REPO, EDGES, 6);
    expect(atoms).toHaveLength(1);
    expect(atoms[0].kind).toBe("association");
    expect(atoms[0].label).toBe("association");
    expect(atoms[0].targets.map((t) => t.label)).toEqual(["LineItem", "AuditEntry"]);
  });

  it("ignores INBOUND edges — this cursor is not where they were written", () => {
    const atoms = atomsForLine(REPO, EDGES, 6);
    expect(atoms.some((a) => a.kind === "concern_include")).toBe(false);
  });

  it("carries each target's OWN trust, as a line-style class", () => {
    const [assoc] = atomsForLine(REPO, EDGES, 6);
    expect(assoc.targets[0].tier).toBe("likely");
    expect(assoc.targets[0].trustClass).toBe("kbc-trust-likely");
    expect(assoc.targets[1].tier).toBe("candidate");
    expect(assoc.targets[1].trustClass).toBe("kbc-trust-candidate");
  });

  it("never mints exact, even for a forged wire value", () => {
    const forged = [edge({ src_line: 3, dst_path: "a.rb", trust: "exact" })];
    const [atom] = atomsForLine(REPO, forged, 3);
    expect(atom.targets[0].tier).toBe("candidate");
    expect(atom.targets[0].trustClass).toBe("kbc-trust-candidate");
  });

  it("builds each addressable target's href through codeUrl", () => {
    const [render] = atomsForLine(REPO, EDGES, 2);
    expect(render.kind).toBe("render_partial");
    expect(render.label).toBe("renders partial");
    expect(render.targets[0].href).toBe(`/r/${REPO}/app/views/orders/_row.html.erb`);
  });

  it("leaves a symbol-only edge UNADDRESSED rather than guessing a file", () => {
    const [scope] = atomsForLine(REPO, EDGES, 8);
    expect(scope.targets[0].label).toBe("recent");
    expect(scope.targets[0].href).toBeNull();
    expect(actionsTargetOf(scope)).toBeNull();
  });

  it("labels an i18n key by its own dst_symbol, not the YAML path", () => {
    const [i18n] = atomsForLine(REPO, EDGES, 1);
    expect(i18n.label).toBe("translation key");
    expect(i18n.targets[0].label).toBe("orders.index.title");
    expect(i18n.targets[0].path).toBe("config/locales/en.yml");
  });

  it("renders an unknown edge kind as ITSELF", () => {
    expect(atomKindLabel("job_enqueue")).toBe("enqueues job");
    expect(atomKindLabel("some_future_kind")).toBe("some_future_kind");
    const [atom] = atomsForLine(REPO, [edge({ kind: "some_future_kind", src_line: 4, dst_path: "x.rb" })], 4);
    expect(atom.label).toBe("some_future_kind");
  });

  it("asks /api/actions about the first target that has a FILE", () => {
    const [job] = atomsForLine(REPO, EDGES, 10);
    expect(actionsTargetOf(job)).toEqual({ path: "app/jobs/export_job.rb", line: 1 });
  });

  it("gives each atom a stable id", () => {
    expect(atomsForLine(REPO, EDGES, 6)[0].id).toBe("association@6");
  });
});

describe("route helpers are a SEARCH, never a resolved target", () => {
  it("recognises the `_path`/`_url` shape and nothing else", () => {
    for (const w of ["orders_path", "edit_order_url", "admin_reports_path"]) {
      expect(isRouteHelper(w)).toBe(true);
    }
    for (const w of ["Order", "path", "orders", "_path", "orders_pathx", "OrdersPath"]) {
      expect(isRouteHelper(w)).toBe(false);
    }
  });

  it("produces a kbcq/1 route: clause over the stem, with NO targets", () => {
    const atom = routeHelperAtom("edit_order_url", 12)!;
    expect(atom.source).toBe("search");
    expect(atom.clause).toBe("route:edit_order");
    expect(atom.targets).toEqual([]);
    expect(atom.note).toContain("no route-helper edge");
  });

  it("returns null for a word that is not one", () => {
    expect(routeHelperAtom("Order", 1)).toBeNull();
  });
});

describe("D5 — nothing on this card auto-navigates", () => {
  it("holds for a lens atom at every tier and for a search atom", () => {
    const atoms = [
      ...atomsForLine(REPO, EDGES, 6),
      ...atomsForLine(REPO, EDGES, 2),
      routeHelperAtom("orders_path", 3)!,
    ];
    for (const atom of atoms) {
      const auto: false = neverAutoNavigates(atom);
      expect(auto).toBe(false);
    }
  });
});
