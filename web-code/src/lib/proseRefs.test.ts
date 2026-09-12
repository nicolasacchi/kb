import { describe, expect, it } from "vitest";
import { codeUrl, entityUrl, findingUrl, symbolUrl } from "./codeUrl";
import {
  buildProseTree,
  markdownLinkRanges,
  overlayRun,
  parseInlineSourced,
  proseRefClass,
  proseRefHref,
  type OverlayNode,
  type ProseRef,
} from "./proseRefs";

const CTX = { repo: "acme-app", reviewId: 7 };

function refsOf(tree: ReturnType<typeof buildProseTree>): OverlayNode[] {
  const out: OverlayNode[] = [];
  for (const b of tree.blocks) {
    if (b.kind === "paragraph" || b.kind === "heading") out.push(...b.nodes.filter((n) => n.t === "ref"));
    else if (b.kind === "list") {
      for (const item of b.items) out.push(...item.filter((n) => n.t === "ref"));
    }
  }
  return out;
}

function pathRef(overrides: Partial<ProseRef> = {}): ProseRef {
  const text = "app/models/order.rb:41";
  return {
    kind: "path",
    span: { start: 0, end: text.length },
    text,
    path: "app/models/order.rb",
    line_start: 41,
    resolution: { state: "exact", path: "app/models/order.rb", line: 41 },
    ...overrides,
  };
}

describe("markdownLinkRanges", () => {
  it("finds a [text](dest) span", () => {
    const s = "see [app/foo.rb:1](https://example.com/foo) please";
    expect(markdownLinkRanges(s)).toEqual([[4, 43]]);
  });

  it("ignores a bare [not a link]", () => {
    expect(markdownLinkRanges("see [not a link] please")).toEqual([]);
  });
});

describe("proseRefHref", () => {
  it("path[:line] uses the Location Contract builder (codeUrl)", () => {
    const r = pathRef();
    expect(proseRefHref(r, CTX)).toBe(codeUrl({ repo: "acme-app", path: "app/models/order.rb", line: 41 }));
  });

  it("orphan is never a link", () => {
    const r = pathRef({ resolution: { state: "orphan", caption: "not in the mirror" } });
    expect(proseRefHref(r, CTX)).toBeNull();
  });

  it("finding: uses findingUrl", () => {
    const r: ProseRef = {
      kind: "finding",
      span: { start: 0, end: 15 },
      text: "f-double-charge",
      slug: "f-double-charge",
      resolution: { state: "exact" },
    };
    expect(proseRefHref(r, CTX)).toBe(findingUrl("acme-app", 7, "f-double-charge"));
  });

  it("symbol with ent uses entityUrl", () => {
    const r: ProseRef = {
      kind: "symbol",
      span: { start: 0, end: 21 },
      text: "Billing::InvoiceService",
      container: "Billing::InvoiceService",
      resolution: { state: "exact", ent: "Billing::InvoiceService", path: "app/services/invoice.rb", line: 3 },
    };
    expect(proseRefHref(r, CTX)).toBe(
      entityUrl("acme-app", "Billing::InvoiceService", { path: "app/services/invoice.rb", line: 3 }),
    );
  });

  it("symbol with path and no ent uses symbolUrl", () => {
    const r: ProseRef = {
      kind: "symbol",
      span: { start: 0, end: 17 },
      text: "OpsController#run",
      container: "OpsController",
      member: "run",
      resolution: { state: "likely", path: "app/controllers/ops_controller.rb", line: 10 },
    };
    expect(proseRefHref(r, CTX)).toBe(
      symbolUrl("acme-app", "OpsController#run", {
        fallbackPath: "app/controllers/ops_controller.rb",
        fallbackLine: 10,
      }),
    );
  });

  it("call never becomes exact in the overlay class even if the wire said so", () => {
    // The daemon refuses exact for `call`; the class still follows the wire
    // state, and a missing resolution is not a link.
    const r: ProseRef = {
      kind: "call",
      span: { start: 0, end: 13 },
      text: "enqueue_order",
    };
    expect(proseRefHref(r, CTX)).toBeNull();
  });
});

describe("trust line-style", () => {
  it("solid only for exact; dashed likely; dotted candidate and orphan", () => {
    expect(proseRefClass(pathRef({ resolution: { state: "exact" } }))).toContain("kbc-prose-ref--exact");
    expect(proseRefClass(pathRef({ resolution: { state: "likely" } }))).toContain("kbc-prose-ref--likely");
    expect(proseRefClass(pathRef({ resolution: { state: "candidate" } }))).toContain("kbc-prose-ref--candidate");
    expect(proseRefClass(pathRef({ resolution: { state: "orphan", caption: "gone" } }))).toContain(
      "kbc-prose-ref--orphan",
    );
    expect(proseRefClass(pathRef({ resolution: { state: "orphan", caption: "gone" } }))).toContain(
      "kbc-prose-ref--nolink",
    );
  });
});

