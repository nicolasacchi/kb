; queries/bash-tags.scm — kb-code's vendored Bash symbol-tagging query
; (kb-code-server W2.2).
;
; tree-sitter-bash ships NO `tags.scm` at all (only `HIGHLIGHT_QUERY` — see
; lang.rs's doc comment on `tags_query`), so there is no upstream file to
; adapt here; this is a small from-scratch query, TRIMMED deliberately (per
; the W2.2 scope) to function definitions only:
;
; - `greet() { ... }`      — the "POSIX" form
; - `function greet { ... }` / `function greet() { ... }` — the "bashism" form
;
; tree-sitter-bash's grammar already unifies BOTH surface forms into ONE
; node kind, `function_definition`, with a `name: (word)` field — so one
; pattern covers both; no separate `function_definition` variants to merge
; (pinned by extract.rs's bash fixture test, which exercises both forms).
;
; Top-level variable assignments (`FOO=bar`) are deliberately NOT tagged as
; constants here — plain top-level bash assignments are extremely common and
; rarely meaningful "symbols" in a browsing sense (unlike, say, an exported
; TypeScript const); indexing every one would be noise, not signal. See
; extract.rs's `map_kind` doc for the kind mapping.

(function_definition
  name: (word) @name) @definition.function
