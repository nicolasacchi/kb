# The Lane Budget (kbc-theme/1)

kb-code stacks *lanes*, not one buffer. One line in the review diff can
simultaneously want to say: it is an addition; it is three days old; session
`abc123` wrote it; it carries a blocker finding; it has an LSP warning; it
contains an `exact` usage of the symbol you searched; it is unviewed; it
matches your speed-search. That is **nine signals and about six available
visual channels** (foreground colour, background tint, gutter A, gutter B,
underline style, left border, plus glyph and opacity as weak extras).

No theme file in the world resolves that. So the assignment is fixed here,
once, theme-independently:

> **Themes may RECOLOUR a lane. Themes may never REASSIGN one.**
> One channel per meaning, one meaning per channel.

This is the same discipline the rest of the project already runs on ("one
home per action", "surfaced-never-scored"). It is why a user-imported
base16 file from 2014 cannot break the trust vocabulary.

## The table

| Lane | Channel | Encoding | Tokens | Redundant non-colour cue |
|---|---|---|---|---|
| Diff add / del / mod | line background tint | `--bg` mixed with the diff hue at 10% (line) / 22% (intraline word) in OKLCH | `--diff-{add,del,mod}`, `--diff-*-bg`, `--diff-*-bg-strong`, `--diff-*-gutter`, `--diff-*-fg` | the `+` / `-` / `~` **sign column, always present** — this is the load-bearing cue, the tint is reinforcement |
| Blame / age | **gutter A** | single-hue **lightness** ramp, 10 quantized bands, newest = strongest | `--age-band-0` … `--age-band-9` | date on hover; a **legend is mandatory** (the warm-vs-cool and bright-vs-dark conventions contradict each other across GitLens / GitLab / JetBrains, so the ramp cannot be self-explanatory) |
| Provenance (session / author) | gutter A **hue**, a 2 px rail | 8 stable session hues, spread by maximal pairwise Oklab distance across the theme's own accent wheel | `--prov-hue-0` … `--prov-hue-7` | author initials on hover; agent-authored carries its own badge |
| Diagnostics | **gutter B** | status colour glyph | `--red` / `--warn` / `--blue` / `--ink-dim` | a distinct **icon shape per severity**, never colour alone |
| Review lanes (findings / comments / github) | **gutter C** + right rail | status colour chip | `--red` / `--warn` / `--green` / `--accent` | per-lane icon + count badge |
| Usage trust class | **underline / border STYLE** | `exact` solid · `likely` dashed · `candidate` dotted · `observed` dotted **+ glyph** — all in ONE hue | `--trust-hue`, `.kbc-trust-{exact,likely,candidate,observed}` | the style *is* the cue; the trust badge repeats it in words |
| Occurrence / speed-search | selection-family background | `--hl-med` / `--hl-high` | `--hl-low`, `--hl-med`, `--hl-high` | ephemeral, keyboard-driven |
| Viewed state | left border + text opacity | 100% unviewed → 78% viewed | `--rule-strong` | the "viewed" checkbox in the file header |
| Sticky context / current scope | `--bg-elev` band | — | `--bg-elev` | the pinned header itself |
| Bookmarks / TODO | gutter A glyph slot (glyph over the rail) | `--accent` | — | the glyph |

**Precedence when two lanes want the same channel:**
`diff bg > review-lane bg > occurrence > sticky-context > viewed-dim`.

## Why trust is a line style and not a hue

`exact` / `likely` / `candidate` / `observed` are the project's hardest
invariant — the oracle bar says a wrong `exact` is a release blocker.
Encoding them in **line style** rather than hue makes them structurally
immune to any theme, any colour-vision deficiency and any contrast slider.
A user can import the worst-contrast palette on the internet and the trust
vocabulary still reads. `observed` adds a glyph on top because it is a
different *kind* of evidence (runtime), not merely a weaker one.

## The contrast contract

`npm run lint:themes` is the executable gate (`web-code/scripts/theme-lint.mjs`).
It checks only the pairs this table says can **co-occur** — the role lists it
enumerates are exported from `derive.ts` and are the same lists the AA repair
pass measures against, so the gate and the generator can never disagree.

| Lane | Severity | Floor |
|---|---|---|
| text legibility (WCAG 2.x) | **fail** | 4.5:1 text · 3.0:1 non-text (`--strict` → 7:1 / 4.5:1) |
| APCA Lc | warn | Lc 30 (reported for every pair; per-theme minimum in the summary) |
| state separation, normal vision | **fail** | per-pair Oklab distance |
| CVD-simulated (protan / deutan / tritan) | warn | same floors |
| syntax mutual distinctiveness | warn | 0.05 ΔOklab |

CVD warns rather than fails **because of this table**: every CVD-fragile lane
above already carries a redundant non-colour cue, so a hue collapse under
deuteranopia costs polish, not information. Failing there would reject every
red/green palette in existence, which is not an accessibility win.

Syntax distinctiveness warns because of Flexoki's finding: perfect perceptual
uniformity fights the distinctiveness syntax colouring exists for. When they
conflict, **distinctiveness wins**.

`--ink-faint` is measured and reported but never failed — tokens.css's own
SH.C1 note already scopes it to "decorative rules/disabled glyphs only, never
running text", which puts it outside WCAG's text and non-text scope alike.

## The reading contract

Three tokens, defined in `tokens.css`, **no behaviour change by default** —
they exist so the surfaces that want them can opt in:

- `--measure: 100ch` — the maximum line box for a code region. Applied only
  where a container opts in; the reader's current width is untouched.
- `--lh-code: 1.5` — the code line-height floor.
- `data-density="comfortable" | "compact"` on `<html>`, driving `--density-pad`
  and `--density-row`. Default `comfortable` reproduces today's spacing exactly.
