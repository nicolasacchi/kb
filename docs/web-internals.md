# Web SPA — developer reference

A map of the React app under `web/src/` for contributors: the
components, hooks, API layer, and the cross-cutting conventions. For the
**user-facing** view of what the UI does, see
[`web-ui.md`](web-ui.md); this doc is the internals.

Stack: React 18 + Vite + TypeScript. The daemon serves the built bundle
(`web/dist/`) via `ServeDir` at its root. State lives in the URL
(gallery filters) and `localStorage` (prefs, daemon list); everything
else comes from the daemon over HTTP + SSE.

> This is a snapshot. Prop types and return shapes drift; for the
> authoritative signature of any symbol, read the file — paths are
> given throughout. Regenerate by re-inventorying `web/src/`.

## Directory map

| Dir | What |
|---|---|
| `components/` | Presentational + container components (`*.tsx`) — the tables below cover the architecturally significant ones, not an exhaustive list; the directory has grown well past the original handful as features shipped. |
| `hooks/` | Data/state hooks (`use*.ts`) — same caveat as `components/`. |
| `api/` | daemon I/O: fetchers, types, the SSE manager, prefs, base URL. |
| `lib/` | pure helpers: sort/group, derivations, artifact-host/href, time. |
| `routes/` | Top-level views (gallery, detail, settings, stale-anchors, and — added since this map was first written — slates/board, sessions, notes, lists, memory, search, inbox, replay, ambient). |
| `scripts/` | `annotate.ts` — injected into artifact iframes, not imported by React. |

## Components (`web/src/components/`)

| Component | Renders | Key props |
|---|---|---|
| `TopBar` | Sticky header: kb selector, search pill, view toggle, settings link, stale-anchors badge. | `onOpenCmdk` |
| `LeftRail` | Gallery sidebar filters: tags, folders, capabilities, date range, index-only. Reads URL params. | (reads URL/daemon) |
| `Cmdk` | ⌘K search modal; hybrid/keyword/semantic; navigates to detail on Enter. | `kb?`, `onClose` |
| `Card` | Grid card: title, accent block, glyphs, tags, summary, age, reading chip. | `doc`, `kb`, `progress?` |
| `ListRow` | Compact list row (id-chip, title, folder, category). | `doc`, `kb` |
| `VirtualGrid` | Window-virtualized card grid; computes columns from width; `onEnd` paginates. | `docs`, `kb`, `progress`, `onEnd?`, `hasMore?` |
| `VirtualList` | Window-virtualized list of `ListRow`. | `docs`, `kb`, `onEnd?`, `hasMore?` |
| `GallerySection` | One folder bucket (header + nested grid/list) in grouped view. | `folder`, `docs`, `kb`, `view`, `progress?` |
| `IndexHero` | Featured landing block (hand-authored `index.html`) above the grid. | `docs`, `kb` |
| `AtlasView` | Canvas 2-D scatter of `atlas_x/y/cluster` + edges; pan/zoom; hover title. | `docs`, `kb`, `onNavigate?` |
| `HistoryTimeline` | Chronological opens/searches/comments, grouped by day. | (reads daemon) |
| `FolderTree` | Recursive expandable folder picker for `LeftRail`. | `nodes`, `active`, `onSelect` |
| `FolderBreadcrumb` | `recent › a › b` trail; click to refilter. | `path`, `onNavigate` |
| `SortControl` | Sort-key dropdown (recent/indexed/title/words) + direction. | `sort`, `dir`, `onSort` |
| `GroupControl` | Flat vs folder-grouped toggle. | `group`, `onGroup` |
| `CapabilityGlyphs` | Up to 4 capability glyphs (code/table/svg/interactive/longread). Also exports `glyphsFor(doc)`. | `doc` |
| `TagPill` | Monospace tag pill; `accent` uses the tag color. | `tag`, `accent?` |
| `ReadingChip` | Progress bar + % (or green ✓ when done). | `pct`, `isDone`, `compact?` |
| `chrome/ContextBar` | Detail-view chrome row: back chip · breadcrumb · annotate / comments / anchor / copy-link / share / open-in-tab. Retired the v0.6 `FloatingPill` at v0.12 X1 finish. | `kb`, `id`, `title`, `folder`, `filename`, `isIndex`, `artifactUrl`, `sourceRelative`, `reviewActive`, `annotateMode`, `onToggleAnnotate`, `panelOpen`, `onTogglePanel`, `commentCount` |
| `PreviewInspector` | 320px right-rail on Detail: reading-progress card · identifier kv · metrics · tags · capabilities · neighbors · backlinks. Mutually exclusive with `CommentsPanel` (panel wins when annotate mode is open). | `kb`, `doc`, `progress?` |
| `TocSpy` | Right-edge TOC mini-spy over the iframe; subscribes to `kb:toc` + `kb:section` postMessages from the runtime and posts `kb:scroll-to-id` back on click. Active row gets the 2px accent border-left. | `iframeRef`, `hostSuffix` |
| `AtlasInspector` | 320px right-rail on Atlas (view=atlas): selected-node card · stat list · tag chips · preview / open / ⚓ action row. | `kb`, `doc?`, `onClear` |
| `CommentsPanel` | 360px review panel: file/chapter/section/selection comments, replies, open/resolved filter. | see below |
| `AnnotatorBridge` | Parent↔iframe postMessage relay for the in-page annotator (origin-validated). `forwardRef`. | see below |
| `StatusPill` | Bottom-right aggregated status across daemons; expands to per-daemon metrics. | (reads SSE) |
| `DaemonsManager` | Settings UI to add/remove daemon URLs; pings `/api/identity` before adding. | (reads/writes localStorage) |
| `icons.tsx` | `Icon` — a record of ~28 SVG icon components `(SVGProps) => JSX`. | — |

