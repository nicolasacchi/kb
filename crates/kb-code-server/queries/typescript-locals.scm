; queries/typescript-locals.scm — kb-code V3.G1 locals/scope query for
; TypeScript (tree-sitter-typescript 0.23.2 LANGUAGE_TYPESCRIPT).
; Shared pattern set with tsx-locals.scm (see that file's header).
;
; Merges tree-sitter-javascript's bundled locals.scm (MIT) scopes +
; variable/pattern defs with tree-sitter-typescript's parameter patterns
; and kb-code additions (function hoisting, for/of, catch, arrow params,
; destructuring). Conservative: when unsure, do not capture.

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
; tree-sitter-typescript 0.23.2: formal_parameters children are only
; required_parameter / optional_parameter (never bare identifier). The
; binding lives in the `name` field (or `pattern` for destructuring).

(required_parameter
  name: (identifier) @local.definition.parameter)

(optional_parameter
  name: (identifier) @local.definition.parameter)

(required_parameter
  pattern: (identifier) @local.definition.parameter)

(optional_parameter
  pattern: (identifier) @local.definition.parameter)

; Single-parameter arrow: `x => x + 1` (no parentheses).
(arrow_function
  parameter: (identifier) @local.definition.parameter)

; Destructuring parameters.
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
; EntireScope within the for_in_statement / catch_clause scope node.

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
