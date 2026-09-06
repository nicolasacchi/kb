---
name: kb-artifact
description: Use when producing a standalone deliverable to be served and searched by a kb daemon — research reports, design reviews, RFCs, investigation write-ups, options analyses, deep dives. Authors one self-contained HTML file that satisfies the kb authoring contract, drops it into a kb corpus, and lets the daemon index it.
---

# Authoring kb-indexed HTML artifacts

A kb daemon indexes a corpus of self-contained HTML artifacts and serves
them with hybrid (BM25 + vector) search, an atlas, comments, and history.
When you produce a standalone deliverable for the user to read later, write
it as **one self-contained HTML file** that satisfies the contract below,
then drop it into a kb corpus's source directory — the daemon's watcher
indexes it automatically (no manual trigger).

## When this applies

**Yes:** research / investigation reports, design reviews, RFCs, options
write-ups, architecture decisions, long-form analyses, deep dives — anything
the user will want to find and read later.

**No:** code, commit messages, PR descriptions, in-repo `README`/`CHANGELOG`,
short answers that fit in chat.

## The authoring contract (the five must-knows)

1. **Wrap any model prompt that produced the doc in
   `<template id="kb-prompt">…</template>`** (8 KB cap). The daemon strips it
   on non-loopback serve, so it's safe to include but never leaks to public
   URLs. Don't repurpose that id.
2. **Real `<title>` + a heading hierarchy** (`<h1>` then `<h2>`…). BM25
   indexes title / headings / body as separate fields, and comment
   anchors fuzzy-match heading text.
3. **Stable section ids** (`id="foo"` or `data-kb-id="foo"`) — no UUIDs,
   timestamps, or random class names. Anchors die on regeneration otherwise.
4. **No `<base href>`, no `target="_top"`, no `window.parent.*`.** Artifacts
   are served from sandboxed per-artifact subdomains; the parent SPA is
   cross-origin and these break it. Inline all CSS/JS; no external resources.
5. **Tag + categorise via `<meta>`:**
   `<meta name="kb-tags" content="rust, atlas, design">` and
   `<meta name="kb-category" content="research">` (or `notes`, `review`,
   `reference`). Tags are slugified (lowercase, alphanumeric + dash).

A hand-authored artifact like this is **not** auto-linked to the session
that produced it — that only happens for `kb notes new` (which stamps
`kb-session` from the live session marker unless `--no-session`). If you
want this doc joined to the current Claude Code session on the SPA's
Sessions tab, add `<meta name="kb-session" content="<session-id>">`
yourself (see `docs/authoring-artifacts.md` § Session provenance).

Keep the file kebab-case and descriptive
(`agent-native-web-mvp-options.html`, not `report.html`) — the artifact id
is derived from its source-relative path, so the name is permanent.

## Visual floor (verified outcomes, free values)

One *visual* contract beyond the five structural must-knows — **"verified
outcomes, free values."** Design each artifact its **own** palette and look; the floor is a
small checkable interface, not a stylesheet:

- **Token roles, your values:** express the palette through the standard role names —
  `--bg --surface --fg --muted --rule --accent --accent-fg` (+ `--warn --danger --ok` when
  used) — with **any** values (any hues; `#hex`/`rgb()`/`hsl()`/`oklch()`; light *and* dark).
  The names are the contract because they make an arbitrary palette statically checkable:
  every known role pair must compute ≥4.5:1 (WCAG AA) in both themes.
- **A11y outcomes:** honour `prefers-reduced-motion`; a visible `:focus-visible` ring; a
  *working* skip-link — an `<a class="skip-link" href="#target">` whose target id exists
  (markup, not just CSS); body font ≥16px.
- **Native primitives:** interactive elements are real `<button>`/`<a>`, never a styled
  `<div>` (first rule of ARIA); tables use `<th scope>`; callouts carry a text label, never
  colour alone.

A worked example that passes — plus the full outcome list and the checker
(`contrast-check.py`, driven by the kb-artifact validator where installed) — is the
**"Visual floor"** section of the kb repo's `docs/authoring-artifacts.md`. Shipping the
example palette verbatim defeats the point (and is flagged where the validator runs): verify
the floor; design the **ceiling** (layout, hues, type pairing, density, illustration, voice)
per artifact.

## Scope

This skill governs **reading documents** (research / RFC / review / analysis). If the
`frontend-design` skill is also active, its per-artifact-distinctiveness instinct is welcome
**above** the floor (palette, type pairing, layout, voice) — but the verified outcomes here
(AA ratios, native elements, reduced-motion, focus, ≥16px) remain authoritative for kb
document artifacts. A corpus with its own house stylesheet (e.g. a `_template/style.css`
shared by every artifact in that corpus) keeps it — per-corpus consistency is a legitimate
corpus-level choice.

## The author → index flow

1. **Pick the corpus.** Discover what's available with the CLI manifest
   (`/kb-tools`, or `kb tools`), then list corpora — each kb's source dir is a
   bind-mounted folder on disk.
2. **Write the HTML file** into that corpus's source directory following the
   contract above.
3. **Indexing is automatic** — the daemon watches each corpus's source
   folder and reindexes a changed file within seconds; no command is needed
   once the file lives in a registered corpus. To force a full re-scan, use
   `kb reindex --kb <name>`. (`kb add <folder> --kb <name>` is a separate,
   one-time setup step that registers or re-points a corpus's whole source
   folder in `kb.toml` — it takes a directory, not a single file, and is not
   how you add one artifact.)
4. **Verify** with `kb find <filename>` (prints the 12-hex artifact id) or
   `kb search "<query>" --kb <name>`.

Run `/kb-tools` first if you're unsure of the exact verb syntax — it walks
the live `kb` command tree, so it always matches the installed binary.
