; queries/ruby-locals.scm — kb-code V71-E1 locals/scope query for Ruby
; (tree-sitter-ruby 0.23.1). Adapted from the grammar's own vendored
; `queries/locals.scm`, with three deliberate departures documented below.
;
; UNLIKE the other four `*-locals.scm` files this one is NOT wired into the
; occurrences ingest pass: `locals::supports` (the persisted
; `local_def_ordinal` stamping set) deliberately stays at the V3.G1 proof
; four. Ruby's binding is derived per REQUEST by `usages/2` instead
; (`intel::ruby_strict`), so adding it costs no salt bump and no re-extract
; of the 6.5K-file target corpus — and, per root CLAUDE.md invariant #2's
; posture, the trust class it feeds is computed per request and never
; persisted.
;
; Departure 1 — SCOPE BARRIERS. Upstream marks `(method)` with
; `(#set! local.scope-inherits false)`; `locals.rs` does not evaluate query
; predicates, so the flag is carried by a distinct CAPTURE NAME instead:
; `@local.scope.isolated`. A lookup that reaches an isolated scope stops
; there rather than continuing outward. This is not cosmetic: a Ruby method
; body does NOT close over the enclosing script's locals, so an inheriting
; walk would bind `total` inside `def foo` to a top-level `total = 1` and
; mint a WRONG exact — the release-blocker case. `class`/`module`/
; `singleton_class` bodies are barriers for the same reason; blocks and
; lambdas are NOT (they are real closures).
;
; Departure 2 — VISIBILITY SUFFIXES. Upstream captures every binding as a
; bare `@local.definition` (which `locals.rs` reads as EntireScope). Ruby
; locals are defined at PARSE time, so a read that precedes the assignment
; is `nil`-or-NameError territory rather than a use of the same value;
; assignment-derived bindings therefore carry `.var` (AfterDef) and only
; parameters carry `.parameter` (EntireScope). Classifying DOWN is the
; house rule (`locals.rs`'s `visibility_for_suffix`).
;
; Departure 3 — `for` loop targets are deliberately NOT captured. `for` is
; vanishingly rare in Rails code and its target leaks into the enclosing
; scope; unbound beats a wrong exact.

; --- Scopes ---------------------------------------------------------------

(program) @local.scope

; Barriers — see Departure 1.
(method) @local.scope.isolated

(singleton_method) @local.scope.isolated

(class) @local.scope.isolated

(module) @local.scope.isolated

(singleton_class) @local.scope.isolated

; Closures — these DO see the enclosing method's locals.
(block) @local.scope

(do_block) @local.scope

(lambda) @local.scope

; --- Definitions: parameters (EntireScope) --------------------------------

(method_parameters
  (identifier) @local.definition.parameter)

(block_parameters
  (identifier) @local.definition.parameter)

(lambda_parameters
  (identifier) @local.definition.parameter)

(destructured_parameter
  (identifier) @local.definition.parameter)

(splat_parameter
  name: (identifier) @local.definition.parameter)

(hash_splat_parameter
  name: (identifier) @local.definition.parameter)

(block_parameter
  name: (identifier) @local.definition.parameter)

(keyword_parameter
  name: (identifier) @local.definition.parameter)

(optional_parameter
  name: (identifier) @local.definition.parameter)

; --- Definitions: assignments (AfterDef) ----------------------------------

(assignment
  left: (identifier) @local.definition.var)

(operator_assignment
  left: (identifier) @local.definition.var)

(left_assignment_list
  (identifier) @local.definition.var)

(rest_assignment
  (identifier) @local.definition.var)

(destructured_left_assignment
  (identifier) @local.definition.var)

; `rescue StandardError => e` — the exception binding is a real local.
(exception_variable
  (identifier) @local.definition.var)

; --- References -----------------------------------------------------------

(identifier) @local.reference
