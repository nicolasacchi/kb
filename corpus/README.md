# corpus/

Real HTML artifacts used by the test suite (`crates/kb-core`, `crates/kb-server`,
`crates/kb-cli`, `tests/e2e`) and as the bundled sample corpus for a first
`kb add` — see [Try it in 60 seconds](../docs/quickstart.md).

## Layout

- **`canon/`** — a small set of frozen sample artifacts. **Do not edit these
  files.** Tests reference them directly by relative path
  (`corpus/canon` from the repo root), so they are the executable
  specification for what the indexer's pipeline must handle:
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

## Pointing a daemon at a corpus

Register any of these directories as a kb corpus with `kb add`:

```bash
kb add ./corpus/canon --kb canon        # the bundled sample corpus
kb add ./corpus/curated --kb curated    # your own dropped-in artifacts
```
