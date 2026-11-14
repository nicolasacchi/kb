# T1 twin-seed matrix (T6)

Acceptance gate for `buildSeed` + `QuiescentJoinGate`: the spike T1 matrix
ported to `bun test`, with the viewer seeded by `buildSeed(...)` instead of
bare `serialize()`.

## Families

- **Quiescent (asserted):** `pickQuiescentJoins` -- LF-boundary chunk ends
  outside any `?2026h..?2026l` interval at which a standalone `GroundTracker`
  scan of the whole (post-strip) stream is at ground. Stage A (seed
  fidelity) must be clean within `COSMETIC_RESIDUALS` plus the named
  residuals below. Stage B (convergence after the remainder) must be
  clean within a `STATE_ALLOWLIST` exact `{variant, stream, class}` entry
  or the folded spec-6 residuals (cosmetic classes plus the named
  VISUAL_SAME_P16 / BLANK_FLAG_ATTR proofs over ALL differing cells) or
  the named DECSC restored-position residual below -- there is no blanket
  class wildcard. Every allowed stage-B class (cells AND state) is
  counted under its class key across every quiescent join; the
  spike-bound test (`SPIKE_BOUNDS`: cell chars 15, wide 7, extra rows 6,
  lost rows 2) bounds the CELL/ROW classes and enumerates the named
  residuals (cosmetic, VISUAL_SAME_P16, BLANK_FLAG_ATTR,
  DECSC_STAGEB_CLASS), so state classes sit outside the cell/row budget
  but are never silent. ESC 8 (DECRC) probe must read `same` at every
  join. A `QuiescentJoinGate` cross-check on a sample of the picks must
  agree. Every failing join is enumerated (subjective first-failure
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

## DECSC slots, raw comparison, DECRC equivalence and named residuals (FXT6)

`lib.ts`'s `stateOf` reports `savedX`/`savedY` RAW (absolute buffer
coordinates). Stage A compares them raw whenever the slot is in-viewport
on either side, so a wrong in-viewport CUP always fails (a one-row
mutation test guards this). The DISCRIMINATING raw-vs-clamped pin is the
ybase-delta test: twin and viewer with ybase 0 vs >0 save the SAME
viewport row with different absolute savedY -- clamped stateOf compared
that clean; the raw stage-A comparison must flag it, and it does
(`decscStageAAllowed`). A raw mismatch is permitted only when neither
side can express the slot: at least one slot sits outside its viewport
(scrollback-above `savedY < ybase`, past-end `savedY - ybase >= rows`,
or pending-wrap `savedX == cols`) AND the DECRC-restored positions still
agree. xterm 6.0.0 `restoreCursor` sets `y = max(savedY - ybase, 0)` then
`_restrictCursor` clamps to `[0, rows-1]` (`x` likewise to `[0, cols-1]`;
origin mode clamps to the scroll margin, identical on both sides since
the scroll region is compared separately). Out-of-viewport slots survive
via the seed's early prefix (saved while the viewer's `ybase` is still 0)
and the late tail's clamped re-save; two slots are DECSC-equivalent in
that regime iff their restored positions are equal.

Named stage-B residual `DECSC_STAGEB_CLASS` (decision:
`decscStageBAllowed`; root cause: DECRC-restored viewport equivalence.
(full stream) and seed viewer (seed + remainder) can share exactly the
same viewport slot while their absolute savedY differs by the ybase delta
(different history lengths after ED3 trims): no CUP can reconcile that
from the seed, DECRC ignores it, and the viewport position is what the
user sees. Stage-B coordinate diffs whose DECRC-restored positions agree
are allowed under THIS named root cause -- not a blanket -- and every
such exemption is counted under the `DECSC_STAGEB_CLASS` key and
enumerated by the spike-bound test. The four `STATE_ALLOWLIST`
`harness-s6`/`harness-s7` `DECSC saved cursor` entries remain for the
narrower case the named residual cannot cover (a restored position that
genuinely differs on those streams); their exemptions are counted under
the same key. A stage-B DECSC diff whose restored position DIFFERS and
is not EXACTLY allowlisted must fail (a dedicated negative test pins
this). The ESC 8 (`probeDecrc`) probe is enforced at every join and is
the ultimate arbiter of DECSC equivalence. Stage A is unaffected: raw
in-viewport mismatch still fails stage A even when stage B would allow
it under the named residual.

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
  cell at `cols-1`); the wrap cell of a wide grapheme is its trailing
  spacer (blank, code 0) so it is rewritten with a space (not the
  grapheme verbatim), and an empty cell (code 0) likewise rewrites as a
  space (`readWrapCell` returns blank). The golden pins CUP(1,10)+SGR+space
  plus convergence. Pinned by golden in `seed-state.test.ts` (wide +
  empty cases).

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
