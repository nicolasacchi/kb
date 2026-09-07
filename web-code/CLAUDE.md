# web-code — internals

Sibling of the workspace-root [`/CLAUDE.md`](../CLAUDE.md) and of
[`crates/kb-code-server/CLAUDE.md`](../crates/kb-code-server/CLAUDE.md) (D19 of
the kb-code v7 "The Continuum" design of record). This file holds invariants
that live entirely inside the SPA — above all its keyboard/command dispatch
contract, which is what actually broke, three separate times, during the v7.0
build. Root CLAUDE.md's crate-layout section and
[docs/kb-code.md](../docs/kb-code.md) are the feature-level surface;
`kb-code-server/CLAUDE.md` is the server-side half of every contract
documented here (the two registries this file's commands/themes sections
consume are SOURCED there).

## Orientation

`src/desk/` — the five-region shell (§ Desk below). `src/commands/` — cmd/1's
SPA half: `registry.gen.ts` (generated, do not hand-edit — see
kb-code-server's CLAUDE.md #6), `dispatch.ts` (the pure resolver + chord
machine), `CommandRoot.tsx` (the one window-level keydown host). `src/nav/` —
the Location Contract: `location.ts` (the total `Location` type + pure
`transition()`), `history.ts` (the two router adapters), `ramp.ts` (the
commitment gradient on every result row). `src/themes/` — kbc-theme/1's SPA
half: `derive.ts` (pure anchors→roles derivation), `registry.gen.ts` +
`themes.gen.css` (generated). `src/editor/vimKeys.ts` + `vimReader.ts` — the
CM6 buffer's own modal layer, installed at `Prec.highest` inside every
`CodeView`; it is a SEPARATE executor from `CommandRoot`; see § Keyboard
below for exactly how the two interact. `src/routes/Reader.tsx` hosts the Desk
and wires the vim layer's callbacks to the command bus.

## Keyboard and command dispatch (read this before touching ANY binding)

Four separate defects and two root-cause fixes all trace to the SAME
structural gap: one keystroke, two layers that both react to it, and no
agreement about which of them owns it. If you add a bare key (a key with no
`Ctrl`/`Alt`/`Meta` modifier) to `registry.json`, a handler for an existing
row, or a `stopPropagation()` in a panel that takes focus, read this section
in full — it is written to stop a fifth, not to narrate the first four.

**A registry row with no registered handler is silently dead.** A
`dispatch: "central"` row is fired by `CommandRoot`'s window `keydown`
listener; a `dispatch: "surface"` row is owned by whichever route/component
renders the thing (or, for a row carrying `vim_kind`, the CM6 vim layer).
Declaring a row in `commands/registry.json` makes it visible to the palette,
the `?` sheet, the which-key overlay and `kb-code commands doctor` — it does
**not** make it DO anything. `CommandRoot.onKey`'s `"matched"` branch is
explicit about the consequence: `if (!handler) return; // nothing owns it
here — leave the key alone`. Five `pane.*` commands shipped in
`registry.json` with no matching call to `useCommandHandlers` in
`Reader.tsx` (`V70-A5`/found by `V70-H1`): `Ctrl-w h/l/v/q`/the focus cycle
did nothing, and only one browser spec (`split.spec.ts`) happened to exercise
the failing path — `Ctrl-w v`/`q` "worked" by coincidence (they route through
a DIFFERENT code path than `h`/`l`/the cycle). **Whenever you add a row whose
`dispatch` is `"central"`, grep for a `useCommandHandlers({ "<id>": … })`
call that actually registers it before you trust the key does anything.**
`V71-K2` made the weak half of that mechanical:
`commands/deadRows.test.ts` fails when a shipped central row is registered
NOWHERE, against a pinned ledger (`KNOWN_UNREGISTERED`) that is a debt
list, not a config — shrinking it is the fix, and adding to it is a
deliberate act a reviewer sees. `V71-K3` shrank it to EMPTY: the
`Space`-leader desk/preset/drawer/rail family (seventeen rows, mouse-only
since V70-A4/A6) now each dispatch exactly the call its own button already
made — `Reader.tsx`'s `useCommandHandlers` block, the V71-K3-tagged entries
beside `desk.toggle.drawer`. One row's mouse door had no honest keyboard
analogue to copy: `drawer.reopen` (`Space u`) wants "the most recently
closed tab", but `drawerSets.ts` tracks only insertion order, never a
closed-at order, so it takes the documented smallest-honest-version
fallback (re-expand a collapsed drawer) instead of inventing an ordering
the data does not support. `drawer.tab`'s `Space {1-9}` also needed a
dispatcher fix: it is the first `central` row to wildcard a NON-leading
digit, which the chord machine's `count` accumulator (built for a LEADING
vim count) never captured — `dispatch.ts`'s `step()` now also recovers a
`{1-9}`/`{0-9}` digit from the winning key template itself, scoped so a
literal bare-digit row (the review cockpit's `1`…`5` tabs) is untouched.
What is still NOT tested is the strong half — that the handler is
registered in every reachable ROUTE (`app.tsx`'s shell registers many, but
`Reader.tsx` owns the desk verbs and unmounts with the route) — which
needs a route graph this suite does not have.

**A bare key can be swallowed by the CM6 buffer guard — the predicate is
precise, and getting it wrong breaks either one key or thirty-five.**
`CommandRoot`'s guard 2 (`shouldWithholdFromBuffer`, exported from
`commands/CommandRoot.tsx`) exists because the vim layer installs at
`Prec.highest` inside a `CodeView` and reacts to bare keys itself — so a
bare key inside the read-only reader buffer must be decided ONCE: does vim
own it, or does it fall through to `CommandRoot`'s dispatcher? **Three**
regressions shipped from getting this wrong in three different ways — two in
the predicate (fixed at the root by `V70-K1`) and one in what the predicate
is ASKED ABOUT (`V71-K2`):

- **`V70-H1`**: the guard read `document.activeElement` correctly for
  "is the buffer focused right now", but the SURROUNDING app state
  (`keyboardRegion`) it used to have been conflated with was stale after a
  click on `<body>` (a `focusin` target the app deliberately leaves
  ambiguous) — so `:` (the command palette's own door) stayed withheld
  forever after such a click. Fixed by making the guard read live DOM focus
  directly and never the app's own scope bookkeeping.
- **`V70-K1`** (the root fix): even reading live focus correctly, the guard
  used to withhold **every** bare key the instant the buffer held focus —
  correct for a key vim actually reacts to, wrong for every bare
  `scope: "global"`/`dispatch: "central"` command that vim has no opinion on
  at all. Because "focus follows the file" lands DOM focus in the buffer the
  moment ANY file opens, that made every such command structurally
  unreachable from then on — not just the two keys this milestone found by
  accident (`:`, `u`), but the entire 35-row `Space`-leader family
  (`nav.home`, `view.theme-cycle`, `desk.preset.*`, `drawer.*`, `rail.*`, …)
  plus six more un-prefixed rows (`U`, `.`, `f`, `F`, `s`, `S`).
- **`V71-K2`**: the predicate was right and was still asked the wrong
  question, because the call site handed it live `document.activeElement`.
  The vim layer is at `Prec.highest` INSIDE the buffer, so it handles the
  key and runs its callback BEFORE the event finishes bubbling to
  `CommandRoot`'s window listener — and those callbacks move focus (`K`'s
  `cb-hover` opens the peek panel, which focuses itself; `u`'s `cb-nav-back`
  navigates, unmounting the CodeView and dropping focus to `<body>`). So
  the guard read "the buffer is not focused" for the one keystroke where it
  most certainly was, let the key through, and the central handler fired
  **too**: `K` opened the hover card AND an inline peek on the tree's
  focused row (Esc then closed only the inline one), and `u` ran `nav.back`
  twice, the second run seeing `history.length === 2` and undoing the first
  with a plain Back. **Guard 2 reads the KEYDOWN's own `e.target`** — fixed
  at dispatch, immune to anything a listener does mid-flight, and the same
  value guard 1 (`isTypingTarget`) already reads. Never reintroduce
  `document.activeElement` here: any state a handler you are deferring to
  can mutate is not a state you may read after it has run.