describe("golden overlay — rendered structure", () => {
  it("path-with-range becomes a codeUrl link at the span", () => {
    const text = "The guard in app/controllers/api/internal/ops_controller.rb:57-59 never authorizes the actor.";
    const token = "app/controllers/api/internal/ops_controller.rb:57-59";
    const start = text.indexOf(token);
    const ref: ProseRef = {
      kind: "path",
      span: { start, end: start + token.length },
      text: token,
      path: "app/controllers/api/internal/ops_controller.rb",
      line_start: 57,
      line_end: 59,
      resolution: {
        state: "exact",
        path: "app/controllers/api/internal/ops_controller.rb",
        line: 57,
      },
    };
    const tree = buildProseTree(text, [ref], CTX);
    const links = refsOf(tree);
    expect(links).toHaveLength(1);
    expect(links[0].text).toBe("app/controllers/api/internal/ops_controller.rb:57-59");
    expect(links[0].href).toBe(
      codeUrl({
        repo: "acme-app",
        path: "app/controllers/api/internal/ops_controller.rb",
        line: { start: 57, end: 59 },
      }),
    );
    expect(links[0].className).toContain("kbc-prose-ref--exact");
    expect(tree.blocks[0].kind).toBe("paragraph");
  });

  it("finding-slug becomes a findingUrl", () => {
    const text = "Same failure as f-double-charge but on the new path.";
    const ref: ProseRef = {
      kind: "finding",
      span: { start: 16, end: 31 },
      text: "f-double-charge",
      slug: "f-double-charge",
      resolution: { state: "exact" },
    };
    const links = refsOf(buildProseTree(text, [ref], CTX));
    expect(links).toHaveLength(1);
    expect(links[0].href).toBe(findingUrl("acme-app", 7, "f-double-charge"));
    expect(links[0].kind).toBe("finding");
  });

  it("Namespace::Class becomes an entity link", () => {
    const text = "Billing::InvoiceService double-charges when the webhook retries.";
    const ref: ProseRef = {
      kind: "symbol",
      span: { start: 0, end: 21 },
      text: "Billing::InvoiceService",
      container: "Billing::InvoiceService",
      resolution: { state: "exact", ent: "Billing::InvoiceService", path: "app/services/billing.rb", line: 1 },
    };
    const links = refsOf(buildProseTree(text, [ref], CTX));
    expect(links[0].href).toBe(
      entityUrl("acme-app", "Billing::InvoiceService", { path: "app/services/billing.rb", line: 1 }),
    );
    expect(links[0].trust).toBe("exact");
  });

  it("Class#method inside backticks stays wrapped in code and is still a link", () => {
    const text = "`OpsController#run` swallows the backend error and returns 200.";
    const refs: ProseRef[] = [
      { kind: "code", span: { start: 0, end: 19 }, text: "OpsController#run" },
      {
        kind: "symbol",
        span: { start: 1, end: 18 },
        text: "OpsController#run",
        container: "OpsController",
        member: "run",
        resolution: { state: "likely", path: "app/controllers/ops_controller.rb", line: 4 },
      },
    ];
    const tree = buildProseTree(text, refs, CTX);
    const links = refsOf(tree);
    expect(links).toHaveLength(1);
    expect(links[0].wrap).toBe("code");
    expect(links[0].className).toContain("kbc-prose-ref--likely");
    expect(links[0].href).toBe(
      symbolUrl("acme-app", "OpsController#run", {
        fallbackPath: "app/controllers/ops_controller.rb",
        fallbackLine: 4,
      }),
    );
  });

  it("bare CapWord is not a ref (the wire sent none)", () => {
    const text = "The Product record is loaded twice.";
    const tree = buildProseTree(text, [], CTX);
    expect(refsOf(tree)).toEqual([]);
    expect(tree.blocks).toEqual([
      { kind: "paragraph", nodes: [{ t: "text", text: "The Product record is loaded twice." }] },
    ]);
  });

  it("a URL is not a ref", () => {
    const text = "Introduced by https://github.com/acme/shopfront/pull/77 last week.";
    expect(refsOf(buildProseTree(text, [], CTX))).toEqual([]);
  });

  it("orphan renders with dotted class and no href", () => {
    const text = "Missing app/models/ghost.rb:1 is gone.";
    const ref: ProseRef = {
      kind: "path",
      span: { start: 8, end: 30 },
      text: "app/models/ghost.rb:1",
      path: "app/models/ghost.rb",
      line_start: 1,
      resolution: { state: "orphan", caption: "not in the mirror at this patchset" },
    };
    const links = refsOf(buildProseTree(text, [ref], CTX));
    expect(links).toHaveLength(1);
    expect(links[0].href).toBeNull();
    expect(links[0].className).toContain("kbc-prose-ref--orphan");
    expect(links[0].title).toBe("not in the mirror at this patchset");
    expect(links[0].peek).toBe("not in the mirror at this patchset");
  });

  it("never double-links a ref inside a Markdown [text](dest)", () => {
    const text = "see [app/foo.rb:1](https://example.com/foo) please";
    const ref: ProseRef = {
      kind: "path",
      span: { start: 5, end: 17 },
      text: "app/foo.rb:1",
      path: "app/foo.rb",
      line_start: 1,
      resolution: { state: "exact", path: "app/foo.rb", line: 1 },
    };
    const links = refsOf(buildProseTree(text, [ref], CTX));
    expect(links).toHaveLength(1);
    expect(links[0].href).toBeNull();
    expect(links[0].skippedLink).toBe(true);
    expect(links[0].className).toContain("kbc-prose-ref--nolink");
  });

  it("call inside backticks is overlayable but not exact", () => {
    const text = "`enqueue_order(` is called without an idempotency key.";
    const refs: ProseRef[] = [
      { kind: "code", span: { start: 0, end: 15 }, text: "enqueue_order(" },
      {
        kind: "call",
        span: { start: 1, end: 14 },
        text: "enqueue_order",
        resolution: { state: "likely", path: "app/jobs/enqueue.rb", line: 8 },
      },
    ];
    const links = refsOf(buildProseTree(text, refs, CTX));
    expect(links).toHaveLength(1);
    expect(links[0].kind).toBe("call");
    expect(links[0].trust).toBe("likely");
    expect(links[0].wrap).toBe("code");
    expect(links[0].href).toBe(
      symbolUrl("acme-app", "enqueue_order", { fallbackPath: "app/jobs/enqueue.rb", fallbackLine: 8 }),
    );
  });

  it("UTF-16 offsets: a non-ASCII prefix does not shift the path span", () => {
    const text = "café app/foo.rb:1 done";
    // "café " is 5 UTF-16 units (é is one code unit).
    expect("café ".length).toBe(5);
    const token = "app/foo.rb:1";
    const start = text.indexOf(token);
    expect(start).toBe(5);
    const ref: ProseRef = {
      kind: "path",
      span: { start, end: start + token.length },
      text: token,
      path: "app/foo.rb",
      line_start: 1,
      resolution: { state: "exact", path: "app/foo.rb", line: 1 },
    };
    const links = refsOf(buildProseTree(text, [ref], CTX));
    expect(links[0].text).toBe("app/foo.rb:1");
    expect(links[0].href).toBe(codeUrl({ repo: "acme-app", path: "app/foo.rb", line: 1 }));
  });

  it("code kind is ignored by the overlay (the renderer already styles backticks)", () => {
    const text = "`status: 'in_queue'` is never set before the enqueue runs.";
    const ref: ProseRef = {
      kind: "code",
      span: { start: 0, end: 20 },
      text: "status: 'in_queue'",
    };
    const tree = buildProseTree(text, [ref], CTX);
    expect(refsOf(tree)).toEqual([]);
    const para = tree.blocks[0];
    if (para.kind !== "paragraph") throw new Error("expected paragraph");
    expect(para.nodes.some((n) => n.t === "code" && n.text === "status: 'in_queue'")).toBe(true);
  });

  it("truncated is surfaced, never dropped", () => {
    const tree = buildProseTree("hello", { refs: [], truncated: true }, CTX);
    expect(tree.truncated).toBe(true);
  });
});

