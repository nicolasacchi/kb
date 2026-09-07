//! `kbc-recipe/1` unit tests (V74-L3a).
//!
//! Everything here is pure: loading, the DAG type-check, the closed op
//! set, trust-on-first-use, param validation, the caps refusal, the
//! census vocabulary and the view renderer. The engine-backed half
//! (running the ops against a real mirror, determinism across two runs,
//! materialise + replay, the six repairs) lives in
//! `tests/http_pack/recipe_route.rs`, which has a repo.

use super::census::EmptyReason;
use super::loader::{trust_for, RepoFile};
use super::ops::Op;
use super::run::{validate_params, Context};
use super::*;

fn doc(toml: &str) -> Result<RecipeDoc, LoadError> {
    load_toml(toml)
}

const MINIMAL: &str = r#"
slug = "t:min"
title = "Minimal"
intent = "orienting"
steps = [ { id = "files", op = "tree", args = { limit = 10 } } ]
views = [ { id = "v", kind = "list", step = "files", columns = [ { field = "path" } ] } ]
"#;

#[test]
fn a_minimal_recipe_loads() {
    let d = doc(MINIMAL).expect("loads");
    assert_eq!(d.slug, "t:min");
    assert_eq!(
        d.scope, "$context.scope",
        "scope defaults to the reader's own"
    );
}

// --- the eight builtins ----------------------------------------------------

/// The shipped catalog goes through the SAME loader a repo file does, so
/// this is a real test of the loader, not a smoke test of a struct
/// literal.
#[test]
fn every_builtin_loads_and_type_checks() {
    let all = builtins::all();
    assert_eq!(
        all.len(),
        builtins::DAG_BUILTINS.len() + builtins::NATIVE_SLUGS.len(),
        "eight DAG built-ins + six native adapters"
    );
    assert_eq!(builtins::DAG_BUILTINS.len(), 8, "D11 ships eight");
    for r in &all {
        assert!(!r.doc.title.is_empty(), "{}: title", r.doc.slug);
        assert!(
            INTENT_GROUPS.contains(&r.doc.intent.as_str()),
            "{}: intent {:?}",
            r.doc.slug,
            r.doc.intent
        );
        assert!(r.doc.views.len() <= MAX_VIEWS, "{}: views cap", r.doc.slug);
        assert!(
            !r.doc.views.is_empty(),
            "{}: a recipe with no view is unreadable",
            r.doc.slug
        );
        assert_eq!(r.home, Home::Builtin);
        assert_eq!(r.trust, TrustState::Trusted);
    }
}

/// Every intent group the home renders has at least one shipped recipe —
/// an empty section is the dead-surface defect in the recipe home's own
/// shape.
#[test]
fn every_intent_group_has_a_builtin() {
    let all = builtins::all();
    for g in INTENT_GROUPS {
        assert!(
            all.iter().any(|r| r.doc.intent == *g),
            "intent group {g:?} has no shipped recipe"
        );
    }
}

#[test]
fn no_builtin_uses_a_reserved_slug() {
    for r in builtins::all() {
        assert!(
            !routes::RESERVED_SLUGS.contains(&r.doc.slug.as_str()),
            "{} collides with a route segment",
            r.doc.slug
        );
    }
}

// --- the closed op set -----------------------------------------------------

/// D21, stated as a test. The guard is the TYPE — there is no variant of
/// [`Op`] that names an exec lane, a command, or a process — and this
/// walk keeps it true as the set grows: a future `Op::Exec` (or an op
/// whose declared args name a tool/command/shell) fails HERE.
#[test]
fn the_op_set_cannot_reach_an_exec_lane() {
    const FORBIDDEN: &[&str] = &[
        "exec", "run_tool", "shell", "cmd", "command", "spawn", "process", "script",
    ];
    for op in Op::ALL {
        let name = op.as_str();
        assert!(
            !FORBIDDEN.iter().any(|f| name.contains(f)),
            "op {name:?} is exec-shaped; D21 forbids a recipe reaching an exec lane"
        );
        for arg in op.spec().args {
            assert!(
                !FORBIDDEN.iter().any(|f| arg.contains(f)),
                "op {name:?} declares arg {arg:?}, which is exec-shaped"
            );
        }
    }
    // The one lane-shaped op reads FACTS somebody else's tool already
    // produced; it can never cause a tool to run, and its lane must be
    // enabled in kb-code.toml regardless (invariant 21(a)).
    assert_eq!(Op::Facts.spec().required_args, &["lane"]);
}

