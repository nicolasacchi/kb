# The web UI

The daemon serves a single-page React app at its root (default
`http://127.0.0.1:4000`). It browses, searches, reads, and reviews the
artifacts the daemon has indexed, across one or more daemons. This is a
user guide; the routes behind it are in the README's HTTP API section.
For the component/hook reference, see
[`web-internals.md`](web-internals.md).

## Layout & routes

| Route | View |
|---|---|
| `/` | Gallery — the filterable artifact browser (grid / list / atlas / history). |
| `/a/<kb>/<path>` | Detail — one artifact, rendered in a sandboxed iframe. The path is the source-relative path, so these are stable, shareable permalinks. |
| `/replay/<kb>/<sid>` | Session replay — one captured session, scrubbed beat by beat (see below). |
| `/slates` | Slates — every coordination board, ranked by attention (see below). |
| `/slates/<slug>` | One slate's board. `?topic=` filters, `?history=1` opens the history drawer. |
| `/stale-anchors` | Stale-anchors dashboard — comments whose anchors broke on reindex, fleet-wide. |
| `/settings` | Theme / accent / density and the daemon list. |

Gallery state (view, kb, filters, sort, group, folder) lives in the URL,
so any view is a shareable/reloadable link.

### Bundle & code-splitting

Each route view is `React.lazy`-loaded (one chunk per route, fetched on
first navigation behind a single `<Suspense>` at the route boundary), and
the atlas canvas is lazy even within the gallery. The persistent chrome
(header, rails, status bar, command palette, the SSE SharedWorker wiring)
stays eager so the initial paint and the realtime stream never flash. Vite
also vendor-splits `node_modules` into long-cache chunks (`codemirror`,
`markdown`, `react-vendor`, `tanstack`) via `manualChunks` in
`web/vite.config.ts`. The upshot: the initial entry chunk is ~85 KB
(was ~1.3 MB) and the heavy editors/renderer (CodeMirror, react-markdown)
load only on the detail/notes routes that use them.

There is a **second Vite entry**, `web/sketch.html` → `dist/sketch.html`
(`build.rollupOptions.input.sketch`). It is the slate board's drawing
frame: it bundles mermaid and is loaded ONLY by URL, into an
`<iframe sandbox="allow-scripts">`. Because it is reachable only from
`src/sketch/main.ts` and never imported by the SPA, mermaid (~3 MB
unminified across its lazy per-diagram chunks) stays entirely out of the
`main` graph — verify with `grep -o 'href="/assets/[^"]*"' dist/index.html`,
which lists only `preload-helper`, `react-vendor`, `tanstack` and the CSS.
Mermaid is deliberately given **no** `manualChunks` bucket: naming one makes
Rollup hoist Vite's shared `__vite__preload` helper into it, at which point
every route chunk carries a hard `import "./mermaid-*.js"` and the shell
pays for a renderer it never calls.

## Gallery

Four view modes (toggle in the top bar):

- **Grid** / **List** — virtualized so large corpora stay smooth. Cards
  show title, tags, age, a reading-progress chip, and capability glyphs
  (code-heavy, longread, multi-page…).
- **Atlas** — the 2-D semantic map (see below).
- **History** — the activity timeline (see below).

Filter and arrange via the left rail and top-bar controls:

- **Tags** — click chips to filter (any-of). Counts come from
  `/api/kb/{kb}/tags`.
- **Folders** — a breadcrumb trail + an expandable folder tree;
  selecting a folder filters to it and its descendants.
- **Capabilities**, **date range** (7d / 30d / all), and an
  **index-only** toggle.
- **Sort** by recent / indexed / title / words, with a direction toggle.
- **Group** flat or by folder.
- **Search** — open the command palette (the magnifying glass or
  ⌘K/Ctrl-K). Pick a mode: **hybrid** (BM25 + vector, the default),
  **keyword** (BM25 only), or **semantic** (vector only). Hybrid and
  semantic need an `embedding_model` configured on that kb.

### Reflection canvas (`?view=canvas`)

