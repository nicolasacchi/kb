import { describe, expect, it } from "vitest";
import type { TrailStep } from "./trail";
import {
  MAX_TOUR_STEPS,
  cameraCaption,
  codeRefFor,
  draftProblems,
  draftToDoc,
  hopsToDraft,
  landingOf,
} from "./tourDoc";

// V74-L3b — record-a-tour, and the three rules `tourDoc.ts`'s header states.

function hop(to: string, via?: TrailStep["via"]): TrailStep {
  return { from: "/r/kb/a.rb", fromLabel: "a.rb", to, via, at: 0 };
}

describe("landingOf", () => {
  it("reads the path and the line off a reader URL", () => {
    expect(landingOf("/r/kb/app/models/order.rb?line=12")).toEqual({
      path: "app/models/order.rb",
      line: 12,
    });
  });

  it("reads a path with no line", () => {
    expect(landingOf("/r/kb/app/models/order.rb")).toEqual({ path: "app/models/order.rb" });
  });

  it("stops at a sentinel segment", () => {
    expect(landingOf("/r/kb/app/a.rb/~story?at=abc")).toEqual({ path: "app/a.rb" });
  });

  it("is TOTAL — a URL it cannot read yields no landing rather than a guess", () => {
    for (const url of ["", "/", "/search?q=x", "/r/kb", "/r/kb/~boards/checkout", "nonsense"]) {
      expect(landingOf(url), url).toEqual({});
    }
  });

  it("ignores a non-positive or non-integer line rather than inventing one", () => {
    expect(landingOf("/r/kb/a.rb?line=0")).toEqual({ path: "a.rb" });
    expect(landingOf("/r/kb/a.rb?line=x")).toEqual({ path: "a.rb" });
    expect(landingOf("/r/kb/a.rb?line=2.5")).toEqual({ path: "a.rb" });
  });
});

describe("codeRefFor", () => {
  it("cites a line or a range and NEVER a blob", () => {
    expect(codeRefFor("a.rb", 12)).toBe("code:a.rb:12");
    expect(codeRefFor("a.rb", 12, 20)).toBe("code:a.rb:12-20");
    // The recording did not observe a sha, so the ref carries none — the
    // daemon captures the witness at apply time, under its own exact rule.
    expect(codeRefFor("a.rb", 12)).not.toContain("@");
  });

  it("returns null when there is no line to cite", () => {
    expect(codeRefFor("a.rb", undefined)).toBeNull();
    expect(codeRefFor("a.rb", 0)).toBeNull();
    expect(codeRefFor(undefined, 3)).toBeNull();
  });

  it("drops an end that does not extend the start rather than emitting a backwards range", () => {
    expect(codeRefFor("a.rb", 12, 12)).toBe("code:a.rb:12");
    expect(codeRefFor("a.rb", 12, 4)).toBe("code:a.rb:12");
  });
});

describe("hopsToDraft", () => {
  it("makes one step per landing, in walk order, with distinct ids", () => {
    const d = hopsToDraft(
      [
        hop("/r/kb/app/order.rb?line=3", "search"),
        hop("/r/kb/app/cart.rb?line=9", "definition_of"),
      ],
      { slug: "s", title: "T" },
    );
    expect(d.steps).toHaveLength(2);
    expect(d.steps[0].ref).toBe("code:app/order.rb:3");
    expect(d.steps[1].ref).toBe("code:app/cart.rb:9");
    expect(new Set(d.steps.map((s) => s.id)).size).toBe(2);
    expect(d.steps[0].title).toContain("search");
  });

  it("de-duplicates ids derived from the same basename", () => {
    const d = hopsToDraft([hop("/r/kb/a/x.rb?line=1"), hop("/r/kb/b/x.rb?line=2")]);
    expect(d.steps[0].id).not.toBe(d.steps[1].id);
  });

  it("makes a hop with no line a PROSE step and says why", () => {
    const d = hopsToDraft([hop("/r/kb/app/order.rb")]);
    expect(d.steps[0].ref).toBe("");
    expect(d.steps[0].notes[0]).toContain("no line");
  });

  it("truncates from the END and says so — a walk keeps its beginning", () => {
    const many = Array.from({ length: MAX_TOUR_STEPS + 5 }, (_, i) =>
      hop(`/r/kb/f${i}.rb?line=1`),
    );
    const d = hopsToDraft(many);
    expect(d.steps).toHaveLength(MAX_TOUR_STEPS);
    expect(d.steps[0].title).toContain("f0.rb");
    expect(d.notes[0]).toContain("FIRST");
  });

  it("says so when the window is empty rather than producing a silent nothing", () => {
    const d = hopsToDraft([]);
    expect(d.steps).toHaveLength(0);
    expect(d.notes[0]).toContain("walk somewhere first");
  });
});