#[test]
fn every_op_declares_its_required_args_as_args() {
    for op in Op::ALL {
        let spec = op.spec();
        for r in spec.required_args {
            assert!(
                spec.args.contains(r),
                "{}: required arg {r:?} is not in its arg list",
                op.as_str()
            );
        }
    }
}

// --- the DAG type-check ----------------------------------------------------

#[test]
fn a_step_fed_the_wrong_address_kind_fails_at_load_naming_the_step() {
    // `usages` accepts SYMBOL addresses; `churn` produces FILE ones.
    let err = doc(r#"
slug = "t:wrong"
title = "Wrong kind"
intent = "orienting"
steps = [
  { id = "hot", op = "churn" },
  { id = "uses", op = "usages", args = { from = "$steps.hot" } },
]
"#)
    .unwrap_err();
    assert_eq!(err.at, "steps.uses", "the failure names the STEP");
    assert!(err.message.contains("file"), "{}", err.message);
    assert!(err.message.contains("symbol"), "{}", err.message);
}

#[test]
fn a_forward_step_reference_fails_at_load() {
    let err = doc(r#"
slug = "t:fwd"
title = "Forward"
intent = "orienting"
steps = [
  { id = "a", op = "outline", args = { from = "$steps.b" } },
  { id = "b", op = "tree" },
]
"#)
    .unwrap_err();
    assert_eq!(err.at, "steps.a");
    assert!(err.message.contains("BEFORE"), "{}", err.message);
}

#[test]
fn set_ops_refuses_inputs_that_disagree_on_kind() {
    let err = doc(
        r#"
slug = "t:mix"
title = "Mixed"
intent = "orienting"
steps = [
  { id = "files", op = "tree" },
  { id = "commits", op = "git_log" },
  { id = "both", op = "set_ops", args = { mode = "union", from = "$steps.files", with = "$steps.commits" } },
]
"#,
    )
    .unwrap_err();
    assert_eq!(err.at, "steps.both");
    assert!(err.message.contains("disagree on kind"), "{}", err.message);
}

#[test]
fn git_log_emit_switches_the_output_kind() {
    // `emit = "files"` makes the commit walk feed a file-shaped op.
    doc(r#"
slug = "t:emit"
title = "Emit files"
intent = "reviewing"
steps = [
  { id = "changed", op = "git_log", args = { emit = "files" } },
  { id = "defs", op = "outline", args = { from = "$steps.changed" } },
]
views = [ { id = "v", kind = "list", step = "defs", columns = [ { field = "symbol" } ] } ]
"#)
    .expect("emit=files produces File addresses");
    // …and the default does not.
    let err = doc(r#"
slug = "t:emit2"
title = "Emit commits"
intent = "reviewing"
steps = [
  { id = "changed", op = "git_log" },
  { id = "defs", op = "outline", args = { from = "$steps.changed" } },
]
"#)
    .unwrap_err();
    assert_eq!(err.at, "steps.defs");
}

#[test]
fn map_transforms_are_type_checked_both_ways() {
    // `symbol-of` needs a line-shaped address; a commit is not one.
    let err = doc(r#"
slug = "t:map"
title = "Map"
intent = "orienting"
steps = [
  { id = "c", op = "git_log" },
  { id = "s", op = "map", args = { from = "$steps.c", to = "symbol-of" } },
]
"#)
    .unwrap_err();
    assert_eq!(err.at, "steps.s");
    assert!(err.message.contains("symbol-of"), "{}", err.message);
    assert!(err.message.contains("commit"), "{}", err.message);
}

/// An arg whose VALUE decides the step's output kind must be a literal —
/// otherwise the DAG could not be type-checked at load at all. An arg
/// whose value does NOT decide the kind (`rails`' `noun`/`orphans`) may
/// be a param, and its vocabulary is checked at run time with a census.
#[test]
fn a_kind_deciding_arg_must_be_a_literal_but_a_vocabulary_arg_may_be_a_param() {
    let err = doc(r#"
slug = "t:kind"
title = "Kind from a param"
intent = "orienting"
params = [ { name = "lane", type = "string", default = "files" } ]
steps = [ { id = "s", op = "search", args = { q = "x", lane = "$p.lane" } } ]
"#)
    .unwrap_err();
    assert_eq!(err.at, "steps.s");
    assert!(err.message.contains("must be a literal"), "{}", err.message);

    // …while the shipped `rails:orphans` takes its lane from an enum param.
    let r = builtins::all();
    let orphans = r
        .iter()
        .find(|r| r.doc.slug == "rails:orphans")
        .expect("shipped");
    assert_eq!(
        orphans.doc.steps[0].args.get("orphans"),
        Some(&ArgVal::Str("$p.lane".into()))
    );
}

#[test]
fn an_unknown_op_arg_fails_at_load_and_lists_the_known_ones() {
    let err = doc(r#"
slug = "t:arg"
title = "Bad arg"
intent = "orienting"
steps = [ { id = "t", op = "tree", args = { nope = 1 } } ]
"#)
    .unwrap_err();
    assert_eq!(err.at, "steps.t");
    assert!(err.message.contains("scope"), "{}", err.message);
}

#[test]
fn a_step_referencing_an_undeclared_param_fails_at_load() {
    let err = doc(r#"
slug = "t:param"
title = "Bad param"
intent = "orienting"
steps = [ { id = "t", op = "tree", args = { limit = "$p.nope" } } ]
"#)
    .unwrap_err();
    assert_eq!(err.at, "steps.t");
    assert!(err.message.contains("$p.nope"), "{}", err.message);
}

#[test]
fn a_view_over_an_undeclared_step_fails_at_load() {
    let err = doc(r#"
slug = "t:view"
title = "Bad view"
intent = "orienting"
steps = [ { id = "t", op = "tree" } ]
views = [ { id = "v", kind = "list", step = "nope", columns = [ { field = "path" } ] } ]
"#)
    .unwrap_err();
    assert_eq!(err.at, "views.v");
}

#[test]
fn an_unknown_intent_group_fails_at_load() {
    let err = doc(r#"
slug = "t:intent"
title = "Bad intent"
intent = "vibes"
steps = [ { id = "t", op = "tree" } ]
"#)
    .unwrap_err();
    assert_eq!(err.at, "intent");
    for g in INTENT_GROUPS {
        assert!(err.message.contains(g), "{}", err.message);
    }
}

#[test]
fn a_document_may_not_claim_a_native_body() {
    let err = load_json(serde_json::json!({
        "slug": "fake",
        "title": "Fake",
        "intent": "orienting",
        "native": "not-a-native",
    }))
    .unwrap_err();
    assert_eq!(err.at, "native");
}

#[test]
fn caps_are_enforced_at_load() {
    let steps: Vec<String> = (0..MAX_STEPS + 1)
        .map(|i| format!("{{ id = \"s{i}\", op = \"tree\" }}"))
        .collect();
    let err = doc(&format!(
        "slug = \"t:cap\"\ntitle = \"Cap\"\nintent = \"orienting\"\nsteps = [{}]\n",
        steps.join(", ")
    ))
    .unwrap_err();
    assert_eq!(err.at, "steps");
    assert!(
        err.message.contains(&MAX_STEPS.to_string()),
        "{}",
        err.message
    );
}

// --- args ------------------------------------------------------------------

#[test]
fn arg_references_parse_into_exactly_three_shapes() {
    assert_eq!(
        parse_arg(&ArgVal::Str("$p.since".into())).unwrap(),
        ArgRef::Param("since".into())
    );
    assert_eq!(
        parse_arg(&ArgVal::Str("$context.path".into())).unwrap(),
        ArgRef::Context("path".into())
    );
    assert_eq!(
        parse_arg(&ArgVal::Str("$steps.a".into())).unwrap(),
        ArgRef::Step("a".into())
    );
    // A plain string is a LITERAL, and a `$p.` whose "name" is a sentence
    // is a MALFORMED reference — never a param called `a and $p.b`. There
    // is no interpolation, because a template would be the beginning of a
    // query language (D21).
    let err = parse_arg(&ArgVal::Str("$p.a and $p.b".into())).unwrap_err();
    assert!(err.contains("no interpolation"), "{err}");
    assert!(matches!(
        parse_arg(&ArgVal::Str("plain".into())).unwrap(),
        ArgRef::Literal(_)
    ));
    assert!(parse_arg(&ArgVal::Str("$nope.x".into())).is_err());
    assert!(
        parse_arg(&ArgVal::Str("$steps.a.path".into())).is_err(),
        "a field projection must go through `map`, which is type-checked"
    );
}

#[test]
fn context_fields_are_closed() {
    for f in CONTEXT_FIELDS {
        assert!(
            parse_arg(&ArgVal::Str(format!("$context.{f}"))).is_ok(),
            "{f}"
        );
    }
    assert!(parse_arg(&ArgVal::Str("$context.secrets".into())).is_err());
}

// --- param validation ------------------------------------------------------

fn param_doc() -> RecipeDoc {
    load_toml(
        r#"
slug = "t:params"
title = "Params"
intent = "orienting"
params = [
  { name = "n", type = "int", default = 5, min = 1, max = 10 },
  { name = "who", type = "enum", values = ["a", "b"], default = "a" },
  { name = "must", type = "string", required = true },
  { name = "p", type = "path", default = "app/models" },
]
steps = [ { id = "t", op = "tree", args = { limit = "$p.n" } } ]
"#,
    )
    .unwrap()
}

fn raw(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn a_missing_required_param_names_the_field() {
    let err = validate_params(&param_doc(), &raw(&[])).unwrap_err();
    assert_eq!(err.field, "must");
    assert!(err.message.contains("required"), "{}", err.message);
}

#[test]
fn an_out_of_range_int_refuses_with_the_bound_and_the_value() {
    let err = validate_params(&param_doc(), &raw(&[("must", "x"), ("n", "99")])).unwrap_err();
    assert_eq!(err.field, "n");
    assert!(err.message.contains("99"), "{}", err.message);
    assert!(err.message.contains("10"), "{}", err.message);
}

#[test]
fn an_enum_refusal_lists_the_values() {
    let err = validate_params(&param_doc(), &raw(&[("must", "x"), ("who", "z")])).unwrap_err();
    assert_eq!(err.field, "who");
    assert!(
        err.message.contains('a') && err.message.contains('b'),
        "{}",
        err.message
    );
}

#[test]
fn an_unknown_param_is_refused_not_ignored() {
    let err = validate_params(&param_doc(), &raw(&[("must", "x"), ("nope", "1")])).unwrap_err();
    assert_eq!(err.field, "nope");
}

#[test]
fn a_path_param_cannot_escape_the_repo() {
    let err =
        validate_params(&param_doc(), &raw(&[("must", "x"), ("p", "../etc/passwd")])).unwrap_err();
    assert_eq!(err.field, "p");
}

#[test]
fn defaults_are_applied_and_typed() {
    let out = validate_params(&param_doc(), &raw(&[("must", "x")])).unwrap();
    assert_eq!(out.get("n"), Some(&ArgVal::Int(5)));
    assert_eq!(out.get("who"), Some(&ArgVal::Str("a".into())));
}

// --- the caps refusal ------------------------------------------------------

#[test]
fn a_limit_over_the_cap_refuses_with_both_numbers() {
    let err = run::parse_limit(Some(MAX_STEP_ROWS + 1)).unwrap_err();
    assert_eq!(err.field, "limit");
    assert!(
        err.message.contains(&(MAX_STEP_ROWS + 1).to_string()),
        "{}",
        err.message
    );
    assert!(
        err.message.contains(&MAX_STEP_ROWS.to_string()),
        "{}",
        err.message
    );
    assert!(run::parse_limit(Some(MAX_STEP_ROWS)).is_ok());
    assert!(run::parse_limit(None).is_ok());
    assert!(run::parse_limit(Some(0)).is_err());
}

// --- trust on first use ----------------------------------------------------

fn repo_file(text: &str, blob: &str) -> RepoFile {
    RepoFile {
        slug: "t".into(),
        path: ".kbc/recipes/t.toml".into(),
        blob: blob.into(),
        text: text.into(),
    }
}

#[test]
fn first_sight_is_untrusted_and_not_runnable() {
    let (state, diff) = trust_for(&repo_file("a = 1\n", "abc"), None);
    assert_eq!(state, TrustState::Untrusted);
    assert!(diff.is_none());
    assert!(!state.runnable());
}

#[test]
fn the_same_content_hash_is_trusted() {
    let (state, diff) = trust_for(&repo_file("a = 1\n", "abc"), Some(("abc", "a = 1\n")));
    assert_eq!(state, TrustState::Trusted);
    assert!(diff.is_none());
    assert!(state.runnable());
}

#[test]
fn a_changed_hash_is_changed_and_surfaces_a_diff() {
    let (state, diff) = trust_for(
        &repo_file("a = 2\nb = 3\n", "def"),
        Some(("abc", "a = 1\nb = 3\n")),
    );
    assert_eq!(state, TrustState::Changed);
    assert!(
        !state.runnable(),
        "a changed recipe must not run on the old decision"
    );
    let d = diff.expect("a changed recipe shows WHAT changed");
    assert!(d.contains("-a = 1"), "{d}");
    assert!(d.contains("+a = 2"), "{d}");
    assert!(d.contains(" b = 3"), "unchanged context is kept: {d}");
}

#[test]
fn the_unified_diff_is_a_real_lcs_not_a_line_by_line_replace() {
    // An INSERTED line must show as one `+`, not as a rewrite of
    // everything below it.
    let d = loader::unified_diff("a\nb\nc\n", "a\nx\nb\nc\n", "f");
    // The prefixes are PARAMETERS, not inline char literals: an inline
    // leading-dash predicate is the shape `tests/security/git_argv_lint.rs`
    // reserves for `git::revspec`, and a lint whose failure mode is
    // "extend the allowlist" is a lint that stops meaning anything.
    let count = |body: &str, header: &str| {
        d.lines()
            .filter(|l| l.starts_with(body) && !l.starts_with(header))
            .count()
    };
    let plus = count("+", "+++");
    let minus = count("-", "---");
    assert_eq!((plus, minus), (1, 0), "{d}");
}

// --- census ----------------------------------------------------------------

/// Exactly one reason means "nothing to worry about". Everything else is
/// a fact about the QUESTION, not about the repo — which is the whole
/// point of the vocabulary being closed.
#[test]
fn exactly_one_empty_reason_is_clean() {
    let clean: Vec<_> = EmptyReason::ALL.iter().filter(|r| r.is_clean()).collect();
    assert_eq!(clean.len(), 1, "{clean:?}");
    assert_eq!(*clean[0], EmptyReason::FilteredOut);
}

#[test]
fn every_empty_reason_renders_a_distinct_explanation() {
    let mut seen: Vec<String> = Vec::new();
    for r in EmptyReason::ALL {
        let c = census::StepCensus::empty(*r);
        let e = c.explain();
        assert!(!e.is_empty(), "{r:?} explains nothing");
        assert!(!seen.contains(&e), "{r:?} duplicates another reason's text");
        seen.push(e);
    }
    // A census with no empty reason explains nothing — a NON-empty step
    // must not carry an explanation for an emptiness it does not have.
    assert!(census::StepCensus::new().explain().is_empty());
}

/// A census reason nothing can ever produce is the dead-surface defect in
/// this module's own shape: a UI would offer a remedy for a state the
/// runner cannot reach. A source scan with a scan's limits (it proves the
/// expression occurs, not that it is reached) — the same trade
/// `git_argv_lint` and `every_declared_filter_key_has_a_consumer` make.
#[test]
fn every_empty_reason_has_a_producer() {
    const OPS_SRC: &str = include_str!("ops.rs");
    for r in EmptyReason::ALL {
        let variant = format!("{r:?}");
        assert!(
            OPS_SRC.contains(&format!("EmptyReason::{variant}")),
            "EmptyReason::{variant} is declared but no op can produce it"
        );
    }
}

#[test]
fn every_empty_reason_round_trips_its_wire_name() {
    for r in EmptyReason::ALL {
        let json = serde_json::to_string(r).unwrap();
        assert_eq!(json, format!("\"{}\"", r.as_str()));
    }
}

// --- addresses + views -----------------------------------------------------

#[test]
fn a_missing_blob_and_a_missing_class_read_as_unknown_never_as_empty() {
    let a = Addr::new(AddrKind::File, "r").with_path("a.rs");
    assert_eq!(a.blob, BLOB_UNKNOWN);
    assert_eq!(a.trust, TRUST_UNKNOWN);
    // …and an explicit empty string does not overwrite them.
    let b = a.clone().with_blob(Some("")).with_trust(Some(""));
    assert_eq!(b.blob, BLOB_UNKNOWN);
    assert_eq!(b.trust, TRUST_UNKNOWN);
    let json = serde_json::to_value(&a).unwrap();
    assert_eq!(json["blob"], "unknown");
    assert_eq!(json["trust"], "unknown");
}

#[test]
fn an_absent_scalar_cell_renders_unknown_never_a_blank() {
    let a = Addr::new(AddrKind::File, "r").with_path("a.rs");
    assert_eq!(
        run::cell(&a, &ColField::Scalar("churn".into())),
        BLOB_UNKNOWN
    );
    assert_eq!(run::cell(&a, &ColField::Path), "a.rs");
}

#[test]
fn the_address_key_is_a_total_order_that_ignores_scalars() {
    let a = Addr::new(AddrKind::File, "r")
        .with_path("a.rs")
        .scalar("x", 1i64);
    let b = Addr::new(AddrKind::File, "r")
        .with_path("a.rs")
        .scalar("x", 99i64);
    assert_eq!(a.key(), b.key(), "two lanes naming one place are one place");
    let c = Addr::new(AddrKind::File, "r").with_path("b.rs");
    assert!(a.key() < c.key());
}

#[test]
fn rendering_an_address_never_produces_a_url() {
    let a = Addr::new(AddrKind::Line, "r")
        .with_path("app/models/order.rb")
        .with_line(12)
        .with_symbol("total");
    let s = run::render_address(&a);
    assert_eq!(s, "app/models/order.rb:12 #total");
    assert!(
        !s.contains("http"),
        "URL composition is the SPA's one builder's job"
    );
}

#[test]
fn spans_for_reports_what_it_could_not_turn_into_a_span() {
    let rows = vec![
        Addr::new(AddrKind::File, "r").with_path("a.rs"),
        Addr::new(AddrKind::Commit, "r").with_commit("deadbeef"),
    ];
    let (spans, skipped) = run::spans_for(&rows);
    assert_eq!(spans.len(), 1);
    assert_eq!(skipped, 1, "an address with no path is REPORTED, not lost");
}

// --- the native adapters ---------------------------------------------------

#[test]
fn every_recipes_1_recipe_has_a_native_adapter() {
    let mut native: Vec<&str> = builtins::NATIVE_SLUGS.to_vec();
    native.sort_unstable();
    let mut legacy: Vec<&str> = crate::recipes::Recipe::ALL
        .iter()
        .map(|r| r.as_str())
        .collect();
    legacy.sort_unstable();
    assert_eq!(
        native, legacy,
        "a recipes/1 recipe with no adapter would be invisible to the new surface"
    );
}

#[test]
fn a_native_adapter_declares_the_columns_the_old_presenters_dropped() {
    let all = builtins::all();
    let npa = all
        .iter()
        .find(|r| r.doc.slug == "new-public-api")
        .expect("adapter exists");
    let fields: Vec<String> = npa.doc.views[0]
        .columns
        .iter()
        .map(|c| c.field.as_label())
        .collect();
    // `first_seen` is the entire point of the recipe and was rendered by
    // neither the SPA table nor the CLI table (the recon's finding 5).
    assert!(fields.contains(&"commit".to_string()), "{fields:?}");
    assert!(
        fields.contains(&"first_seen_unix".to_string()),
        "{fields:?}"
    );
}

#[test]
fn a_legacy_item_becomes_an_address_carrying_every_field_it_had() {
    let item = serde_json::json!({
        "path": "src/a.rs",
        "symbol": "foo",
        "kind": "fn",
        "line": 12,
        "class": "exact",
        "first_seen": { "commit": "abc123", "date_unix": 1700000000i64 },
        "terms": { "fan_in": 3, "fan_out": 1 },
    });
    let a = run::addr_from_legacy_item("r", &item);
    assert_eq!(a.kind, AddrKind::Symbol);
    assert_eq!(a.path.as_deref(), Some("src/a.rs"));
    assert_eq!(a.symbol.as_deref(), Some("foo"));
    assert_eq!(a.line, Some(12));
    assert_eq!(a.trust, "exact");
    assert_eq!(a.commit.as_deref(), Some("abc123"));
    assert_eq!(
        a.scalars.get("first_seen_unix"),
        Some(&ScalarVal::Int(1700000000))
    );
    assert_eq!(a.scalars.get("fan_in"), Some(&ScalarVal::Int(3)));
    assert_eq!(a.scalars.get("kind"), Some(&ScalarVal::Str("fn".into())));
}

// --- the CLI line ----------------------------------------------------------

#[test]
fn the_cli_line_names_every_required_param() {
    for r in builtins::all() {
        let line = routes::cli_line(&r, "acme-app");
        assert!(line.starts_with("kb-code recipe run "), "{line}");
        for p in &r.doc.params {
            if p.required {
                assert!(line.contains(&format!("--p {}=", p.name)), "{line}");
            }
        }
    }
}

// --- context ---------------------------------------------------------------

#[test]
fn the_query_parser_recovers_dotted_keys_and_percent_encoding() {
    let pairs = routes::parse_query("repo=r&p.since=2026-01-01&ctx.path=app%2Fmodels%2Forder.rb");
    assert!(pairs.contains(&("p.since".to_string(), "2026-01-01".to_string())));
    assert!(pairs.contains(&("ctx.path".to_string(), "app/models/order.rb".to_string())));
}

#[test]
fn context_field_lookup_matches_the_declared_vocabulary() {
    let c = Context {
        path: Some("a.rb".into()),
        ..Default::default()
    };
    assert_eq!(c.field("path"), Some("a.rb"));
    assert_eq!(c.field("scope"), None);
    assert_eq!(c.field("nonsense"), None);
}
