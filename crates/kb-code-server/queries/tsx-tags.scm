; queries/tsx-tags.scm — kb-code's vendored TSX symbol-tagging query
; (kb-code-server W2.2). IDENTICAL pattern set to queries/typescript-tags.scm:
; the TSX grammar (tree_sitter_typescript::LANGUAGE_TSX) is the TypeScript
; grammar plus JSX syntax — every node kind referenced below (function_
; declaration, class_declaration, method_definition, interface_declaration,
; type_alias_declaration, enum_declaration, internal_module, ...) exists
; identically in both grammars (pinned by lang.rs's own node-kind tests), so
; one query text works unmodified against both Language handles. Kept as a
; SEPARATE vendored file (not a shared include_str! of one physical file) so
; the two salts (typescript@.../tsx@...) each have their own on-disk query
; artifact to diff/attribute independently, matching this crate's one-
; query-file-per-language-id convention.
;
; See queries/typescript-tags.scm for the full LICENSE / ATTRIBUTION note
; (MIT tree-sitter-typescript upstream + Apache-2.0 Aider-informed coverage +
; kb-code hand-written additions) — it applies verbatim here.

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