A companion to the History view: four synchronized UTC-day tracks —
creation, reading, sessions, comments — sharing one draggable brush, so
you can see a burst of activity across all four facets at a glance
instead of one timeline at a time. Reached via `?view=canvas` directly,
or the "the same days as four density tracks →" crosslink on the History
view; there's no separate top-bar toggle. Drag (or use the keyboard —
each end of the brush is a `role="slider"` handle, arrows/PageUp/
PageDown/Home/End) to select a range, then pivot into a normal gallery
list scoped to exactly what's brushed. The brush itself never refetches;
only activating the pivot re-asks the server, and a full-window pivot is
usually a cache hit. Big or mixed-lane brushes degrade honestly (fall
back to a `from`/`to` window, or say the pivot's unavailable) rather than
silently truncating — see invariant #35.

### Map-home shell (`?shell=map`)

An alternative gallery home that makes the atlas the primary navigation
surface instead of the grid: the map fills the view, and a side panel
lists whatever's selected (lasso or click), with a pivot into a normal
gallery list via the same `ids=` filter atom the atlas lasso already
uses. **This is opt-in and OFF by default** — reachable only via an
explicit `?shell=map` URL or the Settings → Preferences "home" toggle.
It is deliberately not promoted anywhere else in the nav: it's an
evidence-gated experiment (a local usage census decides whether it's
ever promoted to default), and giving it a prominent entry point would
manufacture the very evidence that census exists to measure. Desktop
only — on mobile it degrades to the ordinary gallery.

## Detail view

The artifact renders inside a cross-origin sandboxed iframe served from
`<id>.artifacts.<suffix>` (so artifact scripts can't touch the parent
SPA). A floating pill provides:

- **Sibling navigation** — prev/next within the artifact's folder.
- **Reading progress** — your scroll position is persisted per visit (a
  30-minute window) and resumes when you reopen.
- **Multi-page artifacts** — relative sibling links navigate in place;
  the active page is reflected in the URL (`?p=`).
- **Annotate mode** — the pen icon toggles the comments layer on/off.

### Reviewing (comments)

With annotate mode on, select text (or a heading/section) and add a
comment. A right-side panel lists the artifact's comments, their
anchors, status, and threaded replies, and refreshes live as comments
change. You can reply, resolve/reopen, and export the review (Claude
prompt / JSON / Markdown).

This is the human end of the realtime loop in
[`comment-workflow.md`](comment-workflow.md): comments you add here fire
an SSE event that `kb comments watch` picks up, and when Claude replies
or resolves, this panel updates without a refresh.

### Two-pane reader (`?pane2=`) and provenance registers

`w v` splits the reader, opening the next sibling artifact beside the
current one (`w q` closes it, `w h`/`w l` move focus between panes, `w o`
closes the second pane and refocuses the first — desktop only, reader
scope). The split location lives entirely in the URL (`?pane2=`, on the
primary artifact's own permalink), so it's shareable and survives
back/forward like everything else in the reader; only an artifact can
occupy the second pane, not a search/gallery/list view. Each pane keeps
its own ContextBar (per-artifact chrome — anchor, add-to-list, share,
comments), but there is still exactly one inspector rail for the whole
reader, keyed to whichever pane has focus.

Separately, **provenance registers** (`"` then a–z to store, `'` then
a–z to paste) are 26 letter-keyed slots — browser-local, never sent to
the server — each holding a reference to an artifact position, a text
selection, a session, or a commit. Pasting copies a citation (title +
permalink, or the richer selection-quote form) to the clipboard; a
register can also be added straight to a reading list from the `?` help
sheet's "Your registers" panel. Registers are the generalization of the
older marks feature (`m`/`` ` ``, which is now just the artifact-position
case of the same store) — one 26-slot store, not two.

## Atlas

A 2-D projection (UMAP, or PCA per `[kb.<name>.atlas]`) of the corpus's
embeddings, colored by k-means cluster. Wheel to zoom, drag to pan; the
legend shows the cluster count. Trigger a fresh layout with **Recompute**
(full PCA/UMAP + k-means) or, to retune only the cluster count cheaply,
**Recluster** (k-means on the existing coordinates). Both run async and
stream progress over SSE. Configure the default cluster count / layout
per kb in [`configuration.md`](configuration.md). Everything below reads
the same map, whether you're looking at it in the Gallery's **Atlas**
view or in the map-home shell above.

**Time-lapse** turns Atlas history into a scrubber. Every recompute/
recluster records a frame (the daemon prunes to the newest 24), and
turning time-lapse on adds a play/scrub control that steps through them
— slow enough to read a handful of frames without touching the scrubber,
capped so a full 24-frame history doesn't outlast a short visit. Frame
coordinates arrive already Procrustes-aligned into the newest frame's
space, so a frame's clusters can be remapped onto today's cluster
colors/labels even though cluster *numbers* aren't stable across
recomputes; artifacts the frame remembers that have since been deleted
are dropped from the draw (and the scrubber says how many). It stops at
the last frame rather than looping, and switching kb always rewinds and
stops — a player carried across a kb switch would be scrubbing someone
else's history. The onion skin (below) is hidden while a past frame is
up, since comparing your field against a past machine layout would be
comparing two different maps.

**My field** onion-skins *your* map over the machine's. Turn it on and the
atlas draws a ghost dot for every artifact you have placed by hand, with a
leader line to where the model put it — the line is the disagreement.
**Place** mode lets you drag a dot to where you think it belongs; that
writes only a JSON Canvas sidecar (`<corpus>/atlas/operator.canvas` — the
same format boards use, editable in Obsidian), never the machine layout,
which stays read-only. `type: "group"` nodes in that file are named
islands and their labels are shown verbatim; kb never invents one.
**Disagreement heat** recolours the dots by displacement. The two fields
are brought into one frame by a Procrustes fit computed on the daemon
(rotation · uniform scale · offset removed), so the numbers here and in
`kb atlas field diff` are the same relative disagreement.

**Loci walk** turns any reading list into a path across the map: each stop
flies the camera to that artifact's dot, and *read here* hands off to the
reader with `?list=&entry=` intact so the queue bar takes over. A tour is
not a new object — it *is* the list — and nothing about the walk is
recorded: no progress, no completion, no streak. The derived read state
you already have is the only progress signal shown. Entries that are
tombstoned or absent from the drawn layout are skipped, exactly as the
queue bar skips them, and the bar says how many.

## History timeline

A newest-first log of your session activity — artifacts opened (with
reading progress), searches run, and comments authored. Filter by kind
(open / search / comment). Rows deep-link back to the artifact (and, for
comment rows, scroll to the comment).

## Session replay

Reached from a "replay" link on a session's row (the sessions view, and
an artifact's Biography inspector tab, for a session that touched it).
`/replay/<kb>/<sid>` puts the session's transcript and the artifact it
touched under one playhead:
a left rail lists the session's narrative beats in order (grouped into
prompt-rooted segments), and the right stage shows whatever artifact was
in play at the current beat, scrolled to the section that beat's line
range fell under (a "detected" best-effort — highlight granularity is
section-level, never byte-exact).

The scrubber is index-space, not time-space — one stop per beat, with
each stop's elapsed gap printed, since a real session is a few dense
minutes buried in hours of idle time and a literal timeline would crush
every interesting beat into a few pixels. There is deliberately no
play/pause or autoplay — you scrub by hand (`←`/`→`, `j`/`k`, `[`/`]` to
jump segment, `Home`/`End`). Scrubbing through a replay is explicitly
**not** a read: it doesn't record reading progress or history rows for
the artifacts it passes through, so browsing a 200-beat session never
pollutes what you've actually opened.

## Slates (the board)

A **slate** is one project's coordination surface: an append-only ledger of
short posts that every agent reads as a text digest (`kb slate open`) and
that this view renders as a board. The board is a *second presenter over the
same projection* — it adds every visual device the injected digest refuses
(emoji, colour, two card sizes, an age fade, columns, swimlanes, drawings)
and **not one fact the CLI cannot print**.

`/slates` lists every slate ranked by **attention** — unacknowledged hands +
open asks + contested takes + stale takes — then by recency, with closed
slates last. That same sum is the badge on the Slates nav row (hidden at
zero). `g b` opens the list. Each row also carries a quiet **`served N`**
count (v0.42, D27): how many sessions have reported a cursor on that slate.
It is attribution, never a read receipt, and it is never a rank term.

`/slates/<slug>` is the board:

- A full-width **NOW band** (`role="status"`) on top: one row per topic with
  its now line, then the WARN rows. A warn never ages out; it only fades.
- Five columns below it — **HANDS · ASKS · TAKES · FOUND + IDEA · TRIED** —
  each a `role="region"` with its name. A **swimlane** toggle turns them into
  a grid with one row per topic plus a general lane (`—`).
- Inside a column: **pinned first, then marks descending, then newest** — the
  same order the digest uses, so the two never disagree.
- **Cards** carry the kind emoji *and* the kind word (nothing is conveyed by
  glyph or colour alone), the line, the author chip (`[you]` · `codex/8f2a` ·
  `job:01M11…`), age, a liveness badge on takes (live · stale? · expired), a
  `contested` badge, a mark ring with its count (who marked, on hover),
  `(was #n)` linking to the superseded ancestor in the history drawer, refs as
  chips (`path:` → kb-code when the kb has a `code_url`, `kb:` → the artifact,
  `post:#n` scrolls and flashes, `session:` → `/sessions`), and a body toggle.
  Card size comes from the projection's `tier` (whole → large, folded →
  compact); opacity fades by age (under 1 h full, under 8 h .85, under 2 d .7,
  older .55, with a floor that keeps AA contrast).
- **`seen by N`** (v0.42, D27) rides whole-tier **NOW · WARN · HAND · ASK**
  cards — in the NOW band and in the columns, from one helper, so the two can
  never disagree — and hovers as *served to codex/8f2a, claude/4b7e*. The
  daemon derives it from the cursors sessions **report** (`POST
  /api/slates/{slug}/cursor`); a read never writes one, the author is
  excluded, and the chip is hidden at zero rather than flashing `seen by 0`.
- **Groundedness captions** (v0.42, D29; SL7f amendment) on `found` and
  `tried` cards: when the active kb (`?kb=`, else the first corpus — the
  SPA's usual active-kb rule) has a `code_url`, each `path:` chip is
  captioned `grounded` / `ungrounded` / `unknown` from kb-code's
  `GET /api/doc-lens/path` (`path_state` + `line_state`: a bare present path
  → grounded; a present path with a cited line → `confirmed` grounded,
  `drifted` ungrounded (the cited line NUMBER is stale — kb-code found the
  content only elsewhere in the file, so the citation as written is no
  longer accurate), `absent` ungrounded; an absent path → ungrounded;
  everything else — ambiguous, external, `unverifiable`, a line asked about
  with no verdict at all, or a kb-code that did not answer — → unknown).
  Every call sends `?repo=<slug>` (a slate slug IS the kb-code repo name by
  design — the only way a multi-repo kb-code can answer at all; a
  `repo_required` 400 anyway captions `unknown` and is never retried without
  `repo`) and, for a ref citing a line, `?context=<the post's own line
  text>` — the only way `confirmed`/`drifted` are reachable rather than the
  honest `unverifiable` default. Computed per render behind a 30 s
  `staleTime` (the same no-SSE-tie exception the doc-lens reads document),
  never persisted, never a score, never in the digest, and never sent to the
  kb daemon. **No `code_url` ⇒ nothing is fetched and no caption renders** —
  an absent kb-code is not an ungrounded ref.
- **Actions**: mark, drop, edit (opens the composer prefilled and submits as a
  supersede), pin/unpin (shown when your identity is the operator), done,
  answer, take. Dropping or editing a live *other* session's coordination post
  asks first, and a daemon refusal (`slate-live-author`) re-asks and resends
  with `anyway`; a `slate-taken` conflict toasts with a **post anyway** action.
  There is no in-place rewrite anywhere — every one of these is an append.
- The **composer** (`post` on the NOW band, or `c`) takes a kind (default
  `found`), a 200-character line with a counter, a topic, refs, a subject for
  take/hand, a failed reason for tried, and a Markdown body. A
  ` ```mermaid ` fence in a body renders as a **drawing** inside
  `<iframe sandbox="allow-scripts">` — no `allow-same-origin`, so the frame has
  an opaque origin (no cookies, no storage, no parent DOM) and the SPA
  document gains no raw-HTML sink.
- The **history drawer** (`h`, or `?history=1`) is the one place a wiped post
  still exists: dropped and superseded posts, struck through, with who hid
  them and why.
- **Keyboard**: `j`/`k` move across cards in reading order, `Enter` unfolds,
  `m` mark, `x` drop, `e` edit, `p` pin, `c` compose, `t` topic filter,
  `h` history.
- **Mobile (≤860 px)** is a reader with a capture slot: one column of
  accordions with counts, the NOW band sticky, the composer and the history
  drawer as bottom sheets (✕ / scrim tap / Esc). The mobile composer posts
  kind, line and topic only, and the card actions are mark, drop and done —
  edit and pin are desktop-only.

## Stale-anchors dashboard

When a reindex breaks a comment's anchor (the text it pointed at moved or
changed), the daemon emits `comment.anchor_stale` and the comment shows
up here, grouped by artifact, with a fuzzy-match score and how long ago
it broke. Fix the artifact (or the comment) and the row clears
automatically when the anchor re-binds (`comment.anchor_resolved`). A red
badge in the top bar counts open stale anchors. This view loads its
initial state from `/api/anchors/stale` (a fleet-wide cold load) and then
stays live over SSE.

## Multiple daemons

The SPA keeps its daemon list in `localStorage` (manage it on the
Settings page; it pings `/api/identity` before adding one). The top-bar
status pill aggregates the worst severity plus total in-flight work, open
errors, and open comments across all configured daemons.