The corrected predicate (`shouldWithholdFromBuffer(focusTarget, token,
chordPendingLength, ctx)` in `commands/CommandRoot.tsx`) asks the REGISTRY
instead of re-deriving the boundary by hand — it resolves/continues the
token in scope `"reader"` (which already folds in `global` rows via
`resolve()`'s own precedence) and checks `vim_kind`:

- an **exact match** settles it outright: withhold **iff that command
  carries `vim_kind`** (`vimParity.test.ts` proves, bidirectionally, that
  `vim_kind` names EXACTLY the tokens the CM6 reducer reacts to — this is
  what keeps `:`/`u`/`?` routed through their own vim-layer forwarding arm:
  they are `global`+`central` rows that ALSO carry `vim_kind`);
- a **prefix** withholds only when **EVERY** reachable continuation carries
  `vim_kind` — **"every", never "any."** `[`/`]` continue into BOTH
  vim-owned rows (`[c`/`[f`, reader scope) and non-vim rows (`[d`/`]d`,
  `drawer.tab-{prev,next}`, global+central); withholding `[` because ONE
  interpretation is vim's would make the OTHER interpretation unreachable
  too — the identical bug, one keystroke later. Letting a mixed prefix
  through is safe either way it eventually resolves: a vim-owned
  continuation with no central registration is a harmless no-op on the
  central side, and vim's own reducer no-ops on any continuation it does
  not recognise. **If you ever "simplify" this to "any continuation
  carries `vim_kind`," you have reintroduced the Space-leader bug** — write
  a test with a mixed prefix (like `[`/`]`) before touching this function.

**Guard 2 rules on a chord's FIRST token only — the SECOND token is a
DIFFERENT ownership question, and it has its own answer.**
`shouldWithholdFromBuffer` bails the instant `chordPendingLength !== 0`, by
design: once a chord is in flight the keystroke must reach `step()`. What
nobody had said out loud is that the vim keymap has no such bail — the CM6
reducer evaluates every keydown independently, with no memory that a
`Space`-prefixed sequence is under way. So a bare letter that is BOTH some
central chord's final token AND a standalone vim key fired both layers,
every time, and `V71-K3` measured it: `Space u` (`drawer.reopen`) and
`Space R u` (`rail.tab.understand`) also ran vim's `u` (`nav.back`), which
navigated the reader out from under its own spec; `Space R a` also ran
`annotate.line`; `Space P v`/`Space R v`/`Space R h`/`Space R n`/`Space p`
collided too and merely landed harmlessly. `Space g h`/`Space g c`/… were
the same bug one key earlier: vim entered its OWN `g` prefix on the middle
token and then fired `gh`/`gc` beside the central `nav.*`.

`V71-K4` closed it with a second exported predicate beside guard 2 —
`chordConsumesKey(chord, token, scope, ctx, preset, hasHandler)` — reached
through the command bus as `chordWillConsume(e)`. It does NOT re-derive the
boundary (that is how guard 2 went wrong twice). It runs the very same pure
`step()` `CommandRoot.onKey` is about to run, on the same live refs, and
answers one question: **will CommandRoot CONSUME this keystroke?**
`pending`/`cancelled` ⇒ yes (it `preventDefault`s and holds, or collapses,
the chord); `matched` ⇒ yes only if the row is `shipped` AND something is
registered for it; `none` ⇒ no. Any layer that runs BEFORE the window
listener asks it first and stands down on `true`: `vimReader.ts`'s keymap
(returning `false`, so the event finishes bubbling and is handled exactly
once) and `PeekPanel`'s own `onKeyDown`.

- **That `matched` clause is load-bearing — do not simplify it to "the row
  has no `vim_kind`."** `[c`/`]c`/`[f`/`]f` (`commit.prev/next`,
  `workingset.prev/next`) and every `Ctrl-w` chord resolve centrally but are
  `dispatch: "surface"` with no central registration, so CommandRoot takes
  its "nothing owns it here" branch and the vim arm is the ONLY executor.
  Consuming them would have traded K3's double-fire for four dead vim
  chords — `V70-K1`'s "identical bug one keystroke later" refusal, again.
- **When the vim layer stands down it CLEARS its own half-typed sequence**
  (`pendingCount`/`pendingPrefix`, never the mode). `[` sets vim's prefix
  before the second key decides whose chord it is; a stranded prefix would
  silently eat the next keystroke as `[<whatever>`. It does NOT feed the
  reducer "for bookkeeping": `v`/`V` carry the mode in the STATE as well as
  in a command, so `Space P v` would leave the buffer believing it is in
  visual mode with nothing having reshaped the selection.
