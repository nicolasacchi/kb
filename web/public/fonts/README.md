# Self-hosted fonts (A-w4/X1, wave-1 wire-don't-build)

The SPA self-hosts every font family it uses — `tokens.css` no longer
`@import`s from `fonts.googleapis.com` and `index.html` no longer
preconnects there. This directory is served verbatim under `/fonts/<file>`
(Vite's `public/` passthrough), and `src/styles/tokens.css` declares one
`@font-face` per `(family, weight, style, subset)` combination, pointed at
the matching file here.

| family          | weights (normal)       | italic | subsets            | license      | upstream                                    |
| --------------- | ----------------------- | ------ | ------------------ | ------------ | -------------------------------------------- |
| Inter Tight     | 300, 400, 500, 600, 700 | —      | latin, latin-ext   | SIL OFL 1.1  | https://github.com/rsms/inter                |
| JetBrains Mono  | 400, 500, 600          | —      | latin, latin-ext   | SIL OFL 1.1  | https://github.com/JetBrains/JetBrainsMono   |
| Source Serif 4  | 400, 500, 600          | 400    | latin, latin-ext   | SIL OFL 1.1  | https://github.com/adobe-fonts/source-serif  |

24 `.woff2` files total (each weight/style × 2 subsets). Filenames follow
`<Family><Weight>-<style>-<subset>.woff2`, e.g.
`InterTight-600-normal-latin-ext.woff2`. Two `unicode-range`-scoped
`@font-face` rules per weight/style let the browser fetch only the subset a
page's characters actually need (basic Latin vs. Latin Extended-A/B +
assorted punctuation) rather than paying for both up front.

`--font-sans` (Inter Tight), `--font-mono` (JetBrains Mono), and
`--font-serif` (Source Serif 4) in `tokens.css` all fall back to system
fonts, so the SPA still renders correctly if this directory is ever emptied
— self-hosting is about not depending on a CDN at runtime, not a hard
requirement to build.

Why self-hosted at all: the SPA must load offline-ish (corpus-local,
loopback-served, sometimes air-gapped installs) without depending on any
third-party CDN — the same reasoning behind the empty API base URL and the
lack of a service worker.

Each face is distributed under the SIL Open Font License 1.1 by its
upstream project (linked above); redistributing the built `.woff2` subsets
alongside this SPA is permitted under that license. See each upstream repo
for the full `OFL.txt` text.
