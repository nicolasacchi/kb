# `haml/1` — the divergence corpus

41 synthetic HAML templates, and beside each one `<name>.expected.json`:
the **real `haml` gem's own `Haml::Parser`** output, projected into the
shape [`src/haml/projection.rs`](../../../src/haml/projection.rs) produces
from kb-code's first-party scanner.
[`tests/haml_corpus.rs`](../../haml_corpus.rs) diffs the two.

## The rule about the gem

> **The `haml` gem is never invoked by CI, and never by the daemon.**

The expectations here were generated **once, offline, by hand**, on a
developer box, and the *output* is what is checked in. `just ci-code` is
pure Rust reading these JSON files; nothing in the build depends on Ruby
existing anywhere, and kb-code-server's
[invariant 10](../../../CLAUDE.md) ("the daemon never spawns a non-git
process") is untouched. `tests/haml_corpus.rs` carries a test that greps
its own source for `Command::new` so this cannot quietly stop being true.

## What generated these

| | |
|---|---|
| generator | [`generate_expected.rb`](generate_expected.rb) (in this directory) |
| gem | **haml 7.5.1** |
| ruby | 3.4.10 |
| generated | 2026-09-06 |
| status | **gem-verified** — every expectation is the real parser's output, not hand-authored |

To regenerate after adding or changing a fixture:

```sh
GEM_HOME=/tmp/haml-oracle gem install haml -v 7.5.1 --no-document
cd crates/kb-code-server/tests/fixtures/haml
GEM_HOME=/tmp/haml-oracle ruby generate_expected.rb
```

The generator **aborts, naming the file**, if the gem rejects a fixture: a
corpus fixture must be legal HAML, because the gem is the oracle. (Two of
the first-draft fixtures were caught this way — HAML rejects a dynamic
class shorthand `%li.item-#{i}` outright, and rejects a tag with both
inline content and nested children.)

## The projection, and its three normalisations

The two models are not the same model. The scanner keeps things the gem
discards (the sigil that produced a script, every interpolation's byte
range, whether a value was reassembled from a `|` continuation); the gem
keeps things the scanner has no use for (`dynamic_attributes`,
`preserve_tag`). The projection is the **intersection** — the facts a
reader would call "the structure of this template" — and both sides emit
it.

Three normalisations are applied, on both sides, each documented in
`projection.rs`'s module doc as well. **Anything not on this list that
differs is a real divergence and the corpus test is supposed to go red.**

1. **Ruby and text values are trimmed.** HAML reports `= foo` as the Ruby
   `" foo"` (leading space kept) and `%p foo` as the text `"foo"`
   (stripped); a `|` join leaves a trailing space. None of that reaches a
   tree-sitter parse differently.
2. **An absent inline value and an empty one are the same thing.** The gem
   reports `nil` for a tag with children and `""` for a childless tag that
   merely has attributes; both mean "no inline content".
3. **Interpolated text is projected as the gem's `script` node.** HAML
   compiles `%p a #{b}` into the Ruby string literal `"a #{b}"`, and
   `== a` / `& a` / `! a` likewise. `projection::quote_ruby` reproduces
   exactly that transform, so both sides compare the same node kind
   instead of declaring a permanent structural difference.

## Recorded differences that are NOT normalised away

* **The scanner accepts a dynamic class/id shorthand (`%li.item-#{i}`);
  HAML rejects it.** A deliberate superset: refusing a whole file over a
  shorthand a reader clearly meant would be the opposite of a read-first
  instrument. It cannot appear in this corpus (the gem cannot produce an
  expectation for input it rejects), so it is pinned by a unit test in
  `src/haml/tests.rs` instead.
* **The scanner captions malformed input; HAML raises.** Tabs in the
  indentation, a dedent landing between two open levels, an unbalanced
  `{`, an unterminated `#{`: HAML's parser raises a `SyntaxError` and
  produces nothing. The scanner records a
  `haml::DiagnosticKind` and returns whatever structure was recoverable.
  That is why no corpus fixture is malformed — malformed input is covered
  by `haml_corpus.rs`'s mutation sweep, which asserts *no panic* and *no
  out-of-bounds span* over ~500 byte-level mutations of these same files.

## Fixture inventory

| # | file | what it pins |
|---|---|---|
| 01 | `01-doctype.haml` | `!!! 5`, the version/type split |
| 02 | `02-basic-elements.haml` | `%tag`, nesting, inline text |
| 03 | `03-implicit-div.haml` | `.class` / `#id` shorthands, implicit `%div` |
| 04 | `04-nesting.haml` | dedent back to several levels |
| 05 | `05-inline-text.haml` | inline text, quotes, whitespace stripping |
| 06 | `06-interpolation-text.haml` | `#{…}` in text, bare interpolated line |
| 07 | `07-script-sigils.haml` | `=` `==` `~` `&=` `!=` `&` `!` |
| 08 | `08-silent-script.haml` | `-` lines, a `do` block with no keyword |
| 09 | `09-if-else.haml` | `if` / `elsif` / `else` mid-block flattening |
| 10 | `10-case-when.haml` | `case` / `when` / `else` |
| 11 | `11-begin-rescue.haml` | `begin` / `rescue` / `ensure` |
| 12 | `12-each-do.haml` | `do |x|` blocks, block args |
| 13 | `13-form-block.haml` | an OUTPUT script that opens a block |
| 14 | `14-ruby-attrs.haml` | `{…}` Ruby hashes, nested hashes |
| 15 | `15-html-attrs.haml` | `(…)` HTML-style literal pairs |
| 16 | `16-mixed-attrs.haml` | shorthands + `(…)` + `{…}` on one tag |
| 17 | `17-multiline-attrs.haml` | an attribute hash spanning three lines |
| 18 | `18-nested-braces-attrs.haml` | `}` inside a string inside `#{}` inside `{}` |
| 19 | `19-object-ref.haml` | `[…]` object references, with and without attrs |
| 20 | `20-self-close.haml` | `%tag/` and implicitly void tags |
| 21 | `21-whitespace-removal.haml` | `>` `<` `><` |
| 22 | `22-haml-comment.haml` | `-#` and its swallowed body |
| 23 | `23-html-comment.haml` | `/` with text and with children |
| 24 | `24-conditional-comment.haml` | `/[if IE]` |
| 25–30 | `25…30-filter-*.haml` | `:javascript` `:css` `:ruby` `:markdown` `:plain` `:preserve` `:escaped` `:erb`, an UNKNOWN filter, and blank lines inside a filter body |
| 31 | `31-escape-backslash.haml` | `\` escaping `=` `-` `%` `.` |
| 32 | `32-pipe-multiline.haml` | `|` continuations, in a script and in text |
| 33 | `33-trailing-comma.haml` | trailing-comma continuations |
| 34 | `34-render-partial.haml` | `render` in every shape the lens resolves |
| 35 | `35-i18n-keys.haml` | `t(".key")`, `t("a.b")`, `I18n.t`, a key in an attribute |
| 36 | `36-unicode.haml` | non-ASCII text, class names and keys |
| 37 | `37-deep-nesting.haml` | five levels, then dedents to each |
| 38 | `38-dynamic-classes.haml` | dynamic classes *the legal way* (attribute hash) |
| 39 | `39-blank-lines.haml` | blank lines between and inside blocks |
| 40 | `40-rails-view.haml` | a realistic view: `content_for`, blocks, render, i18n, a component |
| 41 | `41-interp-edge-cases.haml` | nested braces, nested interpolation, `\#{}`, interpolation inside an attribute string |
