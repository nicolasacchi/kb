# web-code — internals

Sibling of the workspace-root [`/CLAUDE.md`](../CLAUDE.md) and of
[`crates/kb-code-server/CLAUDE.md`](../crates/kb-code-server/CLAUDE.md) (D19 of
the kb-code v7 "The Continuum" design of record). This file holds invariants
that live entirely inside the SPA — above all its keyboard/command dispatch
contract, which is what actually broke, three separate times, during the v7.0
build. Root CLAUDE.md's crate-layout section and
[README.md](../README.md)'s "kb-code" section are the feature-level surface;
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

Three separate regressions and one root-cause fix all trace to the SAME
structural gap. If you add a bare key (a key with no `Ctrl`/`Alt`/`Meta`
modifier) to `registry.json`, or a handler for an existing row, read this
section in full — it is written to stop a fourth regression, not to narrate
the first three.

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
call that actually registers it before you trust the key does anything** —
there is no test today that a shipped central row has a registered handler
in every reachable route (a real gap; a future unit's job, not this one's).

**A bare key can be swallowed by the CM6 buffer guard — the predicate is
precise, and getting it wrong breaks either one key or thirty-five.**
`CommandRoot`'s guard 2 (`shouldWithholdFromBuffer`, exported from
`commands/CommandRoot.tsx`) exists because the vim layer installs at
`Prec.highest` inside a `CodeView` and reacts to bare keys itself — so a
bare key inside the read-only reader buffer must be decided ONCE: does vim
own it, or does it fall through to `CommandRoot`'s dispatcher? Two
regressions shipped from getting this wrong in two different ways before the
predicate was fixed at the root (`V70-K1`):

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

The corrected predicate (`shouldWithholdFromBuffer(activeElement, token,
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
