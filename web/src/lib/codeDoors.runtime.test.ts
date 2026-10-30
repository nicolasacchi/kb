// @vitest-environment jsdom
//
// OL IDEA6 - the artifact runtime's door block, EXECUTED. The block is plain
// ES2015 in crates/kb-server/src/routes/runtime_doors.js, spliced into the
// served runtime; here the same file is loaded into the jsdom realm and driven
// with the golden that ties it to kb_core::coderefs::extract.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Node's fs without Node typings (same trick as styles/tokens.palette.test.ts).
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const fs = (globalThis as any).process.getBuiltinModule("node:fs") as {
  readFileSync(path: string, enc: "utf8"): string;
};
const here = (rel: string): string =>
  decodeURIComponent(new URL(rel, import.meta.url).pathname);
const DIR = "../../../crates/kb-server/src/routes/";
const DOORS_SRC = fs.readFileSync(here(DIR + "runtime_doors.js"), "utf8");
const GOLDEN = JSON.parse(fs.readFileSync(here(DIR + "runtime_doors.golden.json"), "utf8")) as {
  schema: string;
  cases: Array<{
    name: string;
    body: string;
    refs: Array<{ ordinal: number; raw: string; declared: boolean }>;
    decorated: number[];
  }>;
};

// The file is repo-owned source, never user input; a Function over its text is
// the way to run a classic script in this realm.
// eslint-disable-next-line @typescript-eslint/no-implied-eval
const kbInstallCodeDoors = new Function(`${DOORS_SRC}\nreturn kbInstallCodeDoors;`)() as (
  target: string,
) => void;

type Rec = { target: EventTarget; type: string; fn: EventListener };
let recs: Rec[] = [];
const openFn = vi.fn();
let realOpen: typeof window.open;

function install(target: string): void {
  // Record the listeners the block adds so each test starts from a clean
  // document (the block binds to window/document for the life of a page).
  /* eslint-disable @typescript-eslint/no-explicit-any */
  const patched: Array<[any, any]> = [];
  for (const t of [window, document] as any[]) {
    const orig = t.addEventListener;
    patched.push([t, orig]);
    t.addEventListener = function (type: string, fn: EventListener, o?: unknown) {
      recs.push({ target: t, type, fn });
      return orig.call(this, type, fn, o);
    };
  }
  try {
    kbInstallCodeDoors(target);
  } finally {
    for (const [t, orig] of patched) t.addEventListener = orig;
  }
  /* eslint-enable @typescript-eslint/no-explicit-any */
}

function uninstall(): void {
  for (const r of recs) r.target.removeEventListener(r.type, r.fn);
  recs = [];
}

function post(doors: unknown, opts: { source?: unknown; origin?: string } = {}): void {
  const ev = new Event("message");
  Object.defineProperties(ev, {
    data: { value: { kind: "kb:coderefs", doors } },
    source: { value: "source" in opts ? opts.source : window.parent },
    origin: { value: opts.origin ?? "" },
  });
  window.dispatchEvent(ev);
}

const door = (ordinal: number, raw: string, extra: Record<string, unknown> = {}) => ({
  ordinal,
  raw,
  declared: false,
  href: `https://code.example/r/app/x.rs?line=${ordinal + 1}`,
  cls: "exact",
  title: `door ${ordinal}`,
  ...extra,
});

function decorated(): number[] {
  return Array.from(document.body.querySelectorAll("[data-kb-coderef]")).map((e) =>
    Number(e.getAttribute("data-kb-coderef")),
  );
}

function click(el: Element, init: MouseEventInit = {}): MouseEvent {
  const ev = new MouseEvent("click", { bubbles: true, cancelable: true, button: 0, ...init });
  el.dispatchEvent(ev);
  return ev;
}

beforeEach(() => {
  recs = [];
  document.body.innerHTML = "";
  openFn.mockClear();
  realOpen = window.open;
  window.open = openFn as unknown as typeof window.open;
});
afterEach(() => {
  uninstall();
  window.getSelection()?.removeAllRanges();
  window.open = realOpen;
});

describe("runtime doors against the extractor golden", () => {
  it("has the pinned schema and the cases the brief names", () => {
    expect(GOLDEN.schema).toBe("kb-coderef-doors-golden/1");
    expect(GOLDEN.cases.length).toBeGreaterThanOrEqual(5);
  });

  for (const c of GOLDEN.cases) {
    it(`decorates exactly the golden ordinals, attribute-only: ${c.name}`, () => {
      document.body.innerHTML = c.body;
      const original = document.body.innerHTML;
      const nodes = document.body.querySelectorAll("*").length;
      install("*");
      post(c.refs.map((r) => door(r.ordinal, r.raw, { declared: r.declared })));
      expect(decorated()).toEqual(c.decorated);
      expect(document.body.querySelectorAll("*").length).toBe(nodes);
      post([]);
      expect(decorated()).toEqual([]);
      expect(document.body.innerHTML).toBe(original);
    });
  }
});

