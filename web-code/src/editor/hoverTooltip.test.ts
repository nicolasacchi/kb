// V70-A6 — the hover tooltip's pure DOM builder (`renderHoverDom`), exercised
// "without a browser" per this module's own doc.
//
// vitest.config.ts runs in `environment: "node"` (no jsdom — see
// `hooks/useIsMobile.test.ts`'s comment on this crate's convention), so
// `document` does not exist as a global here. `renderHoverDom` only calls
// `document.createElement`/`appendChild`/`setAttribute` and reads/writes a
// handful of DOM properties, so a minimal duck-typed `FakeElement` — the
// same "mock only what is read" discipline `lib/isTypingTarget.test.ts` uses
// — is enough to drive it and inspect the tree it builds. `hoverTooltipExtension`
// itself (the CM6 `hoverTooltip`/`domEventHandlers` wiring, the fetch cache,
// the Ctrl/Cmd-click discrimination) needs a real editor view and is left to
// `e2e/hover.spec.ts`.
import { beforeEach, describe, expect, it } from "vitest";
import { renderHoverDom } from "./hoverTooltip";
import type { HoverOut } from "../api/types";

class FakeElement {
  readonly tagName: string;
  className = "";
  textContent = "";
  title = "";
  private readonly attrs = new Map<string, string>();
  readonly children: FakeElement[] = [];
  constructor(tagName: string) {
    this.tagName = tagName;
  }
  setAttribute(name: string, value: string): void {
    this.attrs.set(name, value);
  }
  getAttribute(name: string): string | null {
    return this.attrs.get(name) ?? null;
  }
  appendChild(child: FakeElement): FakeElement {
    this.children.push(child);
    return child;
  }
  get childNodes(): FakeElement[] {
    return this.children;
  }
}

/// First node (self or descendant) whose className carries `cls` as a
/// SPACE-JOINED token — the trust badge's class is `"kbc-hovertip__trust
/// kbc-trust-exact"`, so this checks token membership, not equality.
function find(root: FakeElement, cls: string): FakeElement | null {
  if (root.className.split(" ").includes(cls)) return root;
  for (const c of root.children) {
    const hit = find(c, cls);
    if (hit) return hit;
  }
  return null;
}

beforeEach(() => {
  (globalThis as unknown as { document: unknown }).document = {
    createElement: (tag: string) => new FakeElement(tag),
  };
});

function symbolOut(extra?: Partial<HoverOut>): HoverOut {
  return {
    schema: "hover/1",
    path: "app/models/order.rb",
    line: 88,
    col: 4,
    precision: "exact",
    trust: "exact",
    symbol: {
      kind: "method",
      name: "total",
      container: "Order",
      signature: "def total(self)",
      doc: null,
    },
    defsite: null,
    framework: null,
    ...extra,
  };
}

describe("renderHoverDom — the honest empty (rule 2)", () => {
  it("out === null renders the word with no claim of understanding it", () => {
    const root = renderHoverDom(null, "foo") as unknown as FakeElement;
    expect(root.getAttribute("data-kbc-hovertip")).toBe("");
    const sig = find(root, "kbc-hovertip__sig");
    expect(sig?.textContent).toBe("foo");
    const empty = find(root, "kbc-hovertip__empty");
    expect(empty?.textContent).toBe("nothing in this repo resolves this identifier");
  });

  it("a resolve with no symbol/defsite/framework is ALSO the honest empty, not a crash", () => {
    const out: HoverOut = {
      schema: "hover/1",
      path: "a.rs",
      line: 1,
      col: 0,
      precision: null,
      trust: null,
      symbol: null,
      defsite: null,
      framework: null,
    };
    const root = renderHoverDom(out, "bar") as unknown as FakeElement;
    expect(find(root, "kbc-hovertip__empty")?.textContent).toBe(
      "nothing in this repo resolves this identifier",
    );
  });

  it("the empty branch never appends the keys hint — there is nothing to act on", () => {
    const root = renderHoverDom(null, "foo") as unknown as FakeElement;
    expect(find(root, "kbc-hovertip__keys")).toBeNull();
  });
});

