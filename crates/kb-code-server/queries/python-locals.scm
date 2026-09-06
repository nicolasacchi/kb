; queries/python-locals.scm — kb-code V3.G1 locals/scope query for Python
; (tree-sitter-python 0.25.0). Conservative captures only.
;
; Visibility (applied in locals.rs):
;   .function / .class / .parameter / .var → EntireScope
;     Python has no TDZ: function/class names and assignments are visible
;     throughout their enclosing scope (including before the assignment in
;     source order). Comprehension targets live in the comprehension's own
;     scope (the comprehension node is @local.scope), so they do NOT leak.
;
; Capture convention: @local.scope / @local.definition* / @local.reference.

; --- Scopes ---------------------------------------------------------------

(module) @local.scope

(function_definition) @local.scope

(class_definition) @local.scope

(lambda) @local.scope

; Comprehension / generator scopes isolate their targets.
(list_comprehension) @local.scope

(set_comprehension) @local.scope

(dictionary_comprehension) @local.scope

(generator_expression) @local.scope

; for-statement body sees the loop target; make the for its own scope so
; the target does not leak to the enclosing block (Python 3 for-loop
; variables DO leak in real Python — but binding a leaked loop var as
; exact is a footgun for resolve; we deliberately scope the target to the
; for-statement so uses inside the loop bind exactly, and uses after the
; loop fall through rather than claiming exact. Classify DOWN when unsure.)
(for_statement) @local.scope

(except_clause) @local.scope

; --- Definitions: function / class (visible whole scope) -----------------

(function_definition
  name: (identifier) @local.definition.function)

(class_definition
  name: (identifier) @local.definition.class)

; --- Definitions: parameters ---------------------------------------------

(parameters
  (identifier) @local.definition.parameter)

(parameters
  (default_parameter
    name: (identifier) @local.definition.parameter))

(parameters
  (typed_parameter
    (identifier) @local.definition.parameter))

(parameters
  (typed_default_parameter
    name: (identifier) @local.definition.parameter))

(parameters
  (list_splat_pattern
    (identifier) @local.definition.parameter))

(parameters
  (dictionary_splat_pattern
    (identifier) @local.definition.parameter))

(lambda_parameters
  (identifier) @local.definition.parameter)

(lambda_parameters
  (default_parameter
    name: (identifier) @local.definition.parameter))

; --- Definitions: assignments --------------------------------------------
; `.variable` (not `.var`): Python has no TDZ — assignments are visible
; throughout the enclosing scope (see locals.rs visibility_for_suffix).

(assignment
  left: (identifier) @local.definition.variable)

(assignment
  left: (pattern_list
    (identifier) @local.definition.variable))

(assignment
  left: (tuple_pattern
    (identifier) @local.definition.variable))

(assignment
  left: (list_pattern
    (identifier) @local.definition.variable))

; --- Definitions: for / comprehension targets ----------------------------
; EntireScope within the for_statement / comprehension scope node.

(for_statement
  left: (identifier) @local.definition.parameter)

(for_statement
  left: (pattern_list
    (identifier) @local.definition.parameter))

(for_statement
  left: (tuple_pattern
    (identifier) @local.definition.parameter))

(for_in_clause
  left: (identifier) @local.definition.parameter)

(for_in_clause
  left: (pattern_list
    (identifier) @local.definition.parameter))

(for_in_clause
  left: (tuple_pattern
    (identifier) @local.definition.parameter))

; except bindings deliberately omitted: the `alias` field is typed as a
; generic expression in tree-sitter-python 0.25.0, and a bare
; `alias: (identifier)` pattern is rejected as impossible by the query
; engine. Prefer unbound over a wrong exact.

; --- References ----------------------------------------------------------

(identifier) @local.reference
