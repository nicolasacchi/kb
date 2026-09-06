# corpus/

Real HTML artifacts used by spike code (unit + manual validation) and, later, by production tests.

## Layout

- **`canon/`** — verbatim copy of `kb-research/sample-artifacts/`. **Frozen.** Do not edit these files. They are the executable specification — what the indexer's pipeline must handle.
  - `fullscreen-viz.html` — single-file interactive (drag-to-resolve borrow checker)
  - `kitchen-sink.html` — component stress test (dialogs, sliders, drag-reorder, zoom)
  - `multi-page.html` — 4 chapters, sticky TOC
  - `cost-of-abstraction.html` — 4-page essay with backlinks
  - `pm/` — multi-file postmortem (summary, timeline, root cause, action items, shared CSS)

- **`curated/`** — additional Claude-generated HTML artifacts you drop in. Starts empty.

## Rules for adding artifacts to `curated/`

1. **Single-file, OR a directory.** Prefer a single `.html` file with all CSS/JS inline. Multi-file artifacts go in a subdirectory `<slug>/` with a `00-summary.html` (or `index.html`) entry point + sibling pages + shared `style.css`.
2. **No external resources required to render.** Fonts and CDN scripts that the artifact *wants* are fine (the iframe sandbox allows them), but the artifact must remain usable offline.
3. **Naming convention.** `<short-slug>.html` (kebab-case). For multi-file: `<short-slug>/` directory.
4. **No secrets.** These artifacts are committed to a git repo.

## Pointing spike code at a specific corpus

Every spike reads the `KB_CORPUS` env var, defaulting to `corpus/canon`:

```bash
just spike walker                                          # uses corpus/canon
KB_CORPUS=corpus/curated just spike walker                 # uses corpus/curated
KB_CORPUS=/some/other/path just spike walker               # uses arbitrary path
```
