; queries/rust-locals.scm — kb-code V3.G1 locals/scope query for Rust
; (tree-sitter-rust 0.24.2). Conservative: only captures bindings we are
; confident about. An unbound identifier falls through to the existing
; resolve ladder; a WRONG binding poisons exact-class results.
;
; Capture convention (tree-sitter locals):
;   @local.scope            — introduces a lexical scope
;   @local.definition       — a binding (optional .function/.parameter/.var
;                             subtype drives visibility in locals.rs)
;   @local.reference        — a name use to resolve against definitions
;
; Visibility (applied in locals.rs, not here):
;   .function  → EntireScope (item-style, visible throughout the scope)
;   .parameter → EntireScope (visible in the whole function/closure body)
;   .var       → AfterDef    (let/for/match: only after the enclosing
;                             declaration ends — so `let a = a` does NOT
;                             self-bind)

; --- Scopes ---------------------------------------------------------------

(source_file) @local.scope

(function_item) @local.scope

(closure_expression) @local.scope

(block) @local.scope

(for_expression) @local.scope

(match_expression) @local.scope

(match_arm) @local.scope

(impl_item) @local.scope

(mod_item) @local.scope

; --- Definitions: functions (hoisted within their scope) ------------------

(function_item
  name: (identifier) @local.definition.function)

; --- Definitions: parameters ---------------------------------------------

(parameter
  pattern: (identifier) @local.definition.parameter)

(parameter
  pattern: (mut_pattern
    (identifier) @local.definition.parameter))

(parameter
  pattern: (reference_pattern
    (identifier) @local.definition.parameter))

(parameter
  pattern: (reference_pattern
    (mut_pattern
      (identifier) @local.definition.parameter)))

; Closure parameters — bare identifier form.
(closure_parameters
  (identifier) @local.definition.parameter)

(closure_parameters
  (mut_pattern
    (identifier) @local.definition.parameter))

; --- Definitions: let bindings -------------------------------------------

(let_declaration
  pattern: (identifier) @local.definition.var)

(let_declaration
  pattern: (mut_pattern
    (identifier) @local.definition.var))

(let_declaration
  pattern: (tuple_pattern
    (identifier) @local.definition.var))

(let_declaration
  pattern: (tuple_pattern
    (mut_pattern
      (identifier) @local.definition.var)))

; --- Definitions: for-loop / match bindings ------------------------------
; EntireScope (.parameter): the for_expression / match_arm node is itself
; a @local.scope, so "visible whole scope" is exactly the loop/arm body —
; AfterDef would push visible_from to the END of the for (after the body)
; and leave body refs unbound.

(for_expression
  pattern: (identifier) @local.definition.parameter)

(for_expression
  pattern: (mut_pattern
    (identifier) @local.definition.parameter))

(for_expression
  pattern: (tuple_pattern
    (identifier) @local.definition.parameter))

(match_pattern
  (identifier) @local.definition.parameter)

(match_pattern
  (mut_pattern
    (identifier) @local.definition.parameter))

; Struct field shorthand `Foo { x }` — the shorthand IS the binding.
; Deliberately skip `Foo(x)` tuple-struct patterns here: a bare
; `(tuple_struct_pattern (identifier))` would also match the type-name
; field (`Foo`), poisoning exact. Prefer unbound over wrong.
(field_pattern
  name: (shorthand_field_identifier) @local.definition.var)

(field_pattern
  pattern: (identifier) @local.definition.var)

(field_pattern
  pattern: (mut_pattern
    (identifier) @local.definition.var))

; --- References ----------------------------------------------------------

(identifier) @local.reference
