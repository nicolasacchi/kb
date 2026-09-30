import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

// The annotator bundle is injected into the ARTIFACT's document as a
// classic `<script defer>` (kb-core's `iframe::inject_annotator`), so every
// binding it declares at top level lands on that page's global scope. That
// is a shared namespace: if the artifact already declared the same name,
// the whole script dies on a SyntaxError before a single listener is
// attached — no highlight bands, no in-page comment icons, no
// `cm:selection` relay, i.e. the entire comment UI silently absent from
// that one artifact.
//
// This is not hypothetical. The `vite 6.4.3 → 8.3.1` bump (#175) moved the
// build to rolldown + oxc, whose mangler renames top-level names to one or
// two letters; `post()` became `function d`, and kitchen-sink.html declares
// its own top-level `const d` (the dialog demo). Every Playwright spec that
// needs the annotator's real listeners failed on both engines, and stayed
// invisible for a day because the e2e step piped playwright through `tee`
// without `set -o pipefail` (fixed in d4f3063d). Vite 6's longer names had
// merely been lucky.
//
// These two specs pin the invariant at the bundle level so it holds for
// EVERY artifact, not just the one whose globals happen to collide today:
// the bundle must install nothing into the global scope, and it must still
// work on a page that already owns the short names.
//
// They read `web/dist/annotate.js` directly (the e2e job builds the SPA
// before Playwright runs — global-setup points the daemon at the same
// `web/dist`), so they test the artifact that actually ships rather than
// re-deriving it from source.

const BUNDLE = resolve(__dirname, "..", "..", "web", "dist", "annotate.js");

/// A minimal but well-formed `window.__KB_COMMENTS` with ONE file-scope
/// comment. File scope is deliberate: its anchor always resolves (to
/// `document.body`), so "the annotator ran" is a statement about the
/// annotator and nothing else. `kb-annot-marker` is the cheapest painted
/// proof — `init()` also injects the `<style>`, but that happens first and
/// would pass even if painting threw.
const ENVELOPE = {
  v: 1,
  etag: null,
  file: {
    artifact: { id: "a1", title: "t", kb: "k" },
    generatedAt: "2026-01-01T00:00:00Z",
    comments: [
      {
        id: "c-bundle-isolation",
        status: "open",
        file: "a1",
        fileLabel: "main",
        anchor: { kind: "file" },
        author: "you",
        body: "painted by the bund",
        createdAt: "2026-01-01T00:00:00Z",
        editedAt: null,
        replies: [],
      },
    ],
  },
};

/// Inject the built bundle the way the daemon does — a classic script
/// element, no `type="module"` — and report what it did to the page.
async function install(page: Page) {
  const src = readFileSync(BUNDLE, "utf8");
  return page.evaluate(
    ({ src, envelope }) => {
      const errors: string[] = [];
      window.addEventListener("error", (e) => errors.push(String(e.message)));

      // Seeded BEFORE the snapshot on purpose: `__KB_COMMENTS` is this
      // harness's own property, and attributing it to the bundle would be a
      // false positive on the exact thing being measured.
      Object.assign(window, { __KB_COMMENTS: envelope });
      const before = Object.getOwnPropertyNames(window);
      const el = document.createElement("script");
      el.textContent = src;
      document.body.appendChild(el);

      return {
        added: Object.getOwnPropertyNames(window).filter(
          (k) => !before.includes(k),
        ),
        errors,
        markers: document.querySelectorAll(".kb-annot-marker").length,
        markerId:
          document.querySelector(".kb-annot-marker")?.getAttribute(
            "data-kb-comment-id",
          ) ?? null,
      };
    },
    { src, envelope: ENVELOPE },
  );
}

test.describe("annotator bundle isolation", () => {
  test("installs into the artifact's global scope without touching it", async ({
    page,
  }) => {
    await page.goto("about:blank");
    const out = await install(page);

    expect(out.errors, "the bundle must parse and run").toEqual([]);
    // The whole point: a classic script's top-level `var`/`function`
    // declarations become properties of the artifact's `window`. Even one
    // here is a name the next artifact revision might also declare.
    expect(out.added, "the bundle must declare no globals").toEqual([]);
    // …and it must still have done its job.
    expect(out.markers).toBe(1);
    expect(out.markerId).toBe("c-bundle-isolation");
  });

  test("still installs on an artifact that already owns the short names", async ({
    page,
  }) => {
    await page.goto("about:blank");
    // Exactly the kitchen-sink.html shape that broke: a top-level `const d`
    // in the artifact's own classic script. `d` is what the oxc mangler
    // picked for the annotator's `post()` helper; the rest are the other
    // single-letter names in that build, so claiming the lot keeps the
    // assertion independent of today's mangler being exactly `d`.
    await page.evaluate(() => {
      const el = document.createElement("script");
      el.textContent =
        "const d=1,e=2,f=3,n=4,r=5,s=6,t=7,i=8,o=9,a=10,u=11,c=12,l=13,p=14,m=15,h=16;";
      document.body.appendChild(el);
    });

    const out = await install(page);

    expect(out.errors).toEqual([]);
    // A redeclaration collision is a SyntaxError, which kills the WHOLE
    // script — so this single assertion covers every listener the annotator
    // registers, not just the painting path.
    expect(out.markers).toBe(1);
    expect(out.markerId).toBe("c-bundle-isolation");
  });
});
