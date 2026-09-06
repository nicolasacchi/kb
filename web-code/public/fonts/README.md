# Self-hosted fonts (V4.U1)

The kb-code SPA self-hosts every font family it uses — `tokens.css`
declares one `@font-face` per `(family, weight, style, subset)`
combination and no longer pulls faces from a third-party CDN.
This directory is served verbatim under `/fonts/<file>` (Vite's
`public/` passthrough).

web-code ships a subset of kb's `web/public/fonts/` (Inter Tight 300
and Source Serif 4 are omitted — zero consumers here).

| family          | weights (normal)  | italic | subsets          | license     | upstream                                  |
| --------------- | ----------------- | ------ | ---------------- | ----------- | ----------------------------------------- |
| Inter Tight     | 400, 500, 600, 700 | —      | latin, latin-ext | SIL OFL 1.1 | https://github.com/rsms/inter              |
| JetBrains Mono  | 400, 500, 600     | —      | latin, latin-ext | SIL OFL 1.1 | https://github.com/JetBrains/JetBrainsMono |

14 `.woff2` files total (each weight × 2 subsets). Filenames follow
`<Family><Weight>-<style>-<subset>.woff2`, e.g.
`InterTight-600-normal-latin-ext.woff2`. Two `unicode-range`-scoped
`@font-face` rules per weight/style let the browser fetch only the subset a
page's characters actually need (basic Latin vs. Latin Extended-A/B +
assorted punctuation) rather than paying for both up front.

`--font-sans` (Inter Tight) and `--font-mono` (JetBrains Mono) in
`tokens.css` fall back to system fonts, so the SPA still renders
correctly if this directory is ever emptied — self-hosting is about
not depending on a CDN at runtime, not a hard requirement to build.

Why self-hosted at all: the SPA must load offline-ish (corpus-local,
loopback-served, sometimes air-gapped installs) without depending on any
third-party CDN — the same reasoning behind the empty API base URL and the
lack of a service worker.

Each face is distributed under the SIL Open Font License 1.1 by its
upstream project (linked above); redistributing the built `.woff2` subsets
alongside this SPA is permitted under that license. See each upstream repo
for the full `OFL.txt` text.