`CommentsPanelProps` (`components/CommentsPanel.tsx:13`):
`kb`, `artifactId`, `file: ReviewFile`, `loading`, `error`,
`staleCommentIds: Set<string>`, `onSave(next) → Promise<"saved"|"merged">`,
`onRequestFlash(id)`, `onHoverComment(id|null)`, `activeCommentId`,
`annotateMode`, `onToggleAnnotate`.

`AnnotatorBridgeProps` (`components/AnnotatorBridge.tsx:16`):
`iframeRef`, `artifactId`, `hostSuffix`, `fileLabel`,
`onAddComment(next, appended) → Promise<"saved"|"merged"|"error">`,
`file: ReviewFile | null`, `annotateMode`, `onFocusComment(id)`.

## Hooks (`web/src/hooks/`)

| Hook | Signature → return | Talks to |
|---|---|---|
| `useDocs` | `(kb, query: DocsQueryParams, pageSize=200) → {rows, total, isLoading, hasMore, error, loadMore, refresh}` | `GET /docs` (paged, debounced, AbortController) |
| `useSearch` | `(q, mode, kb?) → {hits, ms, loading, error}` | `GET /search` (150ms debounce) |
| `useReview` | `(kb, artifactId) → {file, etag, loading, error, staleCommentIds, save, refetch}` | `GET/POST /review/{id}` + `comments.updated`, `comment.anchor_stale/_resolved` SSE |
| `useReadingProgress` | `(kb?) → Map<id, {pct, isDone}>` | `fetchHistory({kind:"open"})` + `history.recorded` SSE |
| `useStaleAnchors` | `() → StaleAnchor[]` (oldest-first) | `GET /api/anchors/stale` + anchor SSE |
| `useMetrics` | `() → {requestsLastSec, requestsTotal, storageDepth, storageCapacity, routes[], lastTickAt}` | `metrics.tick` SSE |
| `useDaemonStatus` | `() → {phase, inFlight, openErrors, openComments, daemons[]}` | `sse.subscribe` (aggregated) |
| `useArtifactHost` | `useArtifactHostSuffix() → string`; `useIdentity() → Identity\|null` | `GET /api/identity` (cached) |
| `useUrl` | `() → {params: URLSearchParams, set(key, value\|null), navigate}` | React Router (URL state) |
| `useDocumentTitle` | `(title) → void` — sets `document.title` to `<title> · kb` | — |
| `usePillState` | `() → {side, collapsed, toggleSide, toggleCollapsed}` | localStorage `kb:pill` |
| `useSiblingsPrefs` | `() → {sort, includeSubfolders, setSort, setIncludeSubfolders}` | localStorage `kb:siblings` |
| `useSlates` | `() → {slates, attention, loading, error}` — the list + the nav chip share ONE `["slates"]` entry (the `useInbox` shape). `useSlatesAttention()` is the chip's single number. | `GET /api/slates` + `slate.updated`/`slate.deleted` via the bridge |
| `useSlateBoard` | `(slug, topic?) → {board, loading, error}` — key `["slate", slug]` (`["slate", slug, topic]` when filtered), `staleTime: Infinity`. | `GET /api/slates/{slug}?view=board` |
| `useSlateHistory` | `(slug, enabled) → {rows, loading, error}` — key `["slate", slug, "history"]`; `enabled` only while the drawer is open. | `GET /api/slates/{slug}/history` |
| `useSlatePost` | `(slug, {confirm, toastErr, toastInfo, onOpenHistory}) → (body, ctx?) => Promise<AppendResponse\|null>` — the ONE write, with the 409 friction (`slate-taken` → "post anyway" toast; `slate-live-author` → confirm + resend with `anyway`, except a supersede on a live take, which has no escape) and the `displaced`/`nudge` toasts. | `POST /api/slates/{slug}/posts` |

