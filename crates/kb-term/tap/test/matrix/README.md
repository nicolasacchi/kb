# T1 twin-seed matrix (T6)

Acceptance gate for `buildSeed` + `QuiescentJoinGate`: the spike T1 matrix
ported to `bun test`, with the viewer seeded by `buildSeed(...)` instead of
bare `serialize()`.

## Families

- **Quiescent (asserted):** `pickQuiescentJoins` -- LF-boundary chunk ends
  outside any `?2026h..?2026l` interval at which a standalone `GroundTracker`
  scan of the whole (post-strip) stream is at ground. Stage A (seed
  fidelity) must be clean within `COSMETIC_RESIDUALS` plus the named
  residuals below; Stage B (convergence after the remainder) must be clean
  within `STATE_ALLOWLIST` (entries need a one-line root cause each) or
  the folded spec-6 residuals (cosmetic classes plus the named
  VISUAL_SAME_P16 / BLANK_FLAG_ATTR proofs over ALL differing cells) --
  there is no blanket class wildcard. Stage-B residual cell/row totals are
  counted across every quiescent join and must stay within the spike
  budgets (`SPIKE_BOUNDS`: cell chars 15, wide 7, extra rows 6, lost rows
  2). ESC 8 (DECRC) probe must read `same` at every join. A
  `QuiescentJoinGate` cross-check on a sample of the picks must agree.
  Every failing join is enumerated (subjective first-failure hiding is
  not used).
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

## DECSC slots, raw comparison and DECRC equivalence (FXT6)

`lib.ts`'s `stateOf` reports `savedX`/`savedY` RAW (absolute buffer
coordinates). Stage A compares them raw whenever the slot is in-viewport
on either side, so a wrong in-viewport CUP always fails (a one-row
mutation test pins this). A raw mismatch is permitted only when neither
side can express the slot: at least one slot sits outside its viewport
(scrollback-above `savedY < ybase`, past-end `savedY - ybase >= rows`,
or pending-wrap `savedX == cols`) AND the DECRC-restored positions still
agree (`decscStageAAllowed`). xterm 6.0.0 `restoreCursor` sets
`y = max(savedY - ybase, 0)` then `_restrictCursor` clamps to
`[0, rows-1]` (`x` likewise to `[0, cols-1]`; origin mode clamps to the
scroll margin, identical on both sides since the scroll region is
compared separately). The ESC 8 (`probeDecrc`) probe is enforced at
every join and witnesses exactly this restored cursor/pen. Out-of-viewport
slots survive via the seed's early prefix (saved while the viewer's
`ybase` is still 0) and the late tail's clamped re-save; two slots are
DECSC-equivalent in that regime iff their restored positions are equal.

## fg/bg colour: SerializeAddon P16 downgrade (visual-same)

Named residual `VISUAL_SAME_P16` ('SerializeAddon P16 downgrade for
palette indices < 16', FXT6 amendment of spec 6): SerializeAddon 0.14.0
emits palette indices 0-15 of a P256 colour as P16 codes (`30-37`/`90-97`),
so the viewer's raw fg/bg word differs in xterm's mode bits while the
displayed colour is identical. `fg colour`/`bg colour` is a cosmetic
residual ONLY when every differing cell of the class is visual-same
(`sameVisualColour`: same palette index, P16 vs P256, index < 16); the
matrix enumerates ALL differing cells (no `maxExamples` sample), and any
real colour change fails. Named residual `BLANK_FLAG_ATTR` ('serialize
drops flags on blank cells'): `attributes` diffs that are flag-only on
empty glyphs with visual-same colours are invisible. A minimal repro
(`4:3;58:5;42;38;5;9` on one cell) pins the P16 behaviour.

## Rejected join classes and size bounds

- **DEC-charset streams are a rejected join class (FXT6).** xterm 6.0.0
  buffers carry no `savedGlevel` and the CharsetService keeps only built
  maps, so designations/glevel cannot be re-emitted. `buildSeed` fails
  closed with the typed `SeedCharsetError` (`code` =
  `seed/charset-unsupported`) on any non-default charset/glevel. Matrix
  streams never set a DEC charset; the refusal is pinned in
  `seed-state.test.ts`.
- **Seed size bound (FXT6, amended):** desktop (50k scrollback) <= 5.5 MB
  (= 5.10 MB raw characters for 50,000 x 100-char lines + tail headroom),
  phone (20k) <= 2.2 MB, build time <= 3 s. The earlier 4.5 MB desktop
  number was structurally unreachable for the 60k-line fixture. Any
  further relaxation needs a README amendment plus a re-pinned assertion,
  never a comment alone. Pinned in `seed.test.ts`.
- **Pending wrap:** the tail rewrites the cell under a pending wrap (the
  cell at `cols-1`); a wide grapheme is rewritten verbatim and an empty
  cell (code 0) rewrites as a space. Pinned by golden in
  `seed-state.test.ts` (wide + empty cases).

## Adapters from the spike port

- `stateOf`/`diffState`/`compareBuffers`/`dumpRows` are re-exported from
  `test/support/compare.ts` (T5's port of the same spike). `lib.ts`'s
  `stateOf` wrapper recomputes pen/savedAttr signatures from the real
  `IAttributeData` (`{fg, bg, extended}`); the compare.ts port reads a
  nonexistent `extendedAttrs` field, which would hide pen underline diffs.
  The wrapper drops `_urlId`/`urlId` (link ids are per-terminal) and
  reports RAW saved coordinates; DECRC-restored equivalence is computed
  separately in `decscStageAAllowed` (see above).
- Term construction is T5's `makeTwin` (the spike's `makeTerm` is not
  copied). Width variants stay T5's (`graphemes`/`unicode11`); no width
  tables are ported here.
