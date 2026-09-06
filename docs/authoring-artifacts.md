# Authoring HTML artifacts for kb

This guide is for sessions *generating* HTML that will be ingested
by `kb`. For the daemon's internals see
[docs/architecture-invariants.md](architecture-invariants.md) (and
[CLAUDE.md](../CLAUDE.md) for orientation); for the user-facing surface see
[README.md](../README.md).

The rules below come from `crates/kb-core/src/parser.rs`,
`crates/kb-core/src/iframe.rs`, `crates/kb-core/src/scrub.rs`,
`crates/kb-core/src/markdown.rs`, and `crates/kb-core/src/review.rs`.
File:line citations are included so you can verify what the indexer
actually does.

Prefer hand-authored HTML for rich, interactive artifacts; reach for
Markdown (see [Markdown artifacts](#markdown-artifacts-md--markdown))
when you just want prose, code, and tables.

## Required structure

Every artifact needs:

- A `<title>` tag — primary BM25 field. Falls back to first `<h1>`,
  then first `<p>`, but explicit `<title>` is always better
  (`parser.rs:183-185`).
- A `<body>` containing the substance. Text under `<script>`,
  `<style>`, `<template>`, `<noscript>` is **invisible to indexing
  and embedding** — don't hide content there (`parser.rs:498`).
- A real heading hierarchy (`<h1>` … `<h6>`). The parser collects
  these into a separate BM25 field `headings` and they're the basis
  for kb-comments Chapter-scope anchors (`parser.rs:159, 429-449`).

The embedder (bge-small, 384-dim) sees the `body` field only — not
title, not headings, not the prompt. Make sure substantive text
lives in body, not just in a fancy heading (`indexer.rs:290`).

## `<template id="kb-prompt">` — the model prompt

If the artifact was generated from a prompt, embed that prompt as
the `inner_html` of a `<template id="kb-prompt">` element. The
parser extracts and stores it; BM25 indexes it; the outbound scrub
strips it on non-loopback serves so it never leaks to public URLs
(`parser.rs:215-223`, `kb-server/src/scrub.rs`).

- Hard cap: **8 KB** (`PROMPT_MAX_BYTES`, `parser.rs:26`).
- Format: HTML or plain text, your choice. Not JSON.
- Don't put prompts anywhere else — only `<template id="kb-prompt">`
  is scrubbed.
- Loopback serves see it unredacted, which is what you want during
  local dev.

## Metadata via `<meta>` tags

Two meta names are recognised; everything else is ignored (a third,
kb-code-rev, is recognized separately by the code-reference extraction pass —
see **Code references** below):

- `<meta name="kb-category" content="notes">` — single string,
  stored as-is (`parser.rs:196-200`).
- `<meta name="kb-tags" content="rust, atlas, design">` — comma-
  separated; each is slugified (lowercase, alphanumeric + dash)
  (`parser.rs:202-213`). If absent, the indexer derives tags from
  path segments instead — better to be explicit.

Both are also editable after the fact from the SPA: the detail-view
inspector rail lets you add/remove tags and change the category. That
rewrites this `<meta>` line (or the `.md` frontmatter key) in the source
file in place — a one-line, byte-preserving edit — and the watcher
re-indexes it. The artifact source stays the source of truth; the UI just
saves you opening the file. (Clearing all tags reverts to the path-derived
fallback, since there is no "no tags" representation.)

### Session provenance (`kb-session`)

Another meta name recognised separately from the two above (like
`kb-code-rev`): `<meta name="kb-session" content="<claude-code-session-id>">`
(HTML) or `kb-session: <id>` in Markdown frontmatter. It links the artifact
back to the Claude Code session that produced it — the join the SPA's
Sessions tab and an artifact's "authored by this session" flag read (see
architecture invariant #11).

**`kb notes new` auto-stamps it for you.** When a fresh session marker
exists (`~/.cache/kb/current-session`, written by the SessionStart /
UserPromptSubmit hooks) and the note doesn't already carry a `kb-session`
value, the CLI splices it into the note's frontmatter before creating it —
an existing value is never overwritten. Pass `--no-session` to opt out.
Hand-authored HTML artifacts — dropped straight into a `kb add`-registered
corpus, or scaffolded via `kb new` — are **not** auto-stamped; add the
`<meta name="kb-session">` tag yourself if you want that link.

### Keeping status honest after shipping

A research/review doc's `kb-tags` typically carries one doc-level lifecycle tag
(`status:open`) set at authoring time — and then never changes, even after half
its findings ship. That forces every later synthesis (a `/kb-audit` pass, a
follow-up review, an agent deciding what's still worth fixing) to re-verify
every finding from zero, because the doc's own metadata is silently stale. The
convention below keeps per-finding status truthful without touching the things
that must stay stable (anchors, the prompt):

- **Doc-level tag**: once *some* (not all) findings in a doc have verifiably
  shipped, flip `status:open` → `status:partial` in the `kb-tags` `<meta>` (a
  slug like any other tag — no schema change). Flip to `status:shipped` only
  when every finding is resolved; leave `status:open` untouched if nothing has
  landed yet.
- **Per-finding marker**: for each individual finding confirmed shipped, add a
  small inline marker near its heading — a bracketed note or a styled `<span>`
  (e.g. `<span class="shipped-note">shipped 9279de1c 2026-07-06</span>`) —
  naming the commit hash(es) that fixed it and the date verified. Don't rewrite
  or remove the finding's prose; the marker is additive so the doc still reads
  as a historical record of what was found.
- **Heading ids never change** — a shipped marker is added *near* a
  `<h2>`/`<h3>`/`<p class="fh">`, never inside the id-bearing tag itself, so
  inbound links/anchors (comments, cross-doc references) keep resolving.
- **`<template id="kb-prompt">` is untouched** — status bookkeeping is a
  reader-facing fact, not part of the generating prompt; never add markers or
  status prose inside the prompt template.
- **Verify before flipping** — "shipped" means grep/read the cited file at the
  named commit and confirm the defect is actually gone, not that a
  plausible-sounding commit touched the area. `git log -S'<snippet>' --
  <file>` or `git show <hash> -- <file>` against the exact evidence quote the
  finding cites is the fast check; a commit that touches the same file for an
  unrelated reason doesn't count.
- **Automation**: the `/kb-audit` skill runs this reconciliation pass —
  pointed at a doc, it re-verifies each finding's evidence against the current
  repo and applies exactly this flip (doc-tag + per-finding markers), so this
  is a mechanical, repeatable step rather than a one-off edit.

See `docs/research/kb-code-craft-review-2026-07.html` for a worked example: its
`status:open` → `status:partial` flip plus the per-finding `shipped` markers on
the ten-findings list.

## Markdown artifacts (`.md` / `.markdown`)

kb treats Markdown sources as first-class artifacts. A `.md` /
`.markdown` file is rendered to HTML — server-side, with GFM + syntect
code highlighting and a built-in serif reading theme — at every read
site: the iframe serve path, `kb get` / the SPA download, and the `kb
share` static export. You write Markdown; readers get the same HTML
everywhere, and the index sees that rendered DOM, so headings, code,
and links match what's served (`markdown.rs`). Everything below about
anchors, the iframe, and cross-artifact links applies to the rendered
output; only metadata and the prompt are authored differently.

**Wikilinks (`[[…]]`).** In a Markdown note, `[[Some Artifact Title]]` (or
`[[ops/deploy.md]]` / `[[<12-hex id>]]`, with an optional `[[target|alias]]`)
links to any other artifact in the same kb — the connective tissue between
notes and the corpus. The target resolves by a deterministic ladder (id →
source-relative path → exact title → unique filename); a match becomes a real
edge, so it shows up under the target's **backlinks** and in the SPA's native
note view as a clickable link (an unresolved `[[…]]` renders as a muted
"pending" link, never an error). Discover/inspect from the CLI with `kb notes
links <note>` and `kb backlinks <target>`. See invariant #29.

### Frontmatter (metadata)

Lead with a `---`-fenced block of flat `key: value` lines — a small
YAML subset (scalars, plus `a, b` or `[a, b]` lists). It's the Markdown
equivalent of the HTML `<meta name="kb-*">` tags and overrides anything
inferred from the body:

````markdown
---
title: How kb resolves anchors across regens
kb-category: notes
kb-tags: kb, anchors, design
---
# How kb resolves anchors across regens
…
````

Recognised keys: `title`, `kb-category`, `kb-tags` (slugified exactly
like the HTML path), plus the same memory / triage facets the `<meta>`
path supports — `kb-status`, `kb-severity`, `kb-salience`, `kb-decay`,
`kb-supersedes`, `kb-session`, `kb-global`, `kb-linked-kbs`. A bare
leading `---` with no closing fence is treated as a thematic break, not
frontmatter. Title falls back to the first `# H1`, then to `Untitled`.

### The prompt — still `<template id="kb-prompt">`, inline

There is **no** frontmatter `prompt:` key by design: a flat-frontmatter
prompt would slip past the element-only outbound scrub and leak on
public URLs. Raw HTML passes through the renderer, so embed the prompt
exactly as in an HTML artifact — drop the element straight into the
Markdown body:

````markdown
# My Doc

<template id="kb-prompt">
The prompt that generated this document. Stripped on non-loopback
serve and on every static share, just like an HTML artifact's.
</template>

Body text…
````

Same 8 KB cap and same scrub rules as the HTML prompt section above.
**Keep the template a single HTML block — no blank line inside it.** A blank
line terminates the raw-HTML block (CommonMark), so comrak would split the
prompt across the `</template>` and fragment it in the index (the scrub still
strips it either way, so nothing leaks — but the indexed prompt gets mangled).
Line breaks without a blank line are fine.

### Headings, ids, and anchors

Write normal Markdown headings (`#`, `##`, …); the in-iframe runtime
owns the heading ids, and kb-comments Chapter anchors match heading
*text*, so keep heading wording stable across regens. For a durable
Section-scope anchor on a specific element, drop in raw HTML with an
explicit `id` (raw HTML passes through): `<section id="overview"> …
</section>` or `<h2 id="overview">…</h2>`.

### Links between Markdown files

Link siblings with their real `.md` paths — `[details](./details.md)`.
On serve the daemon renders the target on the fly; on `kb share` the
relative `*.md` / `*.markdown` link is rewritten to `.html` (the
sibling deploys as a rendered page). To capture a cross-*artifact*
graph edge to another kb document, still use an SPA permalink
(`/a/<kb>/<id>`) — see **Cross-artifact links** below.

### GFM features

Tables, strikethrough, task lists, footnotes, autolinks, superscript,
and description lists are on. Fenced code blocks (e.g. ` ```rust `) get
server-side syntect highlighting with inline styles — no client JS and
no CSP concern. The `.md` source must still be UTF-8.

### Callouts

Obsidian-style callouts render as styled boxes. Open a blockquote with
`[!type]` and an optional title; the body keeps full Markdown:

````markdown
> [!warning] Heads up
> Body **markdown** renders normally inside the callout.
````

Recognised types (each gets an accent colour; an unknown type still
renders with the default style): `note`/`info`,
`abstract`/`summary`/`tldr`, `tip`/`hint`/`important`,
`success`/`check`/`done`, `question`/`help`/`faq`,
`warning`/`caution`/`attention`, `danger`/`error`/`failure`/`bug`,
`example`, `quote`. The title defaults to the capitalised type when
omitted; a fold marker (`> [!note]-`) is accepted but kb always renders
callouts expanded. Rendered identically server-side (`markdown.rs`
`rewrite_callouts`) and in the SPA's native note view (`NoteMarkdown`'s
`rehypeCallouts`).

## Cross-artifact links

Only `<a href>` is followed for the artifact graph. Two shapes are
recognised (`parser::parse_artifact_link`):

- `<a href="/a/<kb>/<source-relative-path>">` — SPA permalink (e.g.
  `/a/mykb/pm/01-timeline.html`). A bare 12-hex artifact id in place
  of the path also works (`/a/mykb/3aefc41d2b90`); the path form is
  resolved to the same id deterministically (ids are
  `sha256(rel_path)[:12]`), so both produce identical graph edges —
  prefer the readable path form.
- `<a href="http://<id>.artifacts.localhost[:port]/...">` — iframe
  canonical URL.

Anything else (external http(s), `mailto:`, `#fragment`,
`javascript:`) is ignored for the graph but renders fine. Use
SPA-permalink links when cross-referencing other kb artifacts so
the cross-artifact graph captures the edge.

**All three shapes stay in-app in the reader** (linkflow, 2026-08):
relative links go through the trampoline as before; subdomain and
SPA-permalink links are relayed to the parent SPA, which navigates
cleanly instead of letting the iframe wander off on its own. A
`#fragment` on a cross-artifact link now lands on that section
(forwarded as the `?sec=` deep link), so prefer
`other.html#section-id` over bare `other.html` when you mean a
specific section. Hovering any cross-artifact link in the reader
shows a peek card (title, tags, excerpt) before committing to the
jump — one more reason to give artifacts real titles and stable
section ids.

**Sharing keeps these working.** When you `kb share` a folder (or a
`--local` offline bundle), cross-artifact links between two artifacts
that are BOTH inside the share are rewritten to relative file paths,
so the static export is self-contained — no daemon needed. Links to
artifacts OUTSIDE the share are "danglers": left as-is by default
(`--links warn`) or bounced to the live instance with `--links
absolute`.

## Code references (DCB v1)

Text naming source files kb-code indexes (`app/models/order.rb:42`,
`CartsController#update`, `github.com/org/repo/issues/123`) is extracted
deterministically at index time — no LLM, closed grammar
(`kb_core::coderefs`) — and resolved LIVE against a picked checkout when the
doc is read in the SPA's Links tab. Resolution never happens at index time and
is never cached: kb only ever stores the RAW HINT (what the text said), never
a verdict (whether it's actually there). Extraction runs over both HTML and
Markdown artifacts (the rendered `<code>` elements either way), never over the
`<template id="kb-prompt">` prompt block.

**Two authoring conventions, prospective (the back catalog is inference-only
— see `kb refs --lint` below):**

- **Declared refs bypass inference.** `<code data-kb-ref="path[#Lstart[-Lend]]">text</code>`
  is extracted as-is, never re-parsed by the grammar — write the exact
  repo-relative path once and stop worrying about the extraction grammar
  guessing wrong. Verification still runs at READ time: a typo'd
  `data-kb-ref` renders `declared but absent` in the reader, loudly, on
  purpose (doc-rot detection — the convention never silently trusts you).
  Markdown authors: a plain backtick code span (`` `path/to/file.rb:42` ``)
  still gets scanned by inference like any other `<code>`, since Markdown
  backticks render to `<code>` before extraction runs; to get the
  DECLARED-ref bypass in a `.md` file, drop in raw HTML — Markdown passes it
  through unchanged (same mechanism the Headings/ids section above uses for
  `<section id="…">`): `` <code data-kb-ref="app/models/order.rb#L42">order.rb:42</code> ``.
- **`<meta name="kb-code-rev" content="<repo-label>@<sha>[+dirty]">`** — a
  THIRD recognized meta name (the "Metadata via `<meta>` tags" section above
  documents `kb-category`/`kb-tags` as the only two the general indexer
  reads; `kb-code-rev` is read separately, by the code-ref extraction pass
  only, not the general metadata pipeline). When present, kb-code can
  confirm line numbers against that exact commit (blame-remap) rather than
  guessing from doc prose. `<repo-label>` matches a `name` in the reader's
  kb-code repo list; `+dirty` (uncommitted working tree at authoring time)
  explicitly skips the remap — there is no committed content to blame
  against.

**Check what you wrote**: `kb refs <doc> --lint` reports every INFERRED-but-
undeclared reference in a doc — the authoring-loop check before hand-adding
`data-kb-ref` to the ones worth pinning.

**What gets extracted** (closed set — nothing outside this list becomes a
link): `path:line`, `path:line-line`, `path:5,19,29` (comma line-lists — **one
reference carrying every cited line**, not one ref per line number: the
rail/W3 "cited by" index would triple-count a single `<code>` element
otherwise, `10-w1a-kb-core.md` §2.3/§14.2), directory-prefixed paths
(`app/…`, `lib/…`, `config/…`, `spec/…`), bare `basename.ext` on a
whitelisted extension, `Namespace::Class` (a LITERAL `::` required — a bare
capitalized word like `Product` or `EUR` never extracts, write
`Store::Product`), `Class#method`, the combined `Namespace::Class#method`,
and GitHub issue links (`<a href="…/issues/N">`, never a bare `#NNNN` — those
are CSS hex colours in this corpus far more often than issue refs). Gem/vendor
paths (`algolia-3.12.2/lib/…`) extract as `external` — never resolved against
your app repo. `.html`/`.md` paths are never a code ref, even inside `<code>`
— those are sibling kb docs; use a normal link or a `[[wikilink]]`. See
`kb_core::coderefs` module doc for the full grammar and the hard-reject list
(Algolia event names, UUIDs, regex literals, SQL, and similar `<code>`
content that isn't a reference at all).

## Anchors that survive regeneration

kb-comments anchors fuzzy-resolve against the DOM. Four scopes,
ranked by stability (`review.rs`):

1. **File** — whole artifact. Always survives.
2. **Chapter** — heading-path Jaccard match (threshold 0.5). Stable
   if you keep heading *text* mostly stable across regens.
3. **Section** — exact match against `id` or `data-kb-id`, OR a
   synthetic `<slug>-<tag>-<nth>` fallback. Real ids are far more
   resilient than synthetic; the `nth` counter shifts whenever
   sibling order changes.
4. **Selection** — CSS path + Jaro-Winkler text snippet match
   (threshold 0.85). Most fragile.

**Author for stability**:

- **Stable ids are the durable foundation.** A `Section` anchor on a
  real `id` / `data-kb-id` re-resolves by *exact* match, so it survives
  edits that fuzzy Chapter/Selection anchors would lose. Give every
  element a reader might comment on — sections, major headings, key
  paragraphs, callouts, figures — an explicit, meaningful
  `id="…"` (or `data-kb-id="…"`).
- Don't use UUIDs, hashes, timestamps, or random class names in ids
  or text snippets — they break fuzz-match.
- Keep heading text consistent across regens (typos OK; full
  rewrites break Chapter anchors).
- **Preserve these ids when editing.** The comment↔location binding
  rides on them: an edit that keeps the `id`/`data-kb-id` and heading
  text of a commented element keeps its anchor exact. The id is the
  contract — change the prose freely, keep the handle.

When an element a comment is anchored to genuinely has to move or be
renamed, the anchor isn't lost: the editor re-points it with
`kb comments reanchor <comment_id> --anchor section:<new-id>` (R9 — an
explicit, ground-truth re-point). The indexer's automatic resolver never
rewrites an anchor on its own; it keeps the original and flags it stale
if it can't re-find it. So: author with stable ids, preserve them across
edits, and reanchor on the rare deliberate move.

## Iframe / parent-app constraints

kb serves each artifact from its own `<id>.artifacts.localhost`
subdomain inside an iframe in the parent SPA (`iframe.rs`,
`kb-server/src/scrub.rs`). The daemon injects two scripts:

- `/_kb/probe.js` (always) — detects multi-page navigation and
  notifies the SPA via `postMessage`.
- `/_kb/annotate.js` (when `?cm=on`) — the kb-comments annotator.

What **breaks** in the iframe:

- `<base href>` at the document root — relative URLs resolve wrong
  under subdomain serving.
- `target="_top"` on links — navigates the entire SPA away from the
  artifact route. Use `target="_blank"` or omit.
- `window.parent.document.*` access — parent is cross-origin
  (different subdomain), throws SecurityError. Use `postMessage` if
  you really need parent communication.
- `position: fixed` on `<body>` — confuses the annotator's
  `getBoundingClientRect + scrollY` marker placement
  (`web/src/scripts/annotate.ts`). Fixed on child elements is fine.

What is **safe**:

- CSS Grid, Flexbox, custom properties, Shadow DOM (for non-
  annotated content).
- Custom fonts — inline data-URIs or `<link>` from CDN. No CSP is
  enforced by kb.
- External `<script src="https://…">` (e.g., mermaid via jsDelivr).
  No CSP block.
- Multiple pages — relative sibling links work, the probe wires up
  postMessage navigation to the SPA.

## Suggested patterns

- **One `<h1>` per artifact**, matching `<title>`. The gallery card
  uses title; the artifact page sees the `<h1>`.
- **Code blocks**: `<pre><code class="language-…">…</code></pre>`.
  Outer `<pre>` count drives the gallery glyph strip; inner `<code>`
  text is preserved for BM25 (`parser.rs:160, 250`).
- **Long-form**: ≥1500 body words trips the `longread` flag
  (`parser.rs:252`) and shows the corresponding gallery glyph. Don't
  pad short artifacts to hit it.
- **Multi-page**: link sibling pages with relative URLs
  (`<a href="part-2.html">`). The probe handles cross-page
  notification (`iframe.rs`). Subfolders are fine:

  ```
  foo/                          ← artifact tree
  ├── _assets/styles.css        ← shared by every page below
  ├── README.html               ← entrypoint
  ├── steps/INDEX.html          ← <link href="_assets/styles.css">
  ├── research/INDEX.html       ← <link href="_assets/styles.css">
  └── archive/INDEX.html        ← <link href="_assets/styles.css">
  ```

  The daemon walks up from the requesting page's folder toward the kb
  source root looking for the requested asset, so chapters at any
  depth pick up `foo/_assets/styles.css` automatically. First match
  wins — a nearer `_assets/` shadows a further-up one. Browser
  normalisation strips `../` before the request reaches the daemon,
  so write the href as `_assets/styles.css`, not `../_assets/…`.
- **Mermaid / diagrams**: render via `<script type="module">` and a
  `<div class="mermaid">…</div>` — no CSP block, but keep the
  trigger inside the artifact, don't depend on parent.
- **Accessibility**: keep semantic landmarks (`<main>`, `<nav>`,
  `<aside>`) — they don't affect indexing but help the annotator's
  synthetic-id walk find a coherent ancestor.

## Visual floor (verified outcomes, free values)

Report-only like everything visual here — but *checked*. The five rules
above are the hard structural contract; this is the one *visual* layer.
Since **2026-07-03** it is governed as **"verified outcomes, free
values"** — an evolution of June's paste-this-block floor, adopted after
a month of production data showed the pasted default palette sticking
verbatim (13 artifacts shipped `--accent:#3a5a9f` unchanged; the "free
accent override" went unused in practice). Original rationale and
evidence:
[Standardize or stay free?](research/standardizing-claude-generated-frontend-for-kb.html)
(now carrying the 2026-07-03 ruling note).

The contract is a small **interface plus five outcomes**, not a stylesheet:

- **Token roles, your values.** Express the palette through the standard
  role names — `--bg --surface --fg --muted --rule --accent --accent-fg`
  (plus `--warn --danger --ok` when used) — with **any** values: any
  hues, `#hex` / `rgb()` / `hsl()` / `oklch()`, light *and* dark. The
  names are the contract because they are what makes an arbitrary
  palette statically checkable.
- **AA by verification.** Every known role pair (`--fg` on `--bg`,
  `--muted` on `--surface`, `--accent-fg` on `--accent`, …) must compute
  ≥4.5:1 (WCAG AA) in both themes. The kb-artifact skill's
  `scripts/contrast-check.py` (driven by `validate-kb.sh`) computes the
  ratios and warns on failures — report-only, never blocking.
- **A11y outcomes.** Honour `prefers-reduced-motion`; a visible
  `:focus-visible` ring; a working skip-link (an
  `<a class="skip-link" href="#target">` whose target id exists — markup,
  not just CSS); body font ≥16px.
- **Native primitives.** Interactive elements are real `<button>`/`<a>`,
  never a styled `<div>` (first rule of ARIA).
- **Everything else is free** — layout, hues, type pairing, density,
  illustration, voice. Design each artifact its own look; the checker
  proves the floor.

One **example** that passes — a starting point to depart from, not a
house style (the validator flags shipping this palette verbatim):

```css
/* ── kb visual floor example — keep in sync with the kb-artifact skill's assets/style.css ── */
:root{
  /* COLOR — one verified example; EVERY value is a free per-artifact choice.
     Keep the ROLE NAMES; contrast-check.py verifies your values (≥4.5:1 on the known pairs). */
  --bg:#fbfbfa; --surface:#f3f3f1; --fg:#1b1b1a; --muted:#5f5f5c;   /* muted/bg 6.19:1 */
  --rule:#d9d9d6; --accent:#3a5a9f; --accent-fg:#ffffff;            /* accent/bg 6.45:1 · white/accent 6.68:1 */
  --warn:#8a5f00; --danger:#9a2f28; --ok:#256148;                  /* ≥5.08:1 on --bg/--surface */
  --fs-body:1.0625rem; --fs-small:.85rem; --lh:1.6; --maxw:46rem;  /* 17px reading floor */
  --sans:system-ui,-apple-system,"Segoe UI",Roboto,sans-serif;
  --serif:ui-serif,Georgia,"Times New Roman",serif;
  --mono:ui-monospace,"SF Mono","Cascadia Code",Menlo,Consolas,monospace;
  --radius:6px; --focus:2px solid var(--accent);
}
@media (prefers-color-scheme:dark){
  :root{
    --bg:#16171a; --surface:#1f2126; --fg:#e6e6e3; --muted:#a3a39e; /* muted/bg 7.08:1 */
    --rule:#2c2e33; --accent:#8fb0ee; --accent-fg:#11131a;          /* accent/bg 8.20:1 */
    --warn:#e0b24d; --danger:#f08079; --ok:#6fcaa0;                 /* ≥6.18:1 on --bg/--surface */
  }
}
@media (prefers-reduced-motion:reduce){
  html{scroll-behavior:auto}
  *,*::before,*::after{animation-duration:.001ms!important;transition-duration:.001ms!important}
}
body{margin:0;background:var(--bg);color:var(--fg);font-family:var(--sans);
  font-size:var(--fs-body);line-height:var(--lh)}
:focus-visible{outline:var(--focus);outline-offset:2px}
/* SKIP-LINK — also needs markup: <a class="skip-link" href="#main">Skip to content</a> + <main id="main"> */
.skip-link{position:absolute;left:-9999px;top:0;background:var(--fg);color:var(--bg);
  padding:.5rem 1rem;z-index:100;font:600 var(--fs-small)/1 var(--mono);
  text-transform:uppercase;letter-spacing:.05em;text-decoration:none}
.skip-link:focus{left:.5rem;top:.5rem}

/* NATIVE PRIMITIVES — interactive elements are real <button>/<a>, never styled <div> (first rule of ARIA) */
.btn{font:600 var(--fs-small)/1 var(--mono);letter-spacing:.08em;text-transform:uppercase;cursor:pointer;
  display:inline-flex;align-items:center;gap:.4rem;padding:.5rem .8rem;
  border:1px solid var(--rule);border-radius:var(--radius);background:transparent;color:var(--fg)}
.btn:hover{border-color:var(--accent);color:var(--accent)}
.btn:focus-visible{outline:var(--focus);outline-offset:2px}
.btn[disabled]{opacity:.5;cursor:not-allowed}
.btn--primary{background:var(--accent);color:var(--accent-fg);border-color:var(--accent)}
.callout{font-family:var(--serif);border:1px solid var(--rule);border-left:4px solid var(--accent);
  border-radius:var(--radius);background:color-mix(in srgb,var(--accent) 7%,var(--bg));padding:.8em 1em;margin:1.2em 0}
.callout-label{font:650 .72rem/1 var(--sans);letter-spacing:.05em;text-transform:uppercase;
  color:var(--accent);display:block;margin-bottom:.3em}
.callout--warn{border-left-color:var(--warn);background:color-mix(in srgb,var(--warn) 9%,var(--bg))}
.callout--warn .callout-label{color:var(--warn)}
.callout--danger{border-left-color:var(--danger);background:color-mix(in srgb,var(--danger) 9%,var(--bg))}
.callout--danger .callout-label{color:var(--danger)}
.callout--ok{border-left-color:var(--ok);background:color-mix(in srgb,var(--ok) 9%,var(--bg))}
.callout--ok .callout-label{color:var(--ok)}
code{font-family:var(--mono);font-size:.88em;background:var(--surface);border-radius:4px;padding:.08em .35em}
pre{background:var(--surface);border:1px solid var(--rule);border-radius:8px;padding:1rem;overflow:auto;font-size:.85rem;line-height:1.5}
pre code{background:none;padding:0}
table{border-collapse:collapse;width:100%;font-family:var(--sans);font-size:.9rem;margin:1em 0}
th,td{border:1px solid var(--rule);padding:.5em .7em;text-align:left;vertical-align:top}
th{background:var(--surface);font-weight:600}
a{color:var(--accent);text-underline-offset:2px}
a:focus-visible{outline:var(--focus);outline-offset:2px;border-radius:2px}
a[target="_blank"]::after{content:" \2197";font-size:.8em}
/* ── end kb visual floor ── */
```

**Why these specifics:**

- **Contrast is guaranteed by verification, not by pinned values.** Low
  contrast is the #1 web-accessibility failure and a pure styling
  decision — computing every known role pair (≥4.5:1, WCAG AA, light and
  dark) removes it as a per-artifact risk while leaving the palette
  itself a per-artifact design choice.
- **Interactive elements are real `<button>`/`<a>`, never styled
  `<div>`** (the first rule of ARIA): role, keyboard, focus, and
  `disabled` come for free; a `div+role` makes all of that your
  per-artifact responsibility.
- **The skip-link needs markup, not just CSS.** The `.skip-link` rule
  is inert without `<a class="skip-link" href="#main">Skip to
  content</a>` as the first `<body>` child and a matching `<main
  id="main">` (the skeleton below seeds both).
- **Callout tints use `color-mix()`** — well-supported; an older engine
  renders a flat background instead, and body text sits on the
  un-tinted background, so its contrast is unaffected. The tinted
  callout *labels* were also contrast-checked (≥4.8:1, AA).
- **Reading size ≥16px** (`--fs-body` is 17px) and `prefers-reduced-motion`
  is honored.

The **checker is canonical**; this block is only the worked example (the
`kb-artifact` skill's `assets/style.css` mirrors it, hand-synced — both
carry the `── kb visual floor ──` markers). The `~/project/research`
catch-all corpus keeps its own `_template/style.css` house style and is
intentionally **not** governed by this floor — per-corpus consistency is
a legitimate corpus-level choice, and a corpus with its own house
stylesheet keeps it.

## Don'ts (quick list)

- Don't hide substance inside `<script>`, `<style>`, `<template>` —
  invisible to embeddings and BM25.
- Don't generate UUID/timestamp ids — anchors die on regen.
- Don't put a prompt anywhere other than `<template id="kb-prompt">`
  — it leaks on public URLs.
- Don't add a `<base href>`, `target="_top"`, or assume parent-doc
  access.
- Don't exceed 8 KB in `<template id="kb-prompt">` — silently
  truncated.

## Minimal correct skeleton

```html
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>How kb resolves anchors across regens</title>
  <meta name="kb-category" content="notes">
  <meta name="kb-tags" content="kb, anchors, design">
  <template id="kb-prompt">
    Explain kb-comments anchor resolution in ~800 words for a
    backend engineer joining the project.
  </template>
  <style>/* define the floor token roles with YOUR palette (see Visual floor), then your layout */</style>
</head>
<body>
  <a class="skip-link" href="#main">Skip to content</a>
  <main id="main">
  <h1>How kb resolves anchors across regens</h1>
  <section id="overview">
    <h2>Overview</h2>
    <p>…body text the embedder will see…</p>
  </section>
  <section id="fuzzy-resolver">
    <h2>The fuzzy resolver</h2>
    <pre><code class="language-rust">
fn resolve(anchor: &amp;Anchor, dom: &amp;Dom) -&gt; Option&lt;Range&gt; {
    …
}
    </code></pre>
  </section>
  </main>
</body>
</html>
```