Hooks that read SSE (`useReview`, `useReadingProgress`, `useStaleAnchors`,
`useMetrics`, `useDaemonStatus`) use module-level stores / singletons so
multiple mounts share one subscription.

## API layer (`web/src/api/`)

**`base.ts`** — `currentDaemonBase(): string` (default `""` =
same-origin) and `setDaemonBase(url)`. Fetchers read it on every call,
so a runtime daemon switch takes effect immediately.

**`slates.ts` + `slateTypes.ts`** (SL4) — the kb-slate/1 client, one file
per feature (the `inbox.ts` convention). `slateTypes.ts` is a HAND-MIRROR
of the design's §9 wire shapes and of `crates/kb-core/src/slate.rs`, field
for field including the serde attributes (`skip_serializing_if =
"Option::is_none"` ⇒ optional on the wire, a bare `Option<T>` ⇒ `T | null`
always present) — **replace it with `api/generated/*` once SL2's ts-rs
export lands.** `slates.ts` carries its own `slateGet`/`slateMutate` (the
`client.ts` pair is module-private) and its own `SlateApiError`, which
exposes the slate's RFC 7807 extension members: `.code` (prefers the
`code` member, falls back to the `detail` prefix — the exact ladder the CLI
reads) and `.holder`. Unlike `inbox.ts` these fetchers do NOT swallow
errors into an empty payload: the 409 friction is the whole point.

**`client.ts`** — fetchers + the core types. All errors are parsed from
RFC-7807 `application/problem+json` into readable `Error`s;
`isAbortError(e)` detects cancellations (hooks swallow those).

- *Types:* `Identity`, `KbSummary`, `DocSummary`, `DocsQueryParams`,
  `TagSummary`, `FolderNode`, `SearchHit`, `AtlasEdge`,
  `Comment`/`Reply`, `ReviewFile`.
- *Read:* `fetchIdentity`, `fetchKbs`, `fetchDocs`, `fetchDocsPage`
  (paginated envelope), `fetchDoc`, `fetchDocByPath` (path permalink),
  `fetchTags`, `fetchFolders`, `fetchAtlasEdges`, `fetchStaleAnchors`,
  `search`, `fetchReview` (404 → `null`).
- *Write (comments):* fine-grained mutations — `addComment`/`addReply`
  (return the created entity), `resolveComment`/`unresolveComment`,
  `resolveAll`/`unresolveAll`, `editComment`/`editReply`,
  `deleteComment`/`deleteReply`. Each sends a small delta to its own
  endpoint; the daemon owns the load → mutate → save under `review_lock`
  (no client `If-Match`). `emptyReview(kb, id)` builds a skeleton.
- *Write (other):* `recomputeAtlas`, `reclusterAtlas` (both 202 + `run`,
  completion via SSE).

**`history.ts`** — `recordOpen` → `{visit_id, scroll_y}`,
`recordScroll` (404 → false), `recordSearch`, `fetchHistory`. Best-effort
/ observational: errors are logged and swallowed.

**`prefs.ts`** — `Theme`/`Accent`/`Density` + `loadPrefs`/`savePrefs`/
`applyPrefs` (sets CSS vars + `data-theme`) + `patchSettingsDebounced`
(`PATCH /api/settings`, 500ms). Source of truth is `localStorage`
(`kb:prefs`); the daemon copy is in-memory in v0.1.

