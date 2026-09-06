; queries/go-tags.scm — kb-code's vendored Go symbol-tagging query
; (kb-code-server W2.6).
;
; WHY THIS IS HAND-WRITTEN, NOT THE BUNDLED tree-sitter-go TAGS_QUERY:
; the official query (`tree_sitter_go::TAGS_QUERY`) tags `@definition.function`
; (function_declaration), `@definition.method` (method_declaration), and
; `@definition.type` (type_spec — covers plain type DEFINITIONS
; (`type Point struct {...}`), struct_type, AND interface_type alike,
; disambiguated below via extract.rs's `map_kind`, same convention as Rust's
; `class` suffix covering struct/enum/union/type_item). It does NOT capture
; `type_alias` (`type Alias = int` — the `=` form is a SEPARATE grammar node
; kind from `type_spec`, confirmed empirically: `type X = Y` parses to
; `(type_alias name: (type_identifier) type: (_))`, not `(type_spec ...)` —
; not obvious from a skim of node-types.json's `type_declaration.children`
; list, which undersells it), so this file adds that pattern too. Its
; `const_declaration`/`var_declaration` patterns capture `@name` on the
; identifier but never wrap the declaration in a `@definition.*` group — so
; extract.rs's "a match needs BOTH a `definition.*` capture AND a `name`
; capture" rule (see extract.rs's module doc) silently drops them. W2.6's
; scope wants const/var indexed as real symbols (func/method/type/struct/
; interface/const/var), so this file is the official query PLUS
; `@definition.constant`/`@definition.variable` wrapping on `const_spec`/
; `var_spec`. The upstream doc-comment-adjacency directives (`@doc` capture,
; `#strip!`, `#set-adjacent!`) and the `@reference.*`/bare-`@name` patterns
; (call-site references, `package_clause`, `import_declaration` — never
; indexed as definitions, see extract.rs's module doc on `@reference.*`) are
; dropped too: kb-code's `Symbol` has no doc-comment field to populate, and
; extract_symbols only ever looks at `@definition.*` captures.
;
; LICENSE / ATTRIBUTION: the function/method/type patterns below are carried
; over verbatim (structurally) from tree-sitter-go's own
; `queries/tags.scm` (MIT license, https://github.com/tree-sitter/tree-sitter-go).
; The const/var patterns are NEW, hand-written for kb-code, following the
; same node shape (`const_spec`/`var_spec`'s `name:` field) the upstream file
; already matches on elsewhere.
;
; A `const`/`var` block with MULTIPLE comma-separated names on one spec
; (`const a, b, c = 1, 2, 3`) is a KNOWN accepted limitation: `@definition.*`
; wraps the whole `const_spec`/`var_spec` node (for a span consistent with
; every other kind here — the full declaration, not just one identifier), so
; extract.rs's same-span dedup (keyed on the captured definition node's byte
; range, not the name) collapses a multi-name spec to ONE row (whichever name
; the query engine visits first — in practice the first-declared name).
; Realistic Go const/var blocks overwhelmingly declare one name per spec
; (`const MaxRetries = 3`); the grouped multi-name form is rare enough that
; a future pass can special-case it (capture the identifier itself, not the
; spec, as `@definition.*`) if it turns out to matter in practice.

(function_declaration
  name: (identifier) @name) @definition.function

(method_declaration
  name: (field_identifier) @name) @definition.method

(type_spec
  name: (type_identifier) @name) @definition.type

(type_alias
  name: (type_identifier) @name) @definition.type

(const_spec
  name: (identifier) @name) @definition.constant

(var_spec
  name: (identifier) @name) @definition.variable