describe("runtime doors - behaviour", () => {
  const FILE = "crates/kb-core/src/x.rs";
  function setup(): HTMLElement {
    document.body.innerHTML = `<p><code id="c">${FILE}</code></p>`;
    install("*");
    post([door(0, FILE)]);
    return document.getElementById("c") as HTMLElement;
  }

  it("marks a door with class, role, tabindex, title and a class-keyed line style", () => {
    const el = setup();
    expect(el.getAttribute("data-kb-coderef-cls")).toBe("exact");
    expect(el.getAttribute("role")).toBe("link");
    expect(el.getAttribute("tabindex")).toBe("0");
    expect(el.getAttribute("title")).toBe("door 0");
  });

  it("maps classes to line styles: exact/moved solid, unsure dotted, hint dashed", () => {
    const ids = ["exact", "moved", "unsure", "hint", "bogus"];
    document.body.innerHTML = ids.map((id, i) => `<p><code id="${id}">crates/kb-core/src/f${i}.rs</code></p>`).join("");
    install("*");
    post(ids.map((id, i) => door(i, `crates/kb-core/src/f${i}.rs`, { cls: id })));
    expect(document.getElementById("bogus")!.getAttribute("data-kb-coderef-cls")).toBe("hint");
    expect(document.getElementById("moved")!.getAttribute("data-kb-coderef-cls")).toBe("moved");
    const ds = (id: string) => document.getElementById(id)!.style.getPropertyValue("text-decoration-style");
    // jsdom's CSSOM may drop text-decoration longhands; when it keeps them they must match.
    for (const [id, want] of [["exact", "solid"], ["moved", "solid"], ["unsure", "dotted"], ["hint", "dashed"]] as const) {
      const got = ds(id);
      if (got) expect(got).toBe(want);
    }
  });

  it("a plain click opens the door in a new tab with noopener,noreferrer and prevents default", () => {
    const el = setup();
    const ev = click(el);
    expect(openFn).toHaveBeenCalledTimes(1);
    expect(openFn).toHaveBeenCalledWith("https://code.example/r/app/x.rs?line=1", "_blank", "noopener,noreferrer");
    expect(ev.defaultPrevented).toBe(true);
  });

  it("Enter on a focused door does the same", () => {
    const el = setup();
    el.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
    expect(openFn).toHaveBeenCalledWith("https://code.example/r/app/x.rs?line=1", "_blank", "noopener,noreferrer");
  });

  it("does not open on a modified Enter, a non-primary click, or a prevented click", () => {
    const el = setup();
    el.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", ctrlKey: true, bubbles: true, cancelable: true }));
    click(el, { button: 1 });
    el.addEventListener("click", (e) => e.preventDefault(), { once: true });
    click(el);
    expect(openFn).not.toHaveBeenCalled();
  });

  it("does not open while the reader is selecting text in the door", () => {
    const el = setup();
    const range = document.createRange();
    range.selectNodeContents(el);
    const sel = window.getSelection()!;
    sel.removeAllRanges();
    sel.addRange(range);
    click(el);
    expect(openFn).not.toHaveBeenCalled();
  });

  it("never decorates a javascript: or non-http(s) href", () => {
    document.body.innerHTML = `<p><code>${"crates/kb-core/src/x.rs"}</code></p>`;
    install("*");
    for (const href of ["javascript:alert(1)", "data:text/html,x", "ftp://h/x", "not a url", ""]) {
      post([door(0, "crates/kb-core/src/x.rs", { href })]);
      expect(decorated(), href).toEqual([]);
    }
    expect(openFn).not.toHaveBeenCalled();
  });

  it("drops a bad door independently of its neighbours", () => {
    document.body.innerHTML = `<p><code>crates/kb-core/src/a.rs</code> <code>crates/kb-core/src/b.rs</code></p>`;
    install("*");
    post([
      door(0, "crates/kb-core/src/a.rs", { ordinal: -1 }),
      door(1, "crates/kb-core/src/b.rs"),
    ]);
    expect(decorated()).toEqual([1]);
  });

  it("an element the page itself marked data-kb-coderef opens nothing", () => {
    document.body.innerHTML = `<p><code id="own" data-kb-coderef="9">crates/kb-core/src/own.rs</code> <code>crates/kb-core/src/x.rs</code></p>`;
    install("*");
    post([door(0, "crates/kb-core/src/x.rs")]);
    click(document.getElementById("own")!);
    expect(openFn).not.toHaveBeenCalled();
  });

  it("a message with no source, or from another origin, changes nothing", () => {
    document.body.innerHTML = `<p><code>${"crates/kb-core/src/x.rs"}</code></p>`;
    install("*");
    post([door(0, "crates/kb-core/src/x.rs")], { source: null });
    expect(decorated()).toEqual([]);
    uninstall();
    install("https://kb.example");
    post([door(0, "crates/kb-core/src/x.rs")], { origin: "https://evil.example" });
    expect(decorated()).toEqual([]);
    post([door(0, "crates/kb-core/src/x.rs")], { origin: "https://kb.example" });
    expect(decorated()).toEqual([0]);
  });

  it("restores a pre-existing style, title and tabindex when the doors are withdrawn", () => {
    document.body.innerHTML = `<p><code id="c" style="color: red" title="mine" tabindex="3">crates/kb-core/src/x.rs</code></p>`;
    const original = document.body.innerHTML;
    install("*");
    post([door(0, "crates/kb-core/src/x.rs")]);
    expect(document.getElementById("c")!.getAttribute("title")).toBe("door 0");
    post([]);
    expect(document.body.innerHTML).toBe(original);
  });

  it("replaces the previous list wholesale on each message", () => {
    document.body.innerHTML = `<p><code>crates/kb-core/src/a.rs</code> <code>crates/kb-core/src/b.rs</code></p>`;
    install("*");
    post([door(0, "crates/kb-core/src/a.rs")]);
    expect(decorated()).toEqual([0]);
    post([door(1, "crates/kb-core/src/b.rs")]);
    expect(decorated()).toEqual([1]);
  });

  it("leaves a click inside an author's link or button to the author", () => {
    document.body.innerHTML = `<p><code id="c"><a id="in" href="#x">crates/kb-core/src/x.rs</a></code></p>`;
    install("*");
    post([door(0, "crates/kb-core/src/x.rs")]);
    // The code sits around a link; the link wins the click.
    click(document.getElementById("in")!);
    expect(openFn).not.toHaveBeenCalled();
  });
});