describe("renderHoverDom — a resolved symbol", () => {
  it("prefers signature, then name, then the raw word", () => {
    const withSig = renderHoverDom(symbolOut(), "total") as unknown as FakeElement;
    expect(find(withSig, "kbc-hovertip__sig")?.textContent).toBe("def total(self)");

    const noSig = renderHoverDom(
      symbolOut({ symbol: { ...symbolOut().symbol!, signature: null } }),
      "total",
    ) as unknown as FakeElement;
    expect(find(noSig, "kbc-hovertip__sig")?.textContent).toBe("total");

    const noSymbolAtAll = renderHoverDom(
      symbolOut({ symbol: null, defsite: { path: "a.rs", line: 1 } }),
      "raw_word",
    ) as unknown as FakeElement;
    expect(find(noSymbolAtAll, "kbc-hovertip__sig")?.textContent).toBe("raw_word");
  });

  it("renders kind + container in the meta row", () => {
    const root = renderHoverDom(symbolOut(), "total") as unknown as FakeElement;
    expect(find(root, "kbc-hovertip__kind")?.textContent).toBe("method");
    expect(find(root, "kbc-hovertip__container")?.textContent).toBe("in Order");
  });

  it("omits kind/container the symbol did not carry, without inventing either", () => {
    const root = renderHoverDom(
      symbolOut({ symbol: { kind: "", name: "total", container: null, signature: "x", doc: null } }),
      "total",
    ) as unknown as FakeElement;
    expect(find(root, "kbc-hovertip__kind")).toBeNull();
    expect(find(root, "kbc-hovertip__container")).toBeNull();
  });

  it("truncates a long doc to 6 lines", () => {
    const doc = Array.from({ length: 10 }, (_, i) => `line ${i}`).join("\n");
    const root = renderHoverDom(
      symbolOut({ symbol: { ...symbolOut().symbol!, doc } }),
      "total",
    ) as unknown as FakeElement;
    const p = find(root, "kbc-hovertip__doc");
    expect(p?.textContent).toBe(["line 0", "line 1", "line 2", "line 3", "line 4", "line 5"].join("\n"));
  });

  it("appends the keys hint on every non-empty render", () => {
    const root = renderHoverDom(symbolOut(), "total") as unknown as FakeElement;
    expect(find(root, "kbc-hovertip__keys")?.textContent).toBe(
      "Ctrl-click to go to the definition · K for the full card",
    );
  });
});

describe("renderHoverDom — trust is shown, never implied (rule 3)", () => {
  it("a trust class renders a badge carrying BOTH the class and the data attribute", () => {
    const root = renderHoverDom(symbolOut({ trust: "likely", precision: "definition" }), "total") as unknown as FakeElement;
    const badge = find(root, "kbc-trust-likely");
    expect(badge).not.toBeNull();
    expect(badge?.className).toContain("kbc-hovertip__trust");
    expect(badge?.getAttribute("data-kbc-hovertip-trust")).toBe("likely");
    expect(badge?.textContent).toBe("likely");
    // the badge's title carries the finer-grained precision, on hover
    expect(badge?.title).toBe("definition");
  });

  it("trust: null renders NO badge — absent, never defaulted", () => {
    const root = renderHoverDom(symbolOut({ trust: null }), "total") as unknown as FakeElement;
    expect(find(root, "kbc-hovertip__trust")).toBeNull();
  });

  it("precision: 'lsp-live' adds the live badge alongside trust", () => {
    const root = renderHoverDom(
      symbolOut({ trust: "exact", precision: "lsp-live" }),
      "total",
    ) as unknown as FakeElement;
    expect(find(root, "kbc-hovertip__live")?.textContent).toBe("live");
  });

  it("a symbol with no trust and no kind/container renders NO meta row at all", () => {
    const root = renderHoverDom(
      symbolOut({
        trust: null,
        precision: null,
        symbol: { kind: "", name: "total", container: null, signature: "def total", doc: null },
      }),
      "total",
    ) as unknown as FakeElement;
    expect(find(root, "kbc-hovertip__meta")).toBeNull();
  });
});

describe("renderHoverDom — ONE provenance line, never both stacked", () => {
  it("prefers defsite over a framework edge when both are present", () => {
    const root = renderHoverDom(
      symbolOut({
        defsite: { path: "app/models/order.rb", line: 42 },
        framework: { kind: "association", dst_kind: "model", dst_path: "app/models/line_item.rb", trust: "likely" },
      }),
      "total",
    ) as unknown as FakeElement;
    const prov = find(root, "kbc-hovertip__prov");
    expect(prov?.textContent).toBe("defined at app/models/order.rb:42");
  });

  it("falls back to a framework edge when there is no defsite", () => {
    const root = renderHoverDom(
      symbolOut({
        defsite: null,
        framework: { kind: "association", dst_kind: "model", dst_path: "app/models/line_item.rb", trust: "likely" },
      }),
      "total",
    ) as unknown as FakeElement;
    const prov = find(root, "kbc-hovertip__prov");
    expect(prov?.textContent).toBe("association → app/models/line_item.rb (likely)");
  });

  it("a framework edge with no dst_path renders no provenance line at all", () => {
    const root = renderHoverDom(
      symbolOut({
        defsite: null,
        framework: { kind: "association", dst_kind: null, dst_path: null, trust: "candidate" },
      }),
      "total",
    ) as unknown as FakeElement;
    expect(find(root, "kbc-hovertip__prov")).toBeNull();
  });

  it("neither defsite nor a resolvable framework: no provenance line", () => {
    const root = renderHoverDom(symbolOut({ defsite: null, framework: null }), "total") as unknown as FakeElement;
    expect(find(root, "kbc-hovertip__prov")).toBeNull();
  });
});
