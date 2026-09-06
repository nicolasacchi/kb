; queries/typescript-tags.scm — kb-code's vendored TypeScript symbol-tagging
; query (kb-code-server W2.2). Shared VERBATIM with queries/tsx-tags.scm
; (see that file's header for why one text works for both grammars).
;
; WHY THIS IS HAND-WRITTEN, NOT THE BUNDLED tree-sitter-typescript TAGS_QUERY:
; the official query (`tree_sitter_typescript::TAGS_QUERY`) only tags AMBIENT
; SIGNATURES (`function_signature`/`method_signature`/
; `abstract_method_signature`), `abstract_class_declaration`, `interface_
; declaration`, and a `module` pattern that can never match anything (the
; grammar's `module` node kind is an UNNAMED anonymous keyword token with no
; fields — see lang.rs/extract.rs's module docs). It misses ordinary
; function/class declarations, methods, type aliases, enums, namespaces, and
; const-bound arrow functions entirely — a known upstream footgun, not a
; packaging bug.
;
; This file is a hand-written MERGE, in kb-code's OWN capture convention (a
; bare `@name` on the identifier + `@definition.<kind>` wrapping the
; definition node — the convention the official tree-sitter `tags.scm` files
; and `extract.rs`'s same-span dedup logic both expect).
;
; LICENSE / ATTRIBUTION:
; - The signature / abstract-class / interface patterns below are carried
;   over from tree-sitter-typescript's own `queries/tags.scm` (MIT license,
;   https://github.com/tree-sitter/tree-sitter-typescript).
; - The additional node-kind COVERAGE (function_declaration,
;   method_definition, class_declaration, type_alias_declaration,
;   enum_declaration) was informed by Aider's `typescript-tags.scm`
;   (Apache-2.0, https://github.com/Aider-AI/aider —
;   aider/queries/tree-sitter-languages/typescript-tags.scm), REWRITTEN here
;   in kb-code's capture convention: Aider's own file uses a different one
;   (a single `@name.definition.<kind>` capture, no separate `@name`/
;   `@definition.<kind>` pair) that this crate's `extract.rs` doesn't parse.
; - Generator declarations, arrow-functions/function-expressions bound to
;   const/let/var, namespaces/modules (via the real `internal_module` node —
;   NOT the dead `module` pattern above), and exported top-level consts are
;   NEW patterns, hand-written for kb-code (present in neither upstream
;   source), following the same JS-aware shape as tree-sitter-javascript's
;   own official `tags.scm` (also MIT).

; --- Ambient signatures (declare/.d.ts-style bodies, interface members) ---

(function_signature
  name: (identifier) @name) @definition.function

(method_signature
  name: (property_identifier) @name) @definition.method

(abstract_method_signature
  name: (property_identifier) @name) @definition.method

; --- Functions ---

(function_declaration
  name: (identifier) @name) @definition.function

(generator_function_declaration
  name: (identifier) @name) @definition.function

; Arrow functions / function expressions bound to a name via const/let/var.
; MUST be declared before the exported-const pattern below: both patterns
; can capture the SAME `variable_declarator` node, and extract.rs resolves
; same-span duplicate captures by keeping the EARLIEST-declared pattern — so
; a function-valued export always classifies as "function", never "const".
(lexical_declaration
  (variable_declarator
    name: (identifier) @name
    value: [(arrow_function) (function_expression) (generator_function)]) @definition.function)

(variable_declaration
  (variable_declarator
    name: (identifier) @name
    value: [(arrow_function) (function_expression) (generator_function)]) @definition.function)

; --- Classes ---

(class_declaration
  name: (type_identifier) @name) @definition.class

(abstract_class_declaration
  name: (type_identifier) @name) @definition.class

; --- Methods ---

(method_definition
  name: (property_identifier) @name) @definition.method

; --- Interfaces ---

(interface_declaration
  name: (type_identifier) @name) @definition.interface

; --- Type aliases ---

(type_alias_declaration
  name: (type_identifier) @name) @definition.type_alias

; --- Enums ---

(enum_declaration
  name: (identifier) @name) @definition.enum

; --- Namespaces / modules ---
;
; Both `namespace Foo {}` and `module Foo {}` parse to `internal_module`
; (the grammar's separate `module` node kind is the anonymous keyword token
; itself, not this named node — see the header comment above).
(internal_module
  name: [(identifier) (nested_identifier)] @name) @definition.module

; --- Exported top-level constants ---
;
; Non-function values only: a function/arrow-valued declarator is already
; claimed by the pattern above (same-span dedup, earliest-pattern-wins).
(export_statement
  declaration: (lexical_declaration
    kind: "const"
    (variable_declarator
      name: (identifier) @name
      value: (_)) @definition.constant))