describe("draftProblems", () => {
  const base = {
    slug: "checkout",
    title: "Checkout",
    description_md: "",
    steps: [{ id: "s1", title: "T", body_md: "", ref: "code:a.rb:1", notes: [] }],
    notes: [],
  };

  it("passes a well-formed draft", () => {
    expect(draftProblems(base)).toEqual([]);
  });

  it("names each problem by field", () => {
    expect(draftProblems({ ...base, slug: "Not A Slug" })[0]).toContain("slug");
    expect(draftProblems({ ...base, slug: "apply" }).join(" ")).toContain("reserved");
    expect(draftProblems({ ...base, title: "   " })[0]).toContain("title");
    expect(draftProblems({ ...base, steps: [] })[0]).toContain("no steps");
    expect(
      draftProblems({
        ...base,
        steps: [base.steps[0], { ...base.steps[0] }],
      }).join(" "),
    ).toContain("duplicate step id");
    expect(
      draftProblems({
        ...base,
        steps: [{ ...base.steps[0], camera: { context: 999 } }],
      })[0],
    ).toContain("camera context");
  });
});

describe("draftToDoc", () => {
  const draft = {
    slug: "checkout",
    title: "  Checkout  ",
    description_md: "how",
    steps: [
      { id: "s1", title: "Entry", body_md: "because", ref: "code:a.rb:1-5", notes: [] },
      { id: "s2", title: "", body_md: "", ref: "", notes: [] },
    ],
    notes: [],
  };

  it("emits the daemon's own document shape and trims what it should", () => {
    const { doc } = draftToDoc("kb", draft);
    expect(doc.schema).toBe("kbc-tour/1");
    expect(doc.repo).toBe("kb");
    expect(doc.title).toBe("Checkout");
    expect(doc.steps[0]).toEqual({
      id: "s1",
      title: "Entry",
      body_md: "because",
      ref: "code:a.rb:1-5",
    });
    // An empty title/body/ref is OMITTED, not sent as "": the document is
    // `deny_unknown_fields` and an empty string is not the same as absent.
    expect(doc.steps[1]).toEqual({ id: "s2" });
  });

  it("emits NO coordinates and would refuse if it ever did", () => {
    const { doc, coordinateViolations } = draftToDoc("kb", draft);
    expect(coordinateViolations).toEqual([]);
    expect(JSON.stringify(doc)).not.toMatch(/"(x|y|width|height|position|left|top|coords)"/);
  });

  it("carries a camera only when the camera says something", () => {
    const withCam = {
      ...draft,
      steps: [
        { ...draft.steps[0], camera: { fold: true, context: 3 } },
        { ...draft.steps[1], camera: {} },
      ],
    };
    const { doc } = draftToDoc("kb", withCam);
    expect(doc.steps[0].camera).toEqual({ fold: true, context: 3 });
    expect(doc.steps[1].camera).toBeUndefined();
  });
});

describe("cameraCaption", () => {
  it("says only what the camera actually asks for", () => {
    expect(cameraCaption(null)).toBeNull();
    expect(cameraCaption({})).toBeNull();
    expect(cameraCaption({ fold: false, context: 0 })).toBeNull();
    expect(cameraCaption({ fold: true })).toContain("folded");
    expect(cameraCaption({ context: 3 })).toContain("±3");
    expect(cameraCaption({ fold: true, context: 3 })).toContain("·");
  });
});
