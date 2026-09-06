; queries/tsx-locals.scm — kb-code V3.G1 locals/scope query for TSX
; (tree-sitter-typescript 0.23.2 LANGUAGE_TSX). Pattern set is deliberately
; the same as typescript-locals.scm (the two grammars share the JS/TS
; statement + parameter shapes this query uses; JSX-specific nodes do not
; introduce extra local bindings we model). Kept as its own file/const so
; each grammar compiles against its own Language handle — same discipline
; as tsx-tags.scm vs typescript-tags.scm.
;
; See typescript-locals.scm for capture convention + visibility notes.

; --- Scopes ---------------------------------------------------------------

(program) @local.scope

; Note: tree-sitter-typescript 0.23.2 has NO separate `for_of_statement`
; node — both `for…in` and `for…of` are `for_in_statement`.
[
  (statement_block)
  (function_expression)
  (arrow_function)
  (function_declaration)
  (generator_function_declaration)
  (method_definition)
  (class_declaration)
  (for_in_statement)
  (for_statement)
  (catch_clause)
] @local.scope

; --- Definitions: function declarations (hoisted in their scope) ---------

(function_declaration
  name: (identifier) @local.definition.function)

(generator_function_declaration
  name: (identifier) @local.definition.function)

; --- Definitions: parameters ---------------------------------------------
; See typescript-locals.scm — same grammar field shapes.

(required_parameter
  name: (identifier) @local.definition.parameter)

(optional_parameter
  name: (identifier) @local.definition.parameter)

(required_parameter
  pattern: (identifier) @local.definition.parameter)

(optional_parameter
  pattern: (identifier) @local.definition.parameter)

(arrow_function
  parameter: (identifier) @local.definition.parameter)

(required_parameter
  pattern: (object_pattern
    (shorthand_property_identifier_pattern) @local.definition.parameter))

(required_parameter
  pattern: (array_pattern
    (identifier) @local.definition.parameter))

(optional_parameter
  pattern: (object_pattern
    (shorthand_property_identifier_pattern) @local.definition.parameter))

(optional_parameter
  pattern: (array_pattern
    (identifier) @local.definition.parameter))

; --- Definitions: let / const / var --------------------------------------

(variable_declarator
  name: (identifier) @local.definition.var)

(variable_declarator
  name: (object_pattern
    (shorthand_property_identifier_pattern) @local.definition.var))

(variable_declarator
  name: (object_pattern
    (pair_pattern
      value: (identifier) @local.definition.var)))

(variable_declarator
  name: (array_pattern
    (identifier) @local.definition.var))

; --- Definitions: for-loop / catch bindings ------------------------------

(for_in_statement
  left: (identifier) @local.definition.parameter)

(for_in_statement
  left: (object_pattern
    (shorthand_property_identifier_pattern) @local.definition.parameter))

(for_in_statement
  left: (array_pattern
    (identifier) @local.definition.parameter))

(catch_clause
  parameter: (identifier) @local.definition.parameter)

; --- References ----------------------------------------------------------

(identifier) @local.reference