**`sse.ts`** — the `sse` singleton facade: per-tab listener registries +
status aggregation. The CONNECTIONS live behind `../sse/transport.ts`
(invariant #24): a SharedWorker (`../workers/sse.worker.ts`) hosting the
context-agnostic core (`../sse/core.ts`) holds ONE unfiltered
`/api/events` stream per daemon for ALL same-origin tabs — or the same
core runs inline in this tab (direct fallback; force with `?sse=direct`
or `localStorage["kb:sse:transport"]="direct"`, e.g. while iterating on
`sse/*` in dev, since HMR never reaches a running SharedWorker). Frames
are fetch-parsed (`../sse/stream.ts`), not EventSource, so every event
type is delivered generically. Reconnect backoff 1s→30s lives in the
core; the worker advances cursors, tabs mirror `kb:lid:<url>`.
- `sse.subscribe(fn) → unsubscribe` — aggregated `{phase, inFlight,
  openErrors, openComments, daemons[]}`.
- `sse.subscribeEvent(type, fn) → unsubscribe` — a specific event type;
  `fn(payload, daemonUrl)`.
- `sse.subscribeAll(fn) → unsubscribe` — every frame from every daemon
  (full `EventEnvelope`, incl. synthetic `lag`/`gap`) — the Live tab.
- `sse.start()` / `sse.stop()` / `sse.loadDaemonUrls()` /
  `sse.saveDaemonUrls(urls)` (persists + reconciles connections).
- `window.__KB_SSE__` — debug/e2e: `transport()` + `status()`.

## Lib (`web/src/lib/`)

| File | Provides |
|---|---|
| `sort.ts` | `SortKey`, `SortDir`, `GroupKey`, `defaultDir`, `sortComparator`, `groupByFolder` — client-side ordering of the gallery's docs. |
| `derive.ts` | Front-end derivations: `fnv1a`, `tagColor`, `pathToTags`, `tagsFor`, `isIndexPage`, `relativeAge`, `isNew`, `basename`, `parentDir`. |
| `artifactHost.ts` | `deriveArtifactHostSuffix`, `artifactOrigin(id, suffix?)`, `isArtifactOrigin(origin, suffix?)` (postMessage trust boundary), `verifyAgainstIdentity`. |
| `artifactHref.ts` | `artifactHref(kb, rel, page?) → /a/<kb>/<rel>[?p=N]` — path permalink builder (per-segment encoded). |
| `time.ts` | `formatHistoryTime(unix, mode?)` — shared by `HistoryTimeline` (FloatingPill consumer retired in v0.12 X1 finish). |
| `slateGlyphs.ts` | PURE glyph/colour map for the slate board: `KIND_GLYPHS`, `STATE_GLYPHS`, `glyphForKind`, `glyphForLiveness`, `colorForKind`, `opacityForAge`, `authorChip`. Every kind and state has a glyph **and** a word (nothing is conveyed by emoji or colour alone) and every colour is a `tokens.css` custom-property NAME, never a literal. |
| `slateLanes.ts` | PURE layout engine for the board: `orderCards` (pinned → marks desc → newest, the digest's own order, implemented once), `columnsOf`, `lanesOf`, `nowRows`, `warnRows`, `readingOrder`, `moveCursor`, and `attentionCount`/`totalAttention`/`orderSlates`. The five columns are a VIEW over the projection's seven sections — section membership is never re-derived from `kind`. |

## Routes (`web/src/routes/`)

| Route | Component | Notes |
|---|---|---|
| `/` | `Gallery` (`gallery.tsx`) | Four views (grid/list/atlas/history); filters/sort/group; virtualized; `useDocs` + `useReadingProgress`. |
| `/a/:kb/*` | `Detail` (`detail.tsx`) | Splat = source-relative path → `fetchDocByPath`. Iframes the artifact at `<id>.artifacts.<suffix>`; `AnnotatorBridge` + `useReview` + scroll postMessage. |
| `/settings` | `Settings` (`settings.tsx`) | Theme/accent/density + `DaemonsManager`. |
| `/stale-anchors` | `AnchorsRoute` (`anchors.tsx`) | `useStaleAnchors`, grouped by (kb, artifact). |
| `/slates` | `SlatesRoute` (`slates.tsx`) | `useSlates`, ranked by `attentionCount`, closed last. |
| `/slates/:slug` | `SlateBoardRoute` (`slateBoard.tsx`) | NOW band + five `role="region"` columns + swimlanes; `?topic=`/`?history=1` are the only state (URL, never a second home). Owns its OWN window keydown listener for `j/k/Enter/m/x/e/p/c/t/h` — registered `scope: "slates"` (doc-only) in `lib/keymap.ts`, NOT dispatched by `HotkeyRoot`, because a global `j`/`k`/`m`/`p` would also fire `useRovingCursor` and the marks handler. `g b` (global) opens `/slates`. |

## Conventions

**Daemon base URL.** Read via `currentDaemonBase()`; never hardcode.
`setDaemonBase()` switches at runtime (multi-daemon).

**Artifact iframe + trust boundary.** The iframe origin is
`artifactOrigin(id, suffix)`; the suffix comes from
`useArtifactHostSuffix()` (daemon's `/api/identity`, falling back to a
`window.location` heuristic). **Every inbound postMessage must be
gated by `isArtifactOrigin(e.origin, suffix)`** before trusting it. The
parent↔iframe message set: outbound `cm:mode`/`cm:flash`/`cm:refresh`;
inbound `cm:add`/`cm:probe`/`open-artifact`/`pm:page`/`kb-probe`/`kb:scroll`.

**SSE.** Never open `EventSource` or stream `/api/events` from tab code —
use the `sse` singleton (invariant #24: the SharedWorker owns the
connections; a tab-opened stream re-introduces per-tab connection
multiplication against the browser's 6-per-host pool). `subscribe` for
aggregated status, `subscribeEvent(type, fn)` for a specific event,
`subscribeAll(fn)` for the firehose; all return an unsubscribe to call
on cleanup.

**URL state.** Gallery filters live in the URL via `useUrl()`
(`?kb`, `?view`, `?tags`, `?caps`, `?since`, `?folder`, `?index`,
`?sort`, `?dir`, `?group`); `set(key, value)` preserves the others with
`replace: true`. Detail uses `/a/:kb/*` + optional `?p=` for multi-page.

**Errors.** Daemon errors are RFC-7807 problem+json; `client.ts`
surfaces them as readable `Error`s. Comment mutations are fine-grained
deltas serialised server-side under `review_lock` (no client `If-Match`);
`useReview` applies them optimistically and refetches to resync if a
request fails.

**localStorage keys:** `kb:prefs`, `kb:pill`, `kb:siblings`,
`kb:daemons`, `kb:lid:<url>`. No user content is stored locally —
comments and history live on the daemon.

## Where things connect

- New gallery filter → add the param in `useUrl` usage + `LeftRail`,
  thread it into `DocsQueryParams`/`fetchDocsPage`.
- New artifact field → extend `DocSummary` (`client.ts`) and, if
  derivable client-side, `derive.ts`.
- New SSE-driven UI → `sse.subscribeEvent("<type>", …)` inside a hook
  with a module-level store; see `useMetrics`/`useStaleAnchors`.
- New slate affordance → the projection already carries the field. Render
  it from `lib/slateGlyphs.ts` (a glyph AND a word) or `lib/slateLanes.ts`
  (position); never add a second classifier that re-derives a section from
  `kind`, and never add a fact the CLI digest cannot print.
- New slate mutation → there isn't one. Every board action is an append of
  one of the twelve kinds through `appendSlatePost`; `drop`/`mark`/`done`/
  `answer` are ordinary posts carrying `re`, and an edit is a post carrying
  `supersedes`. There is no in-place rewrite route to reach for.

**The sketch frame (`kb-sketch/1`).** `components/slate/Sketch.tsx` mounts
`<iframe sandbox="allow-scripts" src="/sketch.html">` — no
`allow-same-origin`, so the frame has an opaque origin: no cookies, no
storage, no parent DOM. Handshake: frame → parent `{type:"kb-sketch/1",
ready:true}`, parent → frame `{type:"kb-sketch/1", source}`, frame → parent
`{type:"kb-sketch/1", height}` (capped at 480 px). `targetOrigin` is `"*"`
because an opaque origin has no literal string to name; the parent
authenticates instead by OBJECT IDENTITY — `event.source ===
iframe.contentWindow`, a window reference no other document can forge.
`src/sketch/main.ts` is the only module in the repo that imports mermaid
(`securityLevel: "strict"`, `htmlLabels: false`, `startOnLoad: false`) and
it is a separate Vite HTML entry, so the SPA bundle never carries it and
the "no `rehype-raw`, no raw-HTML sink" posture of `CommentBody.tsx` is
intact. `CommentBody` gains an opt-in `sketchSeq?: number` prop that turns
on the ` ```mermaid ` interception; absent (every pre-SL4 call site) the
output is byte-identical.