- **A focused panel may stop only the keys it HANDLES.** `PeekPanel` used to
  call `stopPropagation()` unconditionally and focuses itself on mount, so
  from the moment a peek opened no global command and no chord could reach
  the window listener at all — including `drawer.keep` (`Space K`), the one
  command whose entire purpose is to act on the peek that is open. Its key
  set is now the pure, unit-pinned `peekPanelHandlesKey` (its list cursor,
  Escape, and `nav/ramp.ts`'s own rung table). If you add a keydown handler
  to anything that takes focus, filter it the same way.

A bare key that must fire from INSIDE the buffer needs its own vim-layer
forwarding arm — a registry row alone is not enough, structurally, because
`vimReader.ts` (`Prec.highest`) sees the keystroke first. The pattern (see
`nav.back`'s `cb-nav-back` case in `vimReader.ts` and its wiring in
`Reader.tsx`: `onNavBack: () => commandBus.run("nav.back")`) is: give the
`VimCommand` its own kind, forward to `useCommands().run(id)` by COMMAND ID —
never re-implement the decision the central handler already makes. Do not
add a second copy of that logic in the vim arm.

**Playwright serves the BUILT bundle, not live `src/`.**
`web-code/e2e/global-setup.ts` passes `web-code/dist` to the daemon as
`KB_CODE_SPA_DIST`; `npx vitest run` compiles straight from `src/` and will
stay green while the browser suite silently exercises a stale bundle — this
cost roughly an hour of meaningless runs during `V70-H1`. **Run `npm run
build` after any `src/` edit before trusting a Playwright result**; if a fix
"isn't showing up" in e2e, check `dist/index.html`'s mtime against your
newest source edit before you start doubting the fix itself.

For orientation, `CommandRoot`'s window `keydown` handler applies three
guards in order, and only the middle one is the subtle one described above:
(1) `isTypingTarget` — never steal a key out of a composer or the CM6
suggestion editor; (2) the read-only-buffer guard above; (3) a modifier bail
— a pending chord plus a browser chord (e.g. `Ctrl-t`) belongs to the
browser, not this app.

## The Location Contract (`V70-A6`, design §P7)

`nav/location.ts` defines ONE total, serialisable `Location` (repo, frame,
path/sym/ent, anchor, center mode, panes, rail tab, drawer tab) with a total
`decode`/`encode` pair and a pure `transition(from, to) → "push" | "replace"
| "none"`, golden-pinned by `location.test.ts`'s transition table. It is a
thin layer OVER `lib/codeUrl.ts` (root CLAUDE.md #35 — the URL builder,
golden-pinned in lock-step with the server) — `codeUrl.ts` stays THE answer
to "what is the URL for this file at this line"; `location.ts` only adds "is
moving between two locations a push or a replace," which needs a PAIR of
places plus state several different builders own. Every URL it emits comes
out of `codeUrl.ts`'s own functions; it never re-implements a path/param
grammar. `decode` is total BY CONSTRUCTION: an unmodelled route decodes to
`{mode: "other", raw}`, whose `encode` is `raw` verbatim — a round-trip is
the identity for every same-origin path, which is what lets `navigate()` be
the single door without first modelling the whole app. Pane focus
(`panes.focused`) is carried on the value but deliberately NEVER encoded
(root CLAUDE.md #30's recon R10 — pane location stays URL-derived via
`?pane2=`, but which pane has keyboard focus does not belong in the URL).

`nav/history.ts` provides the two router adapters (Navigation API where
present — Chrome/Edge 102+, Firefox 145+ — falling back to the History API)
behind one interface: **in-app Back IS the browser's Back.** There is no
second stack that could disagree with it; `nav.back` (`u`), the pane arrows,
mouse buttons 4/5 and the browser chrome all end up calling the same
`adapter.back()`. The adapter NEVER intercepts a push or a replace (only
traversals) — intercepting a push would make React Router's own
`BrowserRouter` blind to a same-document navigation it thinks is
cross-document. Do not put durable per-hop metadata in `navigate(...,
{info})`: `info` does not survive a back/forward round trip; the `via` edge
that needs to survive one lives in the URL (`?via=`) and in `lib/trail.ts`.

## Themes (`kbc-theme/1`, `V70-A7`)

`themes/derive.ts` is a pure, one-way, side-effect-free derivation:
authored ANCHORS (26 OKLCH values per theme, sourced from
`kb-code-server/themes/registry.json` — see that crate's CLAUDE.md #7) →
~80 ROLES (CSS custom properties) → SURFACES (chrome vars, the 15
`.kbc-hl-*` syntax classes, diff/age/provenance/trust lanes). The direction
never reverses and never becomes circular — a surface never feeds back into
a role, and a role never feeds back into an anchor. The one exception is the
AA REPAIR pass: when a role's contrast against a background the Lane Budget
says it can actually sit on falls under its floor, ONLY the OKLCH lightness
is nudged (hue and chroma preserved), and every repair is emitted as a
comment in the generated CSS — a repaired vendor value must never be
presented as an authored one. `derive.ts` is deliberately a single
self-contained file with no relative imports (the dev-time node scripts
`scripts/gen-themes.mjs`/`theme-lint.mjs` import it directly by its `.ts`
specifier, relying on node's built-in type stripping, which has no
`.js`→`.ts` resolution rewrite) — do not split it without checking both
scripts still resolve it. `npm run lint:themes` (WCAG AA + APCA + Oklab
state-separation, CVD-simulated) is a CI gate in `ci-code-spa`; it must stay
that way — a palette regression should fail on its own named step with the
offending pairs listed, not surface as an unexplained vitest assertion three
files away.

## The Desk (`V70-A4`, design §P1)

`desk/deskState.ts` is the reducer that owns the shell's geometry — five
regions (left dock, main with one or two panes, right rail, bottom drawer,
two icon stripes). **`react-resizable-panels` is the resize MECHANISM
only, never the source of truth**: sizes flow OUT of this reducer into the
library (`defaultLayout` at mount, imperative `setLayout` afterwards) and
back IN only through `onLayoutChanged`'s `isUserInteraction` branch — never
`autoSaveId` (an opaque per-group localStorage blob that a preset, the
keyboard resize submode, `?desk=` or a future CLI could none of them
address). `deskState.ts` reads/writes NO ambient (`Date`, `window`,
`localStorage`) directly — `loadDeskState`/`saveDeskState` take an explicit
`StorageLike` so the unit suite (which runs `environment: "node"`) can
exercise every branch, including corrupt-blob recovery, without a DOM. A
preset (`desk/presets.ts`, `Read`/`Review`/`Explore`/`Present`) is a WHOLE
`DeskState`, never a patch — reading one preset's table tells you its
ENTIRE geometry with nothing inherited from whatever the desk happened to
be before, which is what makes a double-click reset exactly reproducible. A
human drag marks the desk dirty; a preset never silently overrides a dirty
desk.

Two e2e goldens gate any shell change (`e2e/desk-landmarks.spec.ts`,
`e2e/desk-viewport.spec.ts`): the **landmark golden** (region identity,
order and `data-region` addressing are identical across every center mode —
a mode may collapse a region, never move or rename one) and the **viewport
golden** (at 1280×720 with the default desk, the code region is ≥80 columns
× 28 lines, computed from CodeMirror's REAL character advance and line
height — never an assumed ratio; the drawer opens as an overlay rather than
violating that floor). Mobile (≤860px) is ONE column: the rail is the
single bottom sheet with its own tab strip, and the drawer lives INSIDE
that sheet — never a second sheet (the same "one mobile entry point" rule
root CLAUDE.md #30 states for the pre-Desk inspector rail).

## Workspaces v0 (`V70-A10`, D26)

A workspace is a server-side `reading_sets` row with `kind: "workspace"` —
see `kb-code-server/CLAUDE.md`'s invariant 9 for the storage side; this SPA
never treats it as a new client-side entity either. `lib/workspaceDirty.ts`'s
`isWorkingSetDirty` is a narrow, deliberate comparison: it is dirty **iff the
ordered list of open file PATHS differs** (length, membership or order)
from the workspace's saved entries. A line-number drift, a re-anchored
ref, or a new workspace note on an otherwise-unchanged file set is
deliberately **not** dirty — none of those is something "Save workspace"
itself would silently re-capture (the composer re-reads live cursor
positions at save time regardless of the dirty flag). Do not widen this
comparison to cover content drift without re-reading D26's own stated
grammar first — "dirty" here answers one question only: has the FILE SET
changed.

## Search grammar and match rendering (kbcq/1, `V71-D1`)

`lib/kbcq.ts` is a MIRROR of the daemon's `search/grammar.rs`, not a
derivation of it — neither generates the other, and the only thing keeping
them from drifting is ONE shared fixture,
`crates/kb-code-server/grammar/kbcq.golden.json`, which BOTH sides' golden
tests walk (`lib/kbcq.golden.test.ts` here, `golden_corpus_matches_the_rust_parser`
there). Touch the grammar on one side and the other side's test fails,
naming the case. Reading across the crate boundary is fine in a TEST (vitest
runs from the repo, exactly as `commands/registry.gen.test.ts` does); the
BUNDLE may never do it, and `kbcq.ts` imports nothing outside `src/`.

ONE divergence is documented rather than hidden: for a bare `/…` text query,
"is this a regex?" is decided by trying to compile it, and the two sides use
different engines (Rust's `regex` crate vs ECMAScript `RegExp`). Where they
disagree the DAEMON's answer is the real one — this side's `text_mode` is a
display hint, never something the SPA sends.

**Ranking is not a client concern.** kbcq/1's rule is one matcher, nucleo,
server-side; the daemon returns its match positions as UTF-16 ranges on
`FileHit`/`SymbolHit`, and `lib/matchRanges.ts` (`fromWire` → `sliceRanges`
→ `highlightSegments`) turns them into `<mark>`-able segments.
`lib/speedSearch.ts` keeps its ~19 list-filter call sites but is DEPRECATED
as a matcher and re-exports the rendering helpers from `matchRanges.ts`; do
not add ranking logic to it (V71-F1's tree filter is what finishes its
retirement). A symbol's ranges index the `Container::name` HAYSTACK, not
`path` and not `name` — re-base them with `sliceRanges` rather than
re-deriving a match client-side.

## The results page (`V71-D2`)

`routes/Search.tsx` is a LENS over the same `GET /api/search` the Omnibox
calls and the same `SearchSection` renderer — not a second search app. Four
rules, each with a home:

**Every control WRITES the query.** `lib/kbcqEdit.ts` is the one builder
(`appendClause`/`removeClause`/`toggleClause`/`setLane`/`setGroup`/
`setFacets`); the facet rail, the scope chip and the grouping control all go
through it, and NOTHING on the page holds filter state of its own. A filter
you cannot see in the box is not in force. Every function VERIFIES its edit
by re-parsing with `kbcq.parse` rather than trusting a substring — a
`lang:ruby` inside a quoted phrase is a TERM, and only the parser knows
that. Do not add a second writer; `kbcqEdit.test.ts` walks `FILTER_SPECS`
and fails on a key this module cannot round-trip.

**Refinement is not a matcher.** `lib/refine.ts` narrows the page the
daemon already returned — orderless, literal, smartcase, `!needle`
excludes — and only ever REMOVES rows: it assigns no score, preserves the
server's order, never re-queries, and the page reads "N of M shown". If a
`score` field ever appears in that file, kbcq/1's one-matcher rule has been
broken client-side. Grouping is the SERVER's (`section.groups`);
`lib/searchPage.ts` slices `results` by the group's own `indices` and never
re-derives a key — which is what makes the page and `kb-code search --json
--group dir` agree by construction. Grouping runs BEFORE refinement,
because the indices address the unfiltered array; a refined section drops
`groups` rather than remapping them.

**The page's keys are registry rows, dispatched by the page.** They are
`scope: "search"` (a scope coactive with `global` and `palette` ONLY — the
results page is not coactive with the reader or the tree, which is what
lets it bind `Down`/`Up`/`Tab`/`Enter` with no conflict) and
`dispatch: "surface"`, because the query box holds focus and `CommandRoot`'s
guard 1 (`isTypingTarget`) stops the window host before it ever sees them —
the palette's own pattern. Consequently **no bare printable key may be
bound here**: it would be typed into the query box instead of firing, which
is the same "structurally unreachable" shape V70-K1 fixed for the buffer.
`routes/searchCommands.ts` declares the owned ids as a union, `SearchHandlers`
is a TOTAL `Record` over it (so a declared id with no handler does not
compile), and `searchCommands.test.ts` walks the generated registry BOTH
ways — closing, for this one surface, the gap the keyboard section above
names ("there is no test today that a shipped central row has a registered
handler in every reachable route").

**History and saved searches never leave the browser.** `lib/searchHistory.ts`
is pure over an injected `StorageLike` (`desk/deskState.ts`'s shape) and the
recorded cut is in its header: there is no `saved_searches` table and no
`kb-code search saved` verb, so CLI parity for a saved search is the copied
`kb-code search '<kbcq>'` line. The preview is peek-first (it never takes
focus, never navigates) and DEBOUNCED, because `GET /api/file` bumps the
store generation on every read (recon R1) and previewing per keystroke would
rebuild the daemon's files/symbols snapshots between arrow presses.

## Usages and actions (V71-E2)

`lib/usages2.ts` is the dock's whole numeric half, and it exists so
`components/usages/UsagesDock.tsx` renders without counting. **The server's
totals are never re-derived**: `Usages2Out.totals`/`kind_totals` are the
TRUE counts BEFORE the per-class cap and `capped[]` names any class that
hid rows, so `censusOf` reports them verbatim. The ONE number this module
computes itself is how many of the RETURNED rows the chips currently hide,
and it is emitted as a `CensusReason` line — which is what makes "a count
never changes without an on-screen reason" a testable property rather than
a habit. Chips filter the PAGE, not the query (`/api/usages/2` takes no
filter params — V71-E1 cut them because a param no caller sends is dead
surface), so a filtered view of a CAPPED page shows both facts at once.
The grep lane (`/api/xrefs`) survives as an explicit "mentions" chip with
its own field and its own count, NEVER folded into `totals` — the recon
found kb-code already shipping three disagreeing "usages" numbers, and one
number for two questions is how that happened. `session`/`author`/`age`
grouping is deliberately absent: it needs a blame join this wire does not
carry, and an axis whose every row reads "unknown" is the dead surface this
milestone is about.

`]u`/`[u` (`usages.next`/`usages.prev`) carry **no `vim_kind`, on purpose**,
following the `] d`/`[ d` precedent exactly. Re-read the keyboard section
above before "fixing" that: `[`/`]` is a MIXED prefix (`[c`/`[f` are vim's,
`[d`/`]d` are not), so `shouldWithholdFromBuffer` lets the prefix through,
the SECOND key is never withheld (`chordPendingLength !== 0` short-circuits
the guard), and the vim reducer no-ops on a continuation it does not
recognise. Adding a vim arm here would fire the step TWICE. Bare `.`
(`action.panel`) needs no arm either, for the other reason the guard gives:
it resolves EXACTLY and carries no `vim_kind`, so the buffer never claims
it (the same path `U`/`f`/`s` take after V70-K1).

`lib/actionOps.ts` is the menu's pure half. Two rules: `resolveOp`'s
`default` branch assigns to `never`, so a new `ActionOp` variant on the wire
fails the BUILD instead of silently doing nothing; and every navigation goes
through `lib/codeUrl.ts`/`nav/location.ts` — the server hands over an
ADDRESS and this side never assembles a second URL grammar (root CLAUDE.md
#35, enforced by `nav/rawUrls.test.ts`). The `contextmenu` override is ONE
delegated document listener scoped to `.kbc-codeview`, never a
per-component handler (two menus fighting over one nested event is the
documented failure), Shift+right-click passes through, and the mobile path
uses its own 500 ms pointer timer rather than `contextmenu` (unreliable on
iOS since 13.1) opening the SINGLE bottom sheet.

## The file tree (kbc-tree/1, `V71-F1`)

`FileTree.tsx` renders rows it did NOT compute. The projection, the
decoration aggregates, the trust class on an inferred grouping and the
match ranges all arrive from `GET /api/tree/2`; this component's job is to
say which groups are OPEN (`expand=`) and to render honestly. **Do not
re-derive a count, an aggregate or a rank here** — that is how a second
implementation starts, and the server-side module doc names it as the
tree's own top risk.

Three consequences worth stating separately:

- **`lib/treeQuery.ts` contains no grammar.** kbcq/1 pays for a
  golden-pinned TS mirror (`lib/kbcq.ts`, § above) because the search box
  renders its own diagnostics before a round trip; the tree box does not,
  so `routeFilterBox` is a SHAPE rule — structured-looking text
  (`role:spec`, `$generated`, `a && !b`) goes to `?scope=`, anything else
  to `?filter=`. When it guesses wrong the daemon answers
  `scope_applied: false` with a note and the UNSCOPED tree, so a wrong
  guess costs one caption and never a wrong answer. Adding a third parser
  here would be the lock-step cost with none of the benefit.
- **The honesty strip is not decoration.** `notes`, `diagnostics`,
  `truncated`, `scope_applied: false` and `unplaced_total` are rendered
  VERBATIM, each in its own `[data-kbc-tree-note]`. Summarising them, or
  showing the tree without them, re-creates exactly the "a projection
  that hides work" failure the wire exists to prevent.
- **kbc-tree/1 is INDEX-only.** While a non-default ref is being browsed
  the dock keeps the pre-existing per-directory `GET /api/tree` listing,
  adapted into the same row shape by `lib/tree.ts`'s
  `legacyRowsToTreeRows` (no facts, no trust, no ranges — absent, never
  zeroed) and captioned. Only the DATA SOURCE branches: the render, the
  keyboard model and the `FileTreeHandle` are one. Tree-as-of-a-ref
  proper is deferred.

The eleven new registry rows are `scope: "tree"` except `tree.reveal`,
and the keyboard audit behind that is worth not repeating: **`gr` was NOT
claimed** even though the evidence report asked for it. `g r` is
`reader.find-references`, and every other `g`-prefixed row in reader scope
carries a `vim_kind` — so `g` is currently a PURE vim prefix that
`shouldWithholdFromBuffer` withholds outright. Adding any GLOBAL
`g`-prefixed row would make `g` a MIXED prefix inside the CodeView and
change what the buffer guard does with every `g` chord (§ Keyboard's
"every, never any" rule, from the other direction). `Space g f` is the
leader family that V70-K1 already made reachable from inside the buffer.
`]c`/`[c` in tree scope (depth 10) are a designed SHADOW of the reader's
`commit.next`/`commit.prev` (depth 20), not a conflict — `commands
doctor`'s conflict pass only gates two coactive scopes at the SAME depth.

## The dossier center mode (`entity/1`, `V72-G1.2`)

`?ent=<fqn>` puts the reader shell into its `dossier` CENTER MODE. Four
rules, each with a home.

**It is a MODE, not a route.** `routes/Reader.tsx` derives
`centerMode={dossierMode ? "dossier" : "reader"}` from the URL and renders
`components/entity/DossierView.tsx` in place of the panes; nothing else
about the shell changes. That is D1's rule ("any new surface lands in an
existing REGION") and it is CI-checked rather than promised:
`desk-landmarks.spec.ts` now loops both entries of
`desk/centerModes.ts`'s `SHIPPED_CENTER_MODES` and asserts a byte-identical
region set for each. Each mode in that loop declares its OWN "the center
has painted" selector — the spec used to wait on `.kbc-codeview` for every
mode, which would have failed the dossier for the sin of not being a code
buffer. A future third mode adds one line to `SHIPPED_CENTER_MODES`, one to
that `MODES` table, and no new region.

**One read, two renderers.** `hooks/useDossier.ts` owns the single
`GET /api/entity/dossier` query; the center and the rail's Dossier tab
(`components/entity/DossierRail.tsx`) both render the `DossierOut` it
returns. Neither fetches. This is kbc-tree/1's "one projection, two
renderers" rule (see `kb-code-server/CLAUDE.md` invariant 17a, whose own
stated top risk is exactly this pair drifting) applied to a second wire —
and it is why the member ordering lives in `lib/dossier.ts` rather than in
either component.

**Every count comes off the wire; `rows.length` is never the headline.**
`entity/1` sends each list's TRUE `total` beside an explicit `truncated`
precisely because the rows in hand are a CUT of it. So `lib/dossier.ts`'s
`usageGroupCensus` reports `group.total` verbatim and states "N of M shown"
as a separate fact, "show more" RE-FETCHES with a higher `usages_per_kind`
(the rows a smaller cut did not return were never in the browser to
reveal), and the `inherited` toggle is a re-fetch for the same reason — it
changes what the SERVER sends, not what this side hides. `honesty.state`'s
`partial`/`empty` render as a header caption naming the budget and every
lane it dropped, in the server's own `budget.order`; `honesty.notes` are
rendered one per note, verbatim, exactly as the tree's honesty strip is.
Trust renders through the shared `TrustBadge` (LINE STYLE, kbc-theme/1's
Lane Budget) and never through a hue this feature picks.

**`lib/dossier.ts`'s string comparator is `cmpStr`, not `localeCompare` —
do not "modernise" it.** `dossier.rs` sorts members with `a.name.cmp(&b.name)`
(code-point order, so `TAX_RATE` precedes `total`); `localeCompare`
collates case-insensitively and reverses that pair. Since the default
`visibility` sort must be a NO-OP over the order the response already
arrived in, a different comparator silently REORDERS the wire —
`dossier.test.ts`'s golden walk caught exactly that and is what keeps it
caught.

**The tree scoping is the EXISTING scope mechanism.** Dossier mode passes
`FileTree` a `derivedScope` of `ns:<fqn>` — kbc-scope/1's own `ns` atom
over the V71-G0 entity index — so there is one tree, one wire and one
honesty strip. A typed scope `&&`-joins with it (the grammar's own
composition) and a typed fuzzy filter rides alongside as `?filter=`;
neither silently replaces the shell's scope, and the "scoped to <fqn>"
chip is the positive counterpart to the strip's `scope_applied: false`
refusal.

**Keys.** `Space e d` (open, ungated), and `i` / `M` / `] s` / `[ s` gated
`when: center == dossier` via the new `center` context key. Bare `i` and
`M` are safe from inside a focused CodeView for a reason worth keeping
written down: the vim reducer reacts to them ONLY under the `g` prefix
(`gi` = impact, `gM` = mnemonics), so neither carries a `vim_kind`, guard 2
resolves them exactly and lets them through, and nothing fires twice.
`] s`/`[ s` carry no `vim_kind` either, following the `] u`/`] d`
precedent exactly (§ Usages and actions above) — `[`/`]` is a MIXED
prefix, so a vim arm here would fire the step TWICE. The dossier rail's
`j`/`k`/`Enter` are deliberately NOT registry rows: `rail` scope declares
exactly one row today, so there is no convention to join, and the list
obeys the focused-panel rule instead — it stops only the keys it handles
(`railListHandlesKey`), leaving every chord and global command to reach
`CommandRoot`.

## Comments/1 in the SPA (`V72-J2`, D8, Track J)

comments/1 (server: `crates/kb-code-server/src/comments/`) is a repo-wide
scanner over eight comment kinds (doc/annotation/directive/section/licence/
generated/commented_code/prose), each carrying a per-request, never-
persisted `state` (fresh/drifted/unknown/aged/unreasoned/none). This unit
gives it four SPA surfaces, each with a home already named by an existing
convention — nothing here invents a new one.

**The gutter is slot FOUR, not a rail row.** kbc-theme/1's Lane Budget
(`themes/LANE-BUDGET.md`) says the intent/comment column is "gutter slot
four or a rail row, never a fifth slot." `CodeView.tsx` had exactly THREE
`editor/lineGutter.ts` gutters before this unit (blame, the existing
annotations/review gutter, diagnostics — PRR-U9's own comment already
numbers itself "a THIRD `createLineGutter` call"), so comments/1 is the
fourth: `commentGutterHandle`, its own `StateField`/`StateEffect` pair
(the factory's own "each call needs its own identity" contract), the
`.kbc-comment-dot--<kind>` fill + `.kbc-comment-dot--state-<state>` BORDER
STYLE (never hue-only — the Lane Budget's "trust is a line style" rule,
applied here to comments/1's own state vocabulary). `lib/comments.ts`'s
`commentGutterMarkers` marks every line in a block's `[line_start,
line_end]` range, mirroring `lib/diagnostics.ts`'s own region-spanning
convention.

**Three modes, one client-side filter, zero server calls.** `all` (every
kind) → `quiet` (only `annotation`/`directive` rows that carry a non-`none`
STATE — a `doc` row is hidden in quiet even when fresh, since quiet's whole
point is "just the actionable directives/annotations") → `doc-only` (doc
rows only, any state) → back to `all`. `Space C c` cycles it
(`comments.gutter-mode-cycle`); persisted in `kbc:prefs` under
`commentGutterMode` (`lib/prefs.ts`'s `load`/`saveCommentGutterMode`), same
browser-local posture every other reading toggle here takes. A mode NEVER
hides a comment class silently — the toolbar's mode chip
(`[data-kbc-comment-gutter-mode]`) always names which of the three is
active.

**The rail's Comments tab is UNCONDITIONAL.** Unlike Review/Dossier (gated
on "does this file have one"), every file the scanner covers has an
honest, possibly-empty, comments/1 list — so `comments` joins `RailTab`
(`desk/deskState.ts`'s `RAIL_TABS`, now seven) as an always-offered tab,
`Space R c`. `components/comments/CommentsPanel.tsx` renders the WHOLE
file's comments/1 rows regardless of the gutter's own display mode (the
gutter mode is a reading filter over the margin, never a second index).

**Doc hover is additive-only, keyed off the SAME already-fetched data.**
`editor/hoverTooltip.ts`'s existing `renderHoverDom` (the CM6 identifier
tooltip) gained one optional parameter, `docComment` — the comments/1
`doc` row for the hovered symbol (`lib/comments.ts`'s
`findDocCommentForSymbol`, matched by `symbol.name` within the CURRENT
file's own `useCommentsFile` result; no second fetch). Absent/`null`
renders byte-identical to before this unit. When present it appends, right
after the existing `sym.doc` paragraph: the freshness caption
(`freshnessCaption` — "fresh" / "drifted N days (code moved at `<sha>`,
doc last touched at `<sha>`)" / "unknown: `<reason>`" / …, NEVER a verdict
about whether the comment is wrong, per `comments::drift`'s own rule), and
— only when the doc is YARD-shaped (`@param`/`@return`) AND the hovered
symbol's `signature` string has parameters to compare against — a "YARD ≠
signature" chip (`yardSignatureDisagreement`, pure/client-side/golden-
pinned) naming which names are undocumented or over-documented, never
which side is right. This surface is a deliberately narrow append: V72-I2
(landing around the same time) also touches this tooltip/peek pipeline —
expect an append-only rebase here, keep both additions.

**The claim → annotation bridge adds ONE value, not a table.**
`annotations::INTENT_CLAIM = "claim"` (`crates/kb-code-server/src/
annotations.rs`) is the one genuine server change this unit makes: `intent`
validation there is route-boundary string matching with no SQL `CHECK`
behind it (that fn's own doc), so widening the seven-value vocabulary by
one is the whole change — no migration, no new table, no new column. The
bridge (`lib/comments.ts`'s `deriveBridgeRows`) NEVER mints its own id or
back-reference: a bridgeable comment (an `annotation`-kind row whose
keyword is in `GET /api/comments/keywords`'s `todo_family`, never
hardcoded) and an `intent: "claim"` annotation are joined by CURRENT LINE
— both sides are independently re-resolved against the same live blob per
request (comments/1's scan; `annotations::resolve`'s own content-hash
carry-forward), so "same live line" is the honest, already-existing
matching mechanism; there is no fifth identity to invent. Four states,
derived per render, never stored: `open` (comment, no claim) / `tracked`
(open claim, comment present) / `resolved` (claim resolved, regardless of
whether the comment remains — "resolved but the TODO text remains" is
still `resolved`) / `gone` (open claim, comment no longer at that line —
an orphan, surfaced via the SAME carry-forward ladder the annotation's own
`stale`/`line` fields already ride, never hidden). `Space C t`
(`comments.track-as-annotation`) creates the annotation through the
EXISTING `POST /api/annotations` path (`lib/annotations.ts`'s
`buildCreatePayload` shape, called directly with `intent: "claim"`); it
never edits source — a "fix via suggestion" door stays the existing
suggestion composer's job, not this bridge's.

**`~comments` is a NEW dashboard; `~todos` is untouched, not redirected.**
`routes/Comments.tsx` (`Space g m`, `nav.comments`) defaults to the
ACTIONABLE slice — three parallel `GET /api/comments?state=<lane>` calls,
one per `lib/comments.ts`'s `ACTIONABLE_STATES` (a small mirror of the
server's `comments::drift::ACTIONABLE_STATES`, since no wire response
carries that list), the exact shape `kb-code comments audit` prints —
each its own server-paged section with a TRUE total, never a client
slice. Facet chips (kind, keyword, path prefix, off `GET
/api/comments/summary`'s exact counts) narrow every lane's query at once;
"show everything" swaps to one unfiltered, still server-paged list.
`routes/Todos.tsx` (`~todos`, `Space g t`) is DELIBERATELY unchanged byte-
for-byte (its own wire, its own specs, its own `PageId` all keep working)
— it gained one discoverability LINK to `~comments`, not a redirect,
because folding it in cleanly would mean re-deriving its exact TODO-family
row set as a `~comments` preset and this unit chose not to grow the query
grammar for that; a future unit may still retire the standalone page once
that preset exists.

**Registry rows (`crates/kb-code-server/commands/registry.json`, group
`Comments` + one `Go`/`Rail` row each):** `nav.comments` (`Space g m`,
`m` because `c` is `nav.canvas`), `rail.tab.comments` (`Space R c`),
`comments.gutter-mode-cycle` (`Space C c` — a NEW `Space C` leader prefix;
`Space c`/`Space m` are already `diff.context-cycle`/`diff.map-toggle`,
which the shadowing lint (`kb-code commands doctor` check 6) forbids a new
`scope: global` row from reusing), `comments.next`/`comments.prev` (`]
m`/`[ m` — `]c`/`[c` are `commit.next`/`commit.prev`; deliberately NO
`vim_kind`, following the `]u`/`[u`/`]d`/`[d`/`]s`/`[s`/`]p`/`[p`
precedent: `[`/`]` is a MIXED prefix, so `CommandRoot`'s guard 2 lets it
through and the vim reducer no-ops on a continuation ('m') it does not
recognise), `comments.track-as-annotation` (`Space C t`),
`comments.open-card` (`Space C o`). None of the six carry a `vim_kind` —
every one is a `Space`-leader chord or a `[`/`]`-prefixed mixed chord, and
D2's "Space is only the leader" plus the mixed-prefix precedent both mean
none of them needs a vim-layer forwarding arm.

## Review diff v2 (`V73-K2a`, design §D9, Track K)

`routes/ReviewDiff.tsx` is the full-page review reader. Diff v2 added a
file-map COLUMN, per-HUNK state, a patchset SWITCHER, a context DIAL, noise
LABELS and a DRAFTS tray. Five rules, each with a home.

**The URL is the only state.** `?ps=` · `?ctx=` · `?noise=` · `?map=` ·
`?file=` · `?hunk=` join `lib/codeUrl.ts`'s `ReviewDiffHrefOpts`, appended
LAST so every pre-V73 golden row is byte-identical, each omitted at its
default, each with a TOTAL parser (`parseDiffPs`/`parseDiffCtx`/
`parseNoiseMode`/`parseDiffMap`) that degrades junk to the documented
default rather than throwing. There is no parallel store: a reload
reproduces the view, and `routes/ReviewDiff.tsx` writes every one of them
through ONE `setParam` helper with `replace: true` (a view knob is not a
navigation step — the pre-existing `view`/`overlay`/`tour` writers already
had that posture). `?ps=a..b` is the interdiff range; `parseDiffPs`
REFUSES an inverted range rather than swapping it, because guessing which
end the operator meant is the quiet repair this codebase does not do
elsewhere.

**A hunk has a content-addressed NAME.** `lib/diffHunks.ts`'s `hunkId`
(`kbc-hunkid/1`) hashes the path plus the hunk's own `+`/`-` lines and
deliberately NOT its line numbers or its `-U3` context, so the id survives
a rebase — which is what lets `review_hunk_viewed` (migration V0030) store
a viewed mark that follows the CHANGE rather than a position. The daemon
stores these ids opaquely; the addressing scheme lives here. The stated
price is that two identical changes in one file collide onto one id, which
is why the hunk id is never used as a comment/finding anchor — those keep
the server's carry-forward ladder. Bumping the recipe means bumping the
schema string: an old id then simply stops matching, so a viewed hunk
reads UNVIEWED, never the reverse.

**Noise is a LABEL, never a filter.** `lib/diffNoise.ts` owns the five
classes (`generated` · `whitespace-only` · `moved` · `rename-only` ·
`large`), each with its RULE as a sentence that the chip's `title` shows
verbatim plus a `detail` naming this subject's own evidence.
`?noise=collapsed` COLLAPSES a labelled hunk — visibly, still counted in
the census, one click from open — and `noiseCollapses` is the single
function that decides it, so "a label is not a filter" is one assertion
rather than a habit spread over three components. Two honesty carve-outs
are load-bearing: hand-authored `migrations/` are NOT `generated` (they are
the change, not a rendering of it — only schema DUMPS are), and the `moved`
rule searches only the file diffs the page has actually loaded, so a
`moved` label that appears always names its counterpart file while its
ABSENCE claims nothing. `buildMovedIndex`'s own `scanned`/`capped` census
is what keeps that partiality on screen.

**Widened context is fetched, never synthesised.** `GET /api/diff` is
hardcoded to `git diff -U3` and this unit did not add a context param to
it; `?ctx=10|full` and the per-hunk "expand 10 more" splice REAL rows out
of `GET /api/file?ref=<patchset tip>` (`lib/diffContext.ts`). When that
fetch has not landed — or the file is binary, or the ref is gone — the hunk
renders at its wire width under a caption naming why. The line arithmetic
lives in that module and is unit-pinned because it has a trap: a context
row ABOVE a hunk and one BELOW it use DIFFERENT old-side deltas whenever
the hunk is not size-neutral, and using one for both silently mislabels
every gutter number under it.

**Drafts are browser state; publish is one transaction.**
`lib/reviewDrafts.ts` keeps composed comments/questions in
`sessionStorage` per `(repo, review)` — a recorded CLI-parity exemption of
the same kind `lib/searchHistory.ts` carries, and `sessionStorage` rather
than `localStorage` so a draft cannot resurface against a patchset that no
longer exists. `Publish` sends ONE `POST /api/annotations/batch` (one
store transaction, at most one SSE); the tray is cleared only after the
server accepts, so a refusal leaves the review whole in the browser.
**FINDINGS are deliberately NOT drafted**: the batch route is ordinary
bearer while `POST /reviews/{id}/findings` rides
`review_gate::review_mutations_gate`, so an `add_finding` batch op would
graduate finding creation off its own gate as a side effect of a UI
feature. The composer's finding tab keeps its own gated route. Folding
findings into the atomic publish is real work (a gated batch op) and is
named as open, not smuggled in.

**Keys.** Twelve `scope: "diff"`, `dispatch: "surface"` rows, all
registered in `ReviewDiff.tsx` and re-checked by
`commands/diffV2.test.ts` (the family `deadRows.test.ts` structurally
cannot reach, since that suite gates `central` rows only). `Space h`
(hunk viewed) is under the leader for the SAME reason V70-A5 moved
`diff.toggle-viewed` there: bare `v` is the reader's visual mode and
`diff`/`reader` are coactive at depth 20. `z c`/`z o`/`z a` are safe
because `z` is a PURE-vim prefix in READER scope — diff-scope rows are
invisible to `shouldWithholdFromBuffer`, which resolves in `"reader"`, so
`z` is still withheld inside a CodeView exactly as before. `] p`/`[ p`
carry no `vim_kind`, following the `] u`/`] d`/`] s` precedent: `[`/`]` is
a MIXED prefix and a vim arm would fire the step twice.

## The review document (`kbc-review/1`, `V73-K2b`, design §D9/D9-a)

`?tab=doc` is the Review Room's sixth cockpit tab. Four rules, each with a
home, and each about the same thing: this tab renders a document it did not
write, about a repository it does not resolve.

**A ref becomes a card only if the daemon minted one.** `GET …/doc?resolve=
true` sends `cards[]` keyed by the ref body EXACTLY as the author wrote it,
and that key is the join — `lib/reviewDoc.ts`'s `refSpanFor` looks it up and
nothing else. The four outcomes are disjoint and all four RENDER: a card, a
`malformed` chip carrying the daemon's own reason, an `unresolved` chip
naming which of the two causes applies (the read asked for no cards, or the
daemon's scanner did not treat that span as prose), and a bare `[[X]]` left
as literal text because root invariant #29 owns that syntax. There is no
fifth branch in which a span quietly disappears. **Never widen this into
"parse it here and show a card anyway"**: kb-code is the only thing that can
say whether a citation still points at the bytes it claims, and a card this
browser invented would be exactly the wrong-`exact` failure the crate's
oracle bar calls a release blocker.

**An orphan is a VISIBLE card that says "no honest match", and it carries no
link.** `review_doc::cards` deliberately reports no position for one, so
there is nothing to link to and nothing to guess. The same is true of an
`inert` `gh:`/`kb:` card, for a different reason: kb-code never calls GitHub
and does not own the kb corpus, and a bare `gh:comment/12` names no host it
could resolve — `cardHref` returns `null` for both, and that null is
asserted. Worth knowing when reading the e2e spec: an orphan is
**unreachable through `compose`** (`ref_orphan` is a lint ERROR and a
document read is pinned to the patchset it was composed against), so
`review-doc.spec.ts` asserts the two honest failures that ARE reachable —
`malformed` (a `warn`) and `inert` — and the orphan's own rendering is pinned
in `lib/reviewDoc.test.ts`.

**Highlighting, trust, captions and counts are all the server's.** A card's
snippet is painted with `lib/diffHighlight.ts`'s `buildLineSpans`/`paintLine`
over the spans the daemon already rebased onto that snippet — a client-side
highlighter would be a second, disagreeing source of truth, so an unindexed
blob renders unpainted. Trust rides the shared `TrustBadge` (LINE STYLE, the
Lane Budget) and is omitted entirely when the daemon minted no tier, because
`trustTierFrom` classifies a missing class DOWN to `candidate` — which would
be a claim. `revisions`, `omitted[]` and the reading order's `source`/
`caption` are rendered verbatim. The ONE thing this side derives is the
act/blocking FACETING of the findings the wire already sent
(`findingFacets`), and it is a second VIEW of one list, never a second count.

**The Markdown path is `lib/markdownLite.ts` — extended, not replaced.**
V73-K2b added two block kinds, `heading` and `code` (fences), both behind
`MarkdownLiteOptions` and both OFF by default, so `ReportPanel`'s summary
stream is byte-identical to before. A review document opts into both, and the
fence support is load-bearing rather than cosmetic: `review_doc::refs`'s
scanner skips fenced blocks, so without a fence block kind this side would
find a ref the daemon never saw and render a card for it. `lib/kbcRefs.ts` is
the TS MIRROR of that scanner + the ref grammar, golden-pinned against the
SAME `crates/kb-code-server/grammar/kbcrefs.golden.json` the Rust parser
walks — the kbcq/1 discipline (§ Search grammar), applied to a second
grammar. Touch the grammar on one side and the other side's test fails,
naming the case.

**Keys.** Six new rows, all `scope: "review"` / `dispatch: "surface"` /
no `vim_kind`: `6` (`review.tab.doc`), `Space r` (`doc.cards-fold`),
`] r`/`[ r` (`doc.card-next`/`prev`), `Space o` (`doc.card-open`), `Space y`
(`doc.compose-copy`). `ReviewDetail.tsx` is the FIRST surface to publish
`scope: "review"` at all — which is why the same unit also registered the
five `review.tab.*` rows that had shipped in v7.0 and dispatched nowhere:
a live `6` beside a dead `1` is the silently-dead-row failure § Keyboard is
written to prevent. `] r`/`[ r` carry no vim arm, following the `] p`/`] u`/
`] d`/`] s` precedent exactly (`[`/`]` is a MIXED prefix; an arm would fire
the step twice), and none of the others needs one because this route mounts
no `CodeView` — `shouldWithholdFromBuffer` resolves in `"reader"` scope and
never sees a review-scope row. `commands/reviewDoc.test.ts` is the
per-surface gate `deadRows.test.ts` structurally cannot be (that suite gates
`central` rows only), in the shape `diffV2.test.ts` established. The rail's
ref list installs NO keydown handler: bare `j`/`k` in `review` scope already
belong to the inbox's own declared rows, and two homes for one keystroke is
what the registry exists to prevent.

**Authoring is not here.** Composing is loopback-only (D22, unchanged), so
the header offers the exact `kb-code review compose` line to COPY and the
lint panel is read-only. That is the same recorded CLI-parity posture
`lib/searchHistory.ts` carries for a saved search.

## `ReviewDiff`'s shape (`V73-K2b`)

`routes/ReviewDiff.tsx` is the SHELL: the queries, the page-wide derivations
(the moved-block index, the noise census, the map row states, the drafts),
the command handlers and the `live` ref bag the window keydown listener
reads. Everything else moved into `routes/reviewDiff/` — `helpers.ts` (the
`?param=` readers and `splatPath`/`orderedRows`), `useReviewDiffState.ts`
(every URL read and every URL writer, so "the URL is the only state" is
checkable by reading ONE file), `DiffSections.tsx` (`FileDiffBody`,
`LazyDiffSection`), and three props-only region components
(`ReviewDiffToolbar`, `ReviewDiffCenter`, `ReviewDiffRail`). That is the
shape `Reader.tsx` got in V70: a shell that owns state, with regions that own
none. The `live` bag stayed in the shell deliberately — it closes over two
dozen shell values, and threading them into a hook and back would add surface
without removing coupling.

## When to update this file

Add an invariant here when it lives entirely inside the SPA (`web-code/`)
and a contributor could break it without touching kb-code-server or kb
proper. The keyboard section above is the one every future bare-key or
central-dispatch row must be checked against — if you are adding a row and
are unsure whether it needs a vim-layer arm, the test is: "does this key
need to fire while a `CodeView` genuinely has focus?" If yes, it needs one.
Anything that is really about the SERVER'S shape of a registry, a wire
contract, or a migration belongs in
[kb-code-server/CLAUDE.md](../crates/kb-code-server/CLAUDE.md) instead —
cross-link rather than duplicate. Root [`/CLAUDE.md`](../CLAUDE.md) only
needs a one-line pointer update on a layout change; its own 35-slot
invariant index does not apply to this file.
