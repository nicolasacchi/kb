# T1 twin-seed matrix (T6)

Acceptance gate for `buildSeed` + `QuiescentJoinGate`: the spike T1 matrix
ported to `bun test`, with the viewer seeded by `buildSeed(...)` instead of
bare `serialize()`.

## Families

- **Quiescent (asserted):** `pickQuiescentJoins` -- LF-boundary chunk ends
  outside any `?2026h..?2026l` interval at which a standalone `GroundTracker`
  scan of the whole (post-strip) stream is at ground. Stage A (seed
  fidelity) must be clean within `COSMETIC_RESIDUALS`; Stage B (convergence
  after the remainder) must be clean within `STATE_ALLOWLIST` (starts empty;
  entries need a one-line root cause each, totals within the spike's
  15/7/6/2) **except** cosmetic cells carried from before the join (spec 6:
  they never re-render, so they persist at stage B by design) and the
  visual-same/blank-flag derivations. ESC 8 (DECRC) probe must read `same`
  at every join. A `QuiescentJoinGate` cross-check on a sample of the picks
  must agree. Every failing join is enumerated (subjective first-failure
  hiding is not used).
- **Raw (informational):** the spike's `pickJoins` offsets (including
  inside-frame) with bare `serialize()`, printed as a
  52/741-style clean count. Not asserted.

Joins per (variant, stream) come from `KB_MATRIX_JOINS` (default 40 on PRs,
100 for dispatch/nightly). Variants: `graphemes` (production pin) and
`unicode11`. Streams are generated at test time (see
`crates/kb-term/fixtures/streams/README.md`) and run through the production
`stripAll` transform first -- the twin always sees post-transform bytes.
`matrix.json` (schema `kbt-twin-matrix/1`) is written to `$KB_WITNESS_DIR`,
else `$RUNNER_TEMP`, else the OS temp dir.

## Cosmetic residuals (the documented v1 loss)

`COSMETIC_RESIDUALS = ['OSC 8 hyperlink', 'underline style',
'SGR 58 underline colour']`. `serialize()` cannot express per-cell links or
extended underline styles/colours, and replay would require walking
`_extendedAttrs`/`_oscLinkService` for cosmetic benefit. Rows already in the
buffer at join time lose them; new output after the join renders them
normally. Stage A may also report the derived `fg colour`/`attributes`/
`bg colour` on link-carrying cells (the spike's cell compare attributes
those to links). Structural classes (DECSTBM, DECSC, pen, cursor, pending
wrap, modes, alt state) are fixed fully by the seed tail and are asserted.
// FOLLOW-UP(T6b): per-cell replay of OSC 8 / underline style / SGR 58 if
// the operator rejects this loss (see the plan's Later section).

## DECSC slots and DECRC equivalence

A DECSC slot (`savedX`/`savedY`) is an absolute buffer coordinate
(`ybase` + viewport row). After `ybase` moves (scroll, ED2/ED3 trim),
the slot can sit outside `[ybase, ybase+rows-1]`: in scrollback
above (`viewportY < 0`) or past the buffer end (`viewportY >= rows`,
the post-ED3 stale slot). No `CUP` in a fresh viewer can name such
coordinates, so raw `savedY` equality is unreachable for them. The
seed's early prefix saves the slot while `ybase` is still 0 and the
late tail re-saves `CUP(clamp(viewportY, 0, rows-1))` for
`viewportY >= 0`; out-of-viewport slots therefore survive only as
DECRC-equivalent restored positions.

`lib.ts`'s `stateOf` wrapper compares the position DECRC restores:
xterm 6.0.0 `restoreCursor` sets `y = max(savedY - ybase, 0)` then
`_restrictCursor` clamps to `[0, rows-1]` (`x` likewise to
`[0, cols-1]`; origin mode clamps to the scroll margin, identical on
both sides since the scroll region is compared separately). Two
slots are DECSC-equivalent iff those clamped coordinates are equal.
Spec 9.3's `probeDecrc` (`ESC 8`) is enforced at every join, not
only when stage B shows a DECSC class, because it witnesses exactly
this restored cursor/pen. Note the spec 5(iii) `CUP(min(savedY,
rows-1))` formula is viewport-relative; the DECRC-equivalent
comparison is what makes it meaningful against absolute `savedY`.

## fg/bg colour: SerializeAddon P16 downgrade (visual-same)

SerializeAddon 0.14.0 emits palette indices 0-15 of a P256 colour
as P16 codes (`30-37`/`90-97`), so the viewer's raw fg/bg word
differs in xterm's mode bits while the displayed colour is
identical. The matrix classifies `fg colour`/`bg colour` as a
cosmetic residual only when every example is visual-same
(`sameVisualColour` in `lib.ts`: same palette index, P16 vs P256,
index < 16); any real colour change still fails. A minimal repro
(`4:3;58:5;42;38;5;9` on one cell) pins this behaviour.

## Adapters from the spike port

- `stateOf`/`diffState`/`compareBuffers`/`dumpRows` are re-exported from
  `test/support/compare.ts` (T5's port of the same spike). `lib.ts`'s
  `stateOf` wrapper recomputes pen/savedAttr signatures from the real
  `IAttributeData` (`{fg, bg, extended}`); the compare.ts port reads a
  nonexistent `extendedAttrs` field, which would hide pen underline diffs.
  The wrapper drops `_urlId`/`urlId` (link ids are per-terminal) and
  clamps the DECSC slot to DECRC-restored coordinates (see above).
- Term construction is T5's `makeTwin` (the spike's `makeTerm` is not
  copied). Width variants stay T5's (`graphemes`/`unicode11`); no width
  tables are ported here.
