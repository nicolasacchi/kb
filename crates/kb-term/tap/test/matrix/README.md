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
  15/7/6/2). ESC 8 (DECRC) probe must read `same` wherever stage B shows a
  `DECSC saved cursor` class. A `QuiescentJoinGate` cross-check on a sample
  of the picks must agree.
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

## Adapters from the spike port

- `stateOf`/`diffState`/`compareBuffers`/`dumpRows` are re-exported from
  `test/support/compare.ts` (T5's port of the same spike). `lib.ts`'s
  `stateOf` wrapper recomputes pen/savedAttr signatures from the real
  `IAttributeData` (`{fg, bg, extended}`); the compare.ts port reads a
  nonexistent `extendedAttrs` field, which would hide pen underline diffs.
  The wrapper drops `_urlId`/`urlId` (link ids are per-terminal).
- Term construction is T5's `makeTwin` (the spike's `makeTerm` is not
  copied). Width variants stay T5's (`graphemes`/`unicode11`); no width
  tables are ported here.