describe("parseInlineSourced", () => {
  it("tracks offsets through bold and code", () => {
    const runs = parseInlineSourced("ab **cd** `e`");
    expect(runs).toEqual([
      { kind: "text", text: "ab ", start: 0, end: 3 },
      { kind: "bold", text: "cd", start: 5, end: 7 },
      { kind: "text", text: " ", start: 9, end: 10 },
      { kind: "code", text: "e", start: 11, end: 12 },
    ]);
  });
});

describe("overlayRun clips a ref to the run", () => {
  it("splits surrounding text", () => {
    const run = { kind: "text" as const, text: "see Foo::Bar now", start: 0, end: 16 };
    const ref: ProseRef = {
      kind: "symbol",
      span: { start: 4, end: 12 },
      text: "Foo::Bar",
      container: "Foo::Bar",
      resolution: { state: "candidate", caption: "2 classes" },
    };
    const nodes = overlayRun(run, [ref], [], CTX);
    expect(nodes.map((n) => [n.t, n.text, n.href ?? null])).toEqual([
      ["text", "see ", null],
      ["ref", "Foo::Bar", symbolUrl("acme-app", "Foo::Bar", { fallbackPath: "" })],
      ["text", " now", null],
    ]);
    expect(nodes[1].className).toContain("kbc-prose-ref--candidate");
  });
});
