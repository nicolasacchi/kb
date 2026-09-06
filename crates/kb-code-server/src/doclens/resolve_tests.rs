//! DCB W1.C — unit tests for the resolution engine. Kept in their own file
//! (`#[path]`-included from `resolve.rs`) because `resolve.rs` is already the
//! largest module in `doclens/` and the golden table below is long.

use super::*;
use crate::extract::Symbol;

// --- fixtures --------------------------------------------------------------

fn row(path: &str) -> FileRow {
    FileRow {
        path: path.to_string(),
        blob_hash: "0".repeat(40),
        lang: "ruby".to_string(),
        size: 512,
    }
}

/// The `alpha` fixture repo's file list (§12.1) — `algolia.rb` is 6-way
/// ambiguous and `upsell.html.erb` 3-way, exactly like the real checkout.
fn alpha_paths() -> Vec<&'static str> {
    let mut v = vec![
        "app/controllers/carts_controller.rb",
        "app/controllers/carts_controller_spec.rb",
        "app/javascript/website/algolia.js",
        "app/models/concerns/bundles/algolia.rb",
        "app/models/concerns/categories/algolia.rb",
        "app/models/concerns/mapped_brands/algolia.rb",
        "app/models/concerns/products/algolia.rb",
        "app/services/algolia/legacy_recommend_service.rb",
        "app/services/algolia/recommend_service.rb",
        "app/services/algolia/search_service.rb",
        "app/services/algolia_events_dispatcher/conversion.rb",
        "app/services/pub_sub/event_handlers/algolia.rb",
        "app/views/carts/upsell.html.erb",
        "app/views/checkout/upsell.html.erb",
        "app/views/products/upsell.html.erb",
        "spec/support/algolia.rb",
    ];
    v.sort_unstable();
    v
}

fn alpha() -> RepoSnapshot {
    RepoSnapshot::from_rows(
        1,
        "alpha",
        Path::new("/tmp/alpha"),
        alpha_paths().into_iter().map(row).collect(),
    )
}

fn r(kind: &str, path_hint: Option<&str>) -> CodeRefRow {
    CodeRefRow {
        kind: kind.to_string(),
        raw: path_hint.unwrap_or_default().to_string(),
        path_hint: path_hint.map(str::to_string),
        ..CodeRefRow::default()
    }
}

// --- path resolution -------------------------------------------------------

#[test]
fn suffix_match_prefers_exact_path_over_a_suffix_candidate() {
    let snap = alpha();
    let out = resolve_path(
        &snap,
        &r("path", Some("app/services/algolia/search_service.rb")),
        "alpha",
    );
    assert_eq!(out.state, Some(PathState::Present));
    assert_eq!(
        out.resolved.as_deref(),
        Some("app/services/algolia/search_service.rb")
    );
    assert_eq!(out.candidate_count, 1);
    assert!(out.search.is_none());
}

#[test]
fn suffix_match_requires_a_slash_boundary_so_spec_files_never_collide() {
    let snap = alpha();
    let out = resolve_path(&snap, &r("path", Some("carts_controller.rb")), "alpha");
    assert_eq!(out.state, Some(PathState::Present));
    assert_eq!(
        out.resolved.as_deref(),
        Some("app/controllers/carts_controller.rb"),
        "carts_controller.rb must NOT also match carts_controller_spec.rb"
    );
    assert_eq!(out.candidate_count, 1);
}

#[test]
fn basename_with_six_candidates_is_ambiguous_and_omits_the_candidate_list() {
    let snap = alpha();
    let out = resolve_path(&snap, &r("path", Some("algolia.rb")), "alpha");
    assert_eq!(out.state, Some(PathState::Ambiguous));
    assert_eq!(out.candidate_count, 6, "the count stays EXACT");
    assert!(
        out.candidates.is_empty(),
        "over the inline tier the list drops"
    );
    assert_eq!(
        out.search,
        Some(SearchLink {
            q: "algolia.rb".into(),
            repo: "alpha".into()
        })
    );
    assert!(out.resolved.is_none());
}

#[test]
fn basename_with_three_candidates_lists_them_inline() {
    let snap = alpha();
    let out = resolve_path(&snap, &r("path", Some("upsell.html.erb")), "alpha");
    assert_eq!(out.state, Some(PathState::Ambiguous));
    assert_eq!(out.candidate_count, 3);
    assert_eq!(
        out.candidates,
        vec![
            "app/views/carts/upsell.html.erb".to_string(),
            "app/views/checkout/upsell.html.erb".to_string(),
            "app/views/products/upsell.html.erb".to_string(),
        ],
        "candidates are path-ascending — deterministic, never ranked"
    );
    assert!(
        out.search.is_some(),
        "the search link is offered either way"
    );
}

#[test]
fn multi_segment_tail_disambiguates_a_six_way_basename() {
    let snap = alpha();
    let out = resolve_path(&snap, &r("path", Some("products/algolia.rb")), "alpha");
    assert_eq!(out.state, Some(PathState::Present));
    assert_eq!(
        out.resolved.as_deref(),
        Some("app/models/concerns/products/algolia.rb")
    );
    assert_eq!(out.candidate_count, 1);
}

#[test]
fn unknown_path_is_absent_and_carries_a_search_link() {
    let snap = alpha();
    let out = resolve_path(&snap, &r("path", Some("missing_service.rb")), "alpha");
    assert_eq!(out.state, Some(PathState::Absent));
    assert_eq!(out.candidate_count, 0);
    assert_eq!(
        out.search.map(|s| s.q).as_deref(),
        Some("missing_service.rb")
    );
}

#[test]
fn external_kind_is_never_resolved_against_the_repo() {
    let snap = alpha();
    let mut ref_row = r("external", Some("algolia-3.12.2/lib/algolia/api_client.rb"));
    ref_row.line_start = Some(120);
    let out = resolve_path(&snap, &ref_row, "alpha");
    assert_eq!(out.state, Some(PathState::External));
    assert!(out.resolved.is_none());
    assert_eq!(out.candidate_count, 0);
    assert!(out.candidates.is_empty());
    assert!(
        out.search.is_none(),
        "no deep link INTO the repo for a gem path"
    );
}

#[test]
fn glob_and_traversal_paths_are_rejected_as_unusable() {
    let snap = alpha();
    for bad in [
        "carts/precart_popup/*.erb",
        "../../etc/passwd",
        "app/../../secrets.rb",
        "a?b.rb",
        "we[i]rd.rb",
    ] {
        let out = resolve_path(&snap, &r("path", Some(bad)), "alpha");
        assert_eq!(out.state, Some(PathState::Absent), "{bad}");
        assert_eq!(out.note.as_deref(), Some("unusable path"), "{bad}");
        assert!(
            out.search.is_none(),
            "{bad}: never a deep link for a refused path"
        );
    }
    assert_eq!(
        normalize_path_hint("./app/a.rb").as_deref(),
        Some("app/a.rb")
    );
    assert_eq!(
        normalize_path_hint("/app//a.rb").as_deref(),
        Some("app/a.rb")
    );
}

/// Row 19 of the golden table (R11). Its whole point is the NEGATIVE
/// assertion: `acme/shopfront` must never reach the path normaliser, or the
/// ref renders `absent` with a `/search?q=shopfront` link INTO the code repo
/// — a confident lie about a doc that only cited a ticket.
#[test]
fn issue_kind_is_never_resolved_against_the_repo_and_carries_its_github_href() {
    let snap = alpha();
    let mut issue = r("issue", Some("acme/shopfront"));
    issue.line_start = Some(15357);
    issue.raw = "https://github.com/acme/shopfront/issues/15357#issuecomment-52".into();
    let out = resolve_path(&snap, &issue, "alpha");
    assert_eq!(out.state, None, "path_state is null for an issue");
    assert!(out.resolved.is_none());
    assert_eq!(out.candidate_count, 0);
    assert!(out.candidates.is_empty());
    assert!(
        out.search.is_none(),
        "never a search link into the code repo"
    );
    assert_eq!(
        out.issue,
        Some(IssueRef {
            owner: "acme".into(),
            repo: "shopfront".into(),
            number: 15357,
            // REBUILT from the parts — never taken from `raw`, which carries
            // whichever href won the producer's dedup.
            href: "https://github.com/acme/shopfront/issues/15357".into(),
        })
    );

    // The line predicate must see "issue", not line 15357.
    let mut memo = HashMap::new();
    let (line, spans) = line_half_no_rev(&snap, &mut memo, &issue, &out, &[]);
    assert_eq!(line.state, LineState::Absent);
    assert_eq!(line.reason, Some("issue"));
    assert!(spans.is_empty());
    assert!(memo.is_empty(), "an issue ref must never read a file");
}

#[test]
fn a_malformed_issue_ref_is_refused_not_path_resolved() {
    let snap = alpha();
    for (hint, number) in [
        (Some("acme"), Some(1u32)),
        (Some("acme/"), Some(1)),
        (Some("/shopfront"), Some(1)),
        (Some("acme/shopfront"), None),
        (None, Some(1)),
    ] {
        let mut issue = r("issue", hint);
        issue.line_start = number;
        let out = resolve_path(&snap, &issue, "alpha");
        assert_eq!(out.state, None);
        assert!(out.issue.is_none(), "{hint:?}/{number:?}");
        assert_eq!(out.note.as_deref(), Some("unusable issue ref"));
        assert!(out.search.is_none());
    }
}

#[test]
fn declared_but_absent_sets_the_note() {
    let snap = alpha();
    let mut declared = r("path", Some("app/controllers/nope.rb"));
    declared.declared = true;
    let out = resolve_path(&snap, &declared, "alpha");
    assert_eq!(out.state, Some(PathState::Absent));
    assert_eq!(out.note.as_deref(), Some("declared but absent"));

    // A declared ref that DOES resolve carries no note.
    let mut ok = r("path", Some("app/controllers/carts_controller.rb"));
    ok.declared = true;
    assert!(resolve_path(&snap, &ok, "alpha").note.is_none());
}

#[test]
fn path_state_is_null_for_a_ref_with_no_path_hint() {
    let snap = alpha();
    let mut sym = r("symbol_method", None);
    sym.symbol_container = Some("Algolia::SearchService".into());
    sym.symbol_member = Some("listable_results".into());
    let out = resolve_path(&snap, &sym, "alpha");
    assert_eq!(out.state, None, "D5 — null, not a fifth enum value");
    assert!(out.search.is_none() && out.issue.is_none());
}

#[test]
fn resolution_is_invariant_under_frecency_state() {
    // The whole anti-fuzzy property (amendment 5): doc-lens reads the files
    // TABLE, never `search::files`' nucleo+frecency lane. Perturbing the
    // recency ordering of the rows a fuzzy lane would rank by cannot change
    // a single verdict, because the engine only ever asks "equal, or
    // slash-anchored suffix?" — a cardinality, not a ranking.
    let baseline = alpha();
    let mut shuffled: Vec<FileRow> = alpha_paths().into_iter().map(row).collect();
    shuffled.reverse();
    let perturbed = RepoSnapshot::from_rows(1, "alpha", Path::new("/tmp/alpha"), shuffled);

    for hint in [
        "algolia.rb",
        "upsell.html.erb",
        "products/algolia.rb",
        "carts_controller.rb",
        "missing_service.rb",
    ] {
        let a = resolve_path(&baseline, &r("path", Some(hint)), "alpha");
        let b = resolve_path(&perturbed, &r("path", Some(hint)), "alpha");
        assert_eq!(a, b, "{hint}: resolution must not depend on row order");
    }
}

// --- confirm tokens --------------------------------------------------------

fn with_context(ctx: &str) -> CodeRefRow {
    CodeRefRow {
        kind: "path_line".into(),
        context: Some(ctx.to_string()),
        line_start: Some(10),
        ..CodeRefRow::default()
    }
}

#[test]
fn confirm_tokens_take_the_producer_vector_verbatim() {
    let mut row = with_context("prose the predicate must ignore");
    row.path_hint = Some("app/controllers/carts_controller.rb".into());
    row.context_tokens = vec![
        "carts_controller".into(),
        "algolia_user_token".into(),
        "_ALGOLIA".into(),
        "CreateOrderService".into(),
    ];
    // Rule 3 demotes the path-derived `carts_controller` to the tail; nothing
    // is dropped and nothing is re-derived.
    assert_eq!(
        confirm_tokens(&row),
        vec![
            "algolia_user_token".to_string(),
            "_ALGOLIA".to_string(),
            "CreateOrderService".to_string(),
            "carts_controller".to_string(),
        ]
    );
}

#[test]
fn confirm_tokens_drops_producer_tokens_under_the_length_floor() {
    let mut row = with_context("");
    row.context_tokens = vec!["ab".into(), "abc".into(), "save!".into(), "abcd".into()];
    // `save!` survives: the floor is applied AFTER stripping a trailing !/?.
    assert_eq!(
        confirm_tokens(&row),
        vec!["save!".to_string(), "abcd".to_string()]
    );
}

#[test]
fn confirm_tokens_applies_the_length_floor_and_the_stoplist() {
    // No `context_tokens` ⇒ the prose-scan FALLBACK, the only place the
    // stoplist is ever consulted.
    let row = with_context("il params della response con RecommendService e un id");
    let toks = confirm_tokens(&row);
    assert!(toks.contains(&"RecommendService".to_string()));
    for dropped in ["params", "response", "della", "id", "un", "il", "con"] {
        assert!(
            !toks.contains(&dropped.to_string()),
            "{dropped} must be dropped"
        );
    }
}

#[test]
fn confirm_tokens_drops_path_derived_tokens() {
    // "Demote, never drop" — a path stem encodes WHERE, not WHAT.
    let mut row = with_context("il dispatch di conversion usa dispatch_conversion");
    row.path_hint = Some("app/services/algolia_events_dispatcher/conversion.rb".into());
    let toks = confirm_tokens(&row);
    let pos_stem = toks.iter().position(|t| t == "conversion");
    let pos_real = toks.iter().position(|t| t == "dispatch_conversion");
    assert!(pos_stem.is_some(), "the stem is kept, just demoted");
    assert!(pos_real.unwrap() < pos_stem.unwrap());
}

#[test]
fn confirm_tokens_orders_by_length_then_offset_then_lexicographic() {
    let row = with_context("aaaa bbbb cccccc dddd");
    assert_eq!(
        confirm_tokens(&row),
        vec![
            "cccccc".to_string(), // longest first
            "aaaa".to_string(),   // then by first-byte offset
            "bbbb".to_string(),
            "dddd".to_string(),
        ]
    );
}

#[test]
fn confirm_tokens_prefers_symbol_member_then_container() {
    let mut row = with_context("un contesto con RecommendationEngine dentro");
    row.symbol_member = Some("listable_results".into());
    row.symbol_container = Some("SearchService".into());
    let toks = confirm_tokens(&row);
    assert_eq!(toks[0], "listable_results");
    assert_eq!(toks[1], "SearchService");
}

#[test]
fn confirm_tokens_never_exceeds_the_cap() {
    let mut row = with_context("");
    row.context_tokens = (0..40).map(|i| format!("token_{i:02}")).collect();
    assert_eq!(confirm_tokens(&row).len(), MAX_CONFIRM_TOKENS);
}

// --- line_state ------------------------------------------------------------

/// The `alpha` fixture's `carts_controller.rb` (§12.1), byte-exact.
const CARTS: &str = r#"# frozen_string_literal: true

class CartsController < ApplicationController
  include SharedControllerConcern

  def checkout
    return unless current_cart

    order = CreateOrderService.new(
      current_cart,
      current_user,
      algolia_user_token:,
      cookies:
    ).call

    redirect_to cart_path unless order
  end

  def add_cart_product(product, quantity)
    response = UpdateCartService.new(cart: current_cart).upsert_product(
      minsan: product.minsan,
      options: {
        algolia_query_id: params[:query_id],
        origin: params[:source]
      }
    )
    render_updated_cart(response:)
  end

  def add_cart_bundle(bundle, quantity)
    response = UpdateCartService.new(cart: current_cart).upsert_bundle(
      code: bundle.code,
      options: {
        algolia_query_id: params[:query_id]
      }
    )
    render_updated_cart(response:)
  end
end
"#;

/// The `alpha` fixture's `search_service.rb` — purpose-built for the
/// ambiguity arm (`filters` occurs on 3, 4, 7, 8, 11, 12).
const SEARCH_SERVICE: &str = r#"module Algolia
  class SearchService
    def listable_results(filters)
      index.search(query, filters:)
    end

    def facet_results(filters)
      index.search(query, filters:)
    end

    def raw_results(filters)
      index.search(query, filters:)
    end
  end
end
"#;

fn toks(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// [`line_half`] for a document that declared NO `kb-code-rev` — which is
/// every W1.C-era golden in this file, and the shape that must stay
/// byte-for-byte unchanged after W2.A. `RevRemap::prepare(_, _, None)` spawns
/// no subprocess (its decision table short-circuits on `no_doc_rev`), so this
/// stays a pure in-memory helper; the remap arm itself is driven by
/// `remap_outcome` above and by `tests/doclens_remap.rs` against a real
/// fixture repo.
fn line_half_no_rev(
    snap: &RepoSnapshot,
    memo: &mut HashMap<String, Result<Arc<FileText>, &'static str>>,
    r: &CodeRefRow,
    path: &PathResolution,
    tokens: &[String],
) -> (LineOutcome, Vec<SpanOutcome>) {
    let mut no_rev = RevRemap::prepare(Path::new("/nonexistent"), "alpha", None);
    let (line, spans, remap) = line_half(snap, memo, r, path, tokens, &mut no_rev);
    assert!(
        remap.is_none(),
        "a doc with no kb-code-rev must report a null per-ref remap"
    );
    (line, spans)
}

#[test]
fn line_state_confirms_a_unique_token_in_the_plus_minus_three_window() {
    let out = line_state(CARTS, 12, &toks(&["algolia_user_token"]), None);
    assert_eq!(out.state, LineState::Confirmed);
    assert_eq!(out.evidence, LineEvidence::ContextToken);
    assert_eq!(out.token.as_deref(), Some("algolia_user_token"));
    assert_eq!(out.token_line, Some(12));
    assert_eq!(out.file_lines, 39);
}

#[test]
fn line_state_keeps_resolved_line_at_the_hint_when_confirmed() {
    // D6 — the ±3 window is a tolerance on the EVIDENCE, not a correction to
    // the citation. `CreateOrderService` sits at 9, three lines above the
    // cited 12; the reader must still land on 12.
    let out = line_state(CARTS, 12, &toks(&["CreateOrderService"]), None);
    assert_eq!(out.state, LineState::Confirmed);
    assert_eq!(out.token_line, Some(9));
    assert_eq!(
        out.resolved_line,
        Some(12),
        "confirmed never MOVES the reader"
    );
    assert_eq!(out.line_hint_delta, Some(0));
}

#[test]
fn line_state_prefers_the_decisive_token_closest_to_the_hint() {
    // Both are decisive inside ±3 of line 12; `algolia_user_token` (Δ0) must
    // beat `CreateOrderService` (Δ3).
    let out = line_state(
        CARTS,
        12,
        &toks(&["CreateOrderService", "algolia_user_token"]),
        None,
    );
    assert_eq!(out.token.as_deref(), Some("algolia_user_token"));
    assert_eq!(out.token_line, Some(12));
}

#[test]
fn line_state_drifts_on_a_unique_hit_in_the_drift_window_with_a_signed_delta() {
    // Golden #4: the doc says 19, `add_cart_bundle` is really at 30.
    let out = line_state(CARTS, 19, &toks(&["add_cart_bundle"]), None);
    assert_eq!(out.state, LineState::Drifted);
    assert_eq!(out.evidence, LineEvidence::ContextToken);
    assert_eq!(out.token_line, Some(30));
    assert_eq!(out.resolved_line, Some(30), "drift DOES move the reader");
    assert_eq!(out.line_hint_delta, Some(11));

    // and the sign is honest in the other direction too
    let back = line_state(CARTS, 30, &toks(&["add_cart_product"]), None);
    assert_eq!(back.state, LineState::Drifted);
    assert_eq!(back.line_hint_delta, Some(-11));
}

#[test]
fn line_state_uniqueness_is_window_scoped_not_file_scoped_across_a_path_list() {
    // `algolia_query_id: params[:query_id]` appears at BOTH 23 and 34. Each
    // is unique inside its own ±3 window, so both confirm; a file-scoped
    // uniqueness rule would render both unverifiable.
    for hint in [23u32, 34] {
        let out = line_state(CARTS, hint, &toks(&["algolia_query_id"]), None);
        assert_eq!(out.state, LineState::Confirmed, "line {hint}");
        assert_eq!(out.token_line, Some(hint));
        assert_eq!(out.resolved_line, Some(hint));
    }
}

#[test]
fn line_state_is_unverifiable_when_a_token_hits_twice_in_the_window() {
    // `filters` occurs at 7 AND 8 — ambiguous inside ±3 of 8.
    let out = line_state(SEARCH_SERVICE, 8, &toks(&["filters"]), None);
    assert_eq!(out.state, LineState::Unverifiable);
    assert_eq!(out.reason, Some("ambiguous_in_window"));
    assert!(out.resolved_line.is_none() && out.token.is_none());
}

/// 60 lines; `alpha_tok` on 9 and 11 (TWO hits inside ±3 of 10), `beta_tok`
/// on 30 and 50 (none inside ±3, two inside ±64).
fn precedence_text() -> String {
    (1..=60)
        .map(|n| match n {
            9 | 11 => "  call alpha_tok(x)\n".to_string(),
            30 | 50 => "  call beta_tok(y)\n".to_string(),
            _ => format!("  filler_{n}\n"),
        })
        .collect()
}

#[test]
fn line_state_reason_precedence_prefers_the_tighter_window() {
    let text = precedence_text();
    // Both windows are ambiguous ⇒ the TIGHTER window's reason wins.
    let out = line_state(&text, 10, &toks(&["alpha_tok", "beta_tok"]), None);
    assert_eq!(out.state, LineState::Unverifiable);
    assert_eq!(out.reason, Some("ambiguous_in_window"));

    // A token absent from ±3 but ambiguous inside ±64 reports the
    // drift-window reason instead.
    let out = line_state(&text, 10, &toks(&["beta_tok"]), None);
    assert_eq!(out.state, LineState::Unverifiable);
    assert_eq!(out.reason, Some("ambiguous_in_drift_window"));

    // Nothing anywhere ⇒ the weakest reason of the three.
    let out = line_state(&text, 10, &toks(&["gamma_tok"]), None);
    assert_eq!(out.reason, Some("token_not_found"));
}

#[test]
fn line_state_is_unverifiable_with_no_extractable_tokens() {
    // A bare line number is unverifiable BY DEFINITION — never confirmed.
    let out = line_state(CARTS, 12, &[], None);
    assert_eq!(out.state, LineState::Unverifiable);
    assert_eq!(out.reason, Some("no_token"));
    assert_eq!(out.evidence, LineEvidence::None);
}

#[test]
fn line_state_matching_is_word_bounded_and_case_sensitive() {
    let text = "alpha user_token beta\nBase thing\nbase other\n";
    // `token` must NOT match inside `user_token`.
    assert_eq!(
        line_state(text, 1, &toks(&["token"]), None).state,
        LineState::Unverifiable
    );
    // `Base` must not match `base` (line 3), so `Base` is uniquely at line 2.
    let out = line_state(text, 2, &toks(&["Base"]), None);
    assert_eq!(out.state, LineState::Confirmed);
    assert_eq!(out.token_line, Some(2));
    assert!(contains_word("a user_token b", "user_token"));
    assert!(!contains_word("a user_token b", "token"));
    assert!(contains_word("user_token", "user_token"));
}

#[test]
fn contains_word_does_not_panic_on_a_rejected_multibyte_boundary() {
    // [B1] `foo(word-boundary rejection at a non-ASCII-leading match must not
    // step the cursor mid-codepoint. "aède" ~ needle "ède": the only match is
    // preceded by the word char `a`, so it's rejected — advancing by a raw
    // `+ 1` byte from that rejection lands inside the 2-byte `è` and the next
    // `hay[from..]` slice panics. There's no other occurrence, so the honest
    // answer is `false`, not a panic.
    assert!(!contains_word("aède", "ède"));

    // A rejected multi-byte match followed by a genuine one still resolves
    // correctly once the cursor advances by whole characters: "xède ède" has
    // a first `ède` glued to `x` (rejected) and a second one preceded by a
    // space and at end-of-string (a true word-boundary match).
    assert!(contains_word("xède ède", "ède"));
}

#[test]
fn line_state_reports_file_lines_for_an_out_of_range_hint() {
    // Golden #6: `search_service.rb:99` against a 15-line file.
    let out = line_state(SEARCH_SERVICE, 99, &toks(&["listable_results"]), None);
    assert_eq!(out.state, LineState::Unverifiable);
    assert_eq!(out.reason, Some("token_not_found"));
    assert_eq!(
        out.file_lines, 15,
        "so a client can say 'the file has 15 lines'"
    );
}

// --- W2.A: the rev_remap arm (§4) ------------------------------------------

#[test]
fn line_state_prefers_rev_remap_over_the_context_token() {
    // The token pass alone would answer `confirmed` at the hint (12), because
    // `algolia_user_token` is uniquely inside ±3 of it. The remap says the
    // line is really at 20, and arm 1 runs FIRST.
    let out = line_state(
        CARTS,
        12,
        &toks(&["algolia_user_token"]),
        Some(LineMap::Mapped(20)),
    );
    assert_eq!(out.state, LineState::Confirmed);
    assert_eq!(out.evidence, LineEvidence::RevRemap);
    assert_eq!(out.resolved_line, Some(20));
    assert_eq!(out.line_hint_delta, Some(8));
    // git proved the line's IDENTITY — there is no token argument to report,
    // and no file was read to make one.
    assert!(out.token.is_none() && out.token_line.is_none());
    assert_eq!(out.file_lines, 0, "arm 1 reads no file");
}

#[test]
fn line_state_rev_remap_moves_resolved_line_and_still_says_confirmed() {
    // E5 — the ONE place `confirmed` may move the reader. D6 still governs
    // the token pass, which is what makes `line_evidence` the discriminator.
    let moved = line_state(
        CARTS,
        12,
        &toks(&["algolia_user_token"]),
        Some(LineMap::Mapped(15)),
    );
    assert_eq!(moved.state, LineState::Confirmed);
    assert_eq!(moved.resolved_line, Some(15));
    assert_eq!(moved.line_hint_delta, Some(3));

    let token = line_state(CARTS, 12, &toks(&["algolia_user_token"]), None);
    assert_eq!(token.state, LineState::Confirmed);
    assert_eq!(token.resolved_line, Some(12), "D6 — token never moves it");
    assert_eq!(token.line_hint_delta, Some(0));

    // An IDENTITY remap is still git-verified evidence, just with no move.
    let same = line_state(
        CARTS,
        12,
        &toks(&["algolia_user_token"]),
        Some(LineMap::Mapped(12)),
    );
    assert_eq!(same.evidence, LineEvidence::RevRemap);
    assert_eq!(same.line_hint_delta, Some(0));
}

#[test]
fn line_state_falls_through_to_the_token_pass_on_inside_change() {
    // E4 — the cited line's own content was edited, so no honest mapping
    // exists; the token may still find the moved content, and falling through
    // is strictly better than emitting `unverifiable` outright.
    let out = line_state(
        CARTS,
        19,
        &toks(&["add_cart_bundle"]),
        Some(LineMap::InsideChange),
    );
    assert_eq!(out.state, LineState::Drifted);
    assert_eq!(out.evidence, LineEvidence::ContextToken);
    assert_eq!(out.resolved_line, Some(30));
    // Byte-for-byte the W1.C answer — arm 2 is unchanged.
    assert_eq!(
        out,
        line_state(CARTS, 19, &toks(&["add_cart_bundle"]), None)
    );
}

#[test]
fn line_state_maps_both_endpoints_of_a_range_or_falls_through() {
    // E6 — both endpoints map: the span is honest end-to-end.
    let ok = remap_outcome(4, LineMap::Mapped(7), Some(LineMap::Mapped(9))).unwrap();
    assert_eq!(ok.state, LineState::Confirmed);
    assert_eq!(ok.evidence, LineEvidence::RevRemap);
    assert_eq!(ok.resolved_line, Some(7));
    assert_eq!(ok.resolved_line_end, Some(9));
    assert_eq!(ok.line_hint_delta, Some(3));

    // A HALF-mapped range is a lie about the span ⇒ the whole ref falls
    // through to the token pass.
    assert!(remap_outcome(4, LineMap::Mapped(7), Some(LineMap::InsideChange)).is_none());
    assert!(remap_outcome(4, LineMap::InsideChange, Some(LineMap::Mapped(9))).is_none());
    // …as is a range whose end lands BEFORE its start.
    assert!(remap_outcome(4, LineMap::Mapped(9), Some(LineMap::Mapped(7))).is_none());
    // A degenerate one-line range is fine.
    let flat = remap_outcome(4, LineMap::Mapped(7), Some(LineMap::Mapped(7))).unwrap();
    assert_eq!(flat.resolved_line_end, Some(7));
    // A single-line citation carries no end at all.
    assert!(remap_outcome(4, LineMap::Mapped(7), None)
        .unwrap()
        .resolved_line_end
        .is_none());
}

/// The PRODUCER side of §1a's consumer contract (R18): a consumer renders
/// "✓ moved +N · git-verified" for `confirmed && line_hint_delta != 0`, so a
/// `confirmed` outcome with a non-zero delta must NEVER be mintable by any
/// arm other than `rev_remap`. Without this, that badge could be shown for a
/// ref git never verified.
#[test]
fn a_nonzero_delta_on_a_confirmed_ref_always_carries_rev_remap_evidence() {
    let mut checked = 0;
    // Every token-pass shape the fixtures can produce.
    for (text, hint, tok) in [
        (CARTS, 12u32, "algolia_user_token"),
        (CARTS, 12, "CreateOrderService"),
        (CARTS, 19, "add_cart_bundle"),
        (CARTS, 30, "add_cart_product"),
        (CARTS, 23, "algolia_query_id"),
        (SEARCH_SERVICE, 8, "filters"),
        (SEARCH_SERVICE, 99, "listable_results"),
    ] {
        let out = line_state(text, hint, &toks(&[tok]), None);
        if out.state == LineState::Confirmed {
            assert_eq!(out.evidence, LineEvidence::ContextToken);
            assert_eq!(out.line_hint_delta, Some(0), "D6: {tok}@{hint}");
        }
        checked += 1;
    }
    // …and every remap shape.
    for mapped in [1u32, 12, 13, 40] {
        let out = line_state(
            CARTS,
            12,
            &toks(&["algolia_user_token"]),
            Some(LineMap::Mapped(mapped)),
        );
        assert_eq!(out.state, LineState::Confirmed);
        if out.line_hint_delta != Some(0) {
            assert_eq!(out.evidence, LineEvidence::RevRemap);
        }
        checked += 1;
    }
    assert_eq!(checked, 11);
}

#[test]
fn line_state_refuses_a_file_over_the_read_cap() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(
        root.join("huge.rb"),
        vec![b'x'; (LENS_FILE_READ_CAP + 1) as usize],
    )
    .unwrap();
    std::fs::write(root.join("bin.rb"), [0x00, 0xff, 0xfe, 0x00]).unwrap();
    let mut memo = HashMap::new();
    // `.err()` rather than `unwrap_err()` — `FileText` deliberately carries no
    // `Debug` impl (it holds up to 2 MiB of file text; a failing assertion
    // must not dump a whole source file into the test log).
    assert_eq!(
        file_text(&mut memo, root, "huge.rb", LENS_FILE_READ_CAP + 1).err(),
        Some("file_too_large")
    );
    assert_eq!(
        file_text(&mut memo, root, "bin.rb", 4).err(),
        Some("not_text")
    );
    assert_eq!(
        file_text(&mut memo, root, "nope.rb", 4).err(),
        Some("unreadable")
    );
}

#[test]
fn the_file_read_memo_reads_each_path_at_most_once_per_request() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("a.rb"), "one\ntwo\n").unwrap();
    let mut memo = HashMap::new();
    let first = file_text(&mut memo, root, "a.rb", 8).unwrap();
    // Delete it: a second call that actually hit the filesystem would fail.
    std::fs::remove_file(root.join("a.rb")).unwrap();
    let second = file_text(&mut memo, root, "a.rb", 8).unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(memo.len(), 1);
}

#[test]
fn file_text_line_indexing_is_one_based_and_crlf_tolerant() {
    let ft = FileText::new("a\r\nbb\nccc".to_string());
    assert_eq!(ft.line_count(), 3);
    assert_eq!(ft.line(1), Some("a"));
    assert_eq!(ft.line(2), Some("bb"));
    assert_eq!(ft.line(3), Some("ccc"));
    assert_eq!(ft.line(0), None);
    assert_eq!(ft.line(4), None);
}

// --- spans -----------------------------------------------------------------

#[test]
fn parse_line_spans_handles_singles_ranges_and_malformed_segments() {
    assert_eq!(
        parse_line_spans(Some("30,51-65,113-119"), Some(30), None),
        vec![(30, None), (51, Some(65)), (113, Some(119))]
    );
    assert_eq!(
        parse_line_spans(Some("425, 440"), Some(425), None),
        vec![(425, None), (440, None)]
    );
    // Malformed segments are SKIPPED, not fatal.
    assert_eq!(
        parse_line_spans(Some("12,abc,,7-"), Some(12), None),
        vec![(12, None)]
    );
    // An all-malformed (or absent) list falls back to the mirrored pair.
    assert_eq!(
        parse_line_spans(Some("nope"), Some(9), Some(11)),
        vec![(9, Some(11))]
    );
    assert_eq!(parse_line_spans(None, Some(9), None), vec![(9, None)]);
    assert_eq!(parse_line_spans(None, None, None), Vec::new());
    // Legacy `L42` spelling is tolerated rather than dropped.
    assert_eq!(parse_line_spans(Some("L42"), None, None), vec![(42, None)]);
}

#[test]
fn a_path_list_ref_resolves_each_span_and_rolls_up_to_the_weakest() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("app/controllers")).unwrap();
    std::fs::write(root.join("app/controllers/carts_controller.rb"), CARTS).unwrap();
    let snap = RepoSnapshot::from_rows(
        1,
        "alpha",
        root,
        vec![row("app/controllers/carts_controller.rb")],
    );

    let mut list = r("path_list", Some("carts_controller.rb"));
    list.line_start = Some(23);
    list.line_end = Some(23);
    list.line_spans = Some("23,34".into());
    list.context_tokens = vec!["algolia_query_id".into()];

    let path = resolve_path(&snap, &list, "alpha");
    let tokens = confirm_tokens(&list);
    let mut memo = HashMap::new();
    let (rollup, spans) = line_half_no_rev(&snap, &mut memo, &list, &path, &tokens);

    assert_eq!(spans.len(), 2, "ONE ref, many spans — never N refs");
    assert!(spans.iter().all(|s| s.line_state == LineState::Confirmed));
    assert_eq!(spans[0].resolved_line, Some(23));
    assert_eq!(spans[1].resolved_line, Some(34));
    assert_eq!(rollup.state, LineState::Confirmed);
    assert_eq!(
        rollup.resolved_line,
        Some(23),
        "the FIRST span drives the reader"
    );
    assert_eq!(memo.len(), 1, "a 2-span ref still costs ONE file read");

    // Weakest-wins: make the second span unverifiable and the ref-level
    // state must degrade even though the first span confirmed.
    let mut mixed = list.clone();
    mixed.line_spans = Some("23,99".into());
    let (rollup, spans) = line_half_no_rev(&snap, &mut memo, &mixed, &path, &tokens);
    assert_eq!(spans[0].line_state, LineState::Confirmed);
    assert_eq!(spans[1].line_state, LineState::Unverifiable);
    assert_eq!(rollup.state, LineState::Unverifiable);
    assert_eq!(rollup.resolved_line, Some(23));
}

#[test]
fn spans_are_empty_when_there_is_nothing_to_verify() {
    let snap = alpha();
    let mut memo = HashMap::new();
    // no line hint
    let bare = r("path", Some("carts_controller.rb"));
    let p = resolve_path(&snap, &bare, "alpha");
    let (line, spans) = line_half_no_rev(&snap, &mut memo, &bare, &p, &toks(&["x"]));
    assert_eq!(line.state, LineState::Absent);
    assert_eq!(line.reason, Some("no_line_hint"));
    assert!(spans.is_empty());

    // path not present
    let mut missing = r("path_line", Some("nope.rb"));
    missing.line_start = Some(3);
    let p = resolve_path(&snap, &missing, "alpha");
    let (line, spans) = line_half_no_rev(&snap, &mut memo, &missing, &p, &toks(&["x"]));
    assert_eq!(line.reason, Some("path_not_present"));
    assert!(spans.is_empty());

    // external
    let mut ext = r("external", Some("algolia-3.12.2/lib/a.rb"));
    ext.line_start = Some(120);
    let p = resolve_path(&snap, &ext, "alpha");
    let (line, spans) = line_half_no_rev(&snap, &mut memo, &ext, &p, &toks(&["x"]));
    assert_eq!(line.reason, Some("external"));
    assert!(spans.is_empty());
    assert!(
        memo.is_empty(),
        "none of these arms may touch the filesystem"
    );
}

#[test]
fn a_zero_line_hint_that_reaches_doc_lens_is_an_explicit_reason_not_a_null() {
    // [B2b] kb-core's own producer now refuses `foo.rb:0` at the source
    // (B2a) — but doc-lens must still tolerate a hostile or pre-fix payload
    // that puts one on the wire directly. `line_start = Some(0)` survives
    // the `is_none()` check (it's `Some`), so this exercises
    // `parse_line_spans` actually coming back empty INSIDE `line_half`,
    // downstream of a present path and a successfully-read file.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("app/controllers")).unwrap();
    std::fs::write(root.join("app/controllers/carts_controller.rb"), CARTS).unwrap();
    let snap = RepoSnapshot::from_rows(
        1,
        "alpha",
        root,
        vec![row("app/controllers/carts_controller.rb")],
    );

    let mut zero = r("path_line", Some("carts_controller.rb"));
    zero.line_start = Some(0);

    let path = resolve_path(&snap, &zero, "alpha");
    let tokens = confirm_tokens(&zero);
    let mut memo = HashMap::new();
    let (line, spans) = line_half_no_rev(&snap, &mut memo, &zero, &path, &tokens);

    assert_eq!(line.state, LineState::Unverifiable);
    assert_eq!(
        line.reason,
        Some("invalid_line_hint"),
        "an empty `parse_line_spans` result must say WHY, not read as \
         `no_line_hint` (a ref that never carried a hint at all)"
    );
    assert!(spans.is_empty());
}

// --- symbols ---------------------------------------------------------------

fn sym(name: &str, container: Option<&str>, line: u32) -> Symbol {
    Symbol {
        ordinal: 0,
        name: name.to_string(),
        kind: "method".to_string(),
        line_start: line,
        line_end: line + 2,
        col_start: 4,
        col_end: 10,
        container: container.map(str::to_string),
        signature: None,
        doc: None,
        param_min: None,
        param_max: None,
    }
}

fn by_name(rows: Vec<(&str, Symbol)>) -> HashMap<String, Vec<(String, Symbol)>> {
    let mut m: HashMap<String, Vec<(String, Symbol)>> = HashMap::new();
    for (path, s) in rows {
        m.entry(s.name.clone())
            .or_default()
            .push((path.to_string(), s));
    }
    m
}

#[test]
fn symbol_state_hit_unique_container_matched_and_ambiguous() {
    let index = by_name(vec![
        (
            "app/services/algolia/search_service.rb",
            sym("listable_results", Some("SearchService"), 3),
        ),
        (
            "app/services/algolia/recommend_service.rb",
            sym("recommendations", Some("RecommendService"), 4),
        ),
        (
            "app/services/algolia/legacy_recommend_service.rb",
            sym("recommendations", Some("LegacyRecommendService"), 4),
        ),
    ]);

    // 1 row ⇒ hit_unique
    let mut a = r("symbol_method", None);
    a.symbol_container = Some("Algolia::SearchService".into());
    a.symbol_member = Some("listable_results".into());
    let out = resolve_symbol(&index, &a, None);
    assert_eq!(out.state, SymbolState::HitUnique);
    assert_eq!(out.hit_count, 1);

    // 2 rows, no container ⇒ hit_ambiguous (both hits surfaced)
    let mut b = r("symbol_method", None);
    b.symbol_member = Some("recommendations".into());
    let out = resolve_symbol(&index, &b, None);
    assert_eq!(out.state, SymbolState::HitAmbiguous);
    assert_eq!(out.hit_count, 2);
    assert_eq!(out.hits.len(), 2);

    // 2 rows, container disambiguates ⇒ hit_container_matched (never "exact")
    let mut c = r("symbol_method", None);
    c.symbol_container = Some("Algolia::RecommendService".into());
    c.symbol_member = Some("recommendations".into());
    let out = resolve_symbol(&index, &c, None);
    assert_eq!(out.state, SymbolState::HitContainerMatched);
    assert_eq!(
        out.hit_count, 2,
        "the COUNT stays honest about the ambiguity"
    );
    assert_eq!(out.hits.len(), 1);
    assert_eq!(
        out.hits[0].path,
        "app/services/algolia/recommend_service.rb"
    );

    // unknown name ⇒ no_symbol
    let mut d = r("symbol_method", None);
    d.symbol_member = Some("nope".into());
    assert_eq!(
        resolve_symbol(&index, &d, None).state,
        SymbolState::NoSymbol
    );
}

#[test]
fn symbol_state_filters_candidates_to_a_resolved_path_first() {
    let index = by_name(vec![
        ("a/one.rb", sym("call", Some("One"), 3)),
        ("b/two.rb", sym("call", Some("Two"), 9)),
    ]);
    let mut ref_row = r("symbol_method", None);
    ref_row.symbol_member = Some("call".into());
    assert_eq!(
        resolve_symbol(&index, &ref_row, None).state,
        SymbolState::HitAmbiguous
    );
    let scoped = resolve_symbol(&index, &ref_row, Some("b/two.rb"));
    assert_eq!(scoped.state, SymbolState::HitUnique);
    assert_eq!(scoped.hits[0].line_start, 9);
}

#[test]
fn symbol_lookup_name_uses_the_container_only_for_a_const() {
    let mut c = r("symbol_const", None);
    c.symbol_container = Some("BATCH_SIZE".into());
    assert_eq!(symbol_lookup_name(&c), Some("BATCH_SIZE"));

    let mut m = r("symbol_method", None);
    m.symbol_container = Some("SearchService".into());
    assert_eq!(
        symbol_lookup_name(&m),
        None,
        "a bare container on a method ref is not a symbol to look up"
    );

    let mut both = r("symbol_method", None);
    both.symbol_container = Some("SearchService".into());
    both.symbol_member = Some("listable_results".into());
    assert_eq!(symbol_lookup_name(&both), Some("listable_results"));
}

#[test]
fn symbol_hits_are_capped_but_the_count_is_exact() {
    let index = by_name(
        (0..12)
            .map(|i| {
                let path: &'static str = Box::leak(format!("dir{i}/f.rb").into_boxed_str());
                (path, sym("call", Some("C"), 1))
            })
            .collect(),
    );
    let mut ref_row = r("symbol_method", None);
    ref_row.symbol_member = Some("call".into());
    let out = resolve_symbol(&index, &ref_row, None);
    assert_eq!(out.hit_count, 12);
    assert_eq!(out.hits.len(), MAX_SYMBOL_HITS);
}

#[test]
fn symbols_by_name_surfaces_a_store_failure_rather_than_no_symbol() {
    // [C2] A store that can't answer the symbols query must come back as an
    // `Err`, not an empty map that `resolve_refs_blocking` would then read
    // as "this repo genuinely has none of these symbols" for every ref.
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("index.db")).unwrap();
    let repo_id = store.upsert_repo("r", "/tmp/r").unwrap();
    store.drop_symbols_table_for_test();

    let mut ref_row = r("symbol_method", None);
    ref_row.symbol_member = Some("call".into());
    let err = symbols_by_name(&store, repo_id, std::slice::from_ref(&ref_row))
        .expect_err("a dropped symbols table must surface as an Err, not an empty map");
    assert!(
        matches!(err, crate::store::StoreError::Sqlite(_)),
        "unexpected error variant: {err:?}"
    );
}

// --- CT-F2: "when written" (era-resolved citations) ------------------------

fn ctf2_git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git -C {} {args:?} failed: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn ctf2_head_sha(dir: &Path) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// `a.txt` at rev A is `"one\ntwo\nthree\n"`; at rev B (the working tree)
/// line 2 is REPLACED with unrelated content, so a citation of line 2 falls
/// `InsideChange` for `RevRemap` — the exact contrast `when_written` exists
/// for (it evaluates the hint against rev A's OWN content, no remap
/// involved, so it is unaffected either way).
fn ctf2_fixture() -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    ctf2_git(dir, &["init", "-q", "-b", "main"]);
    ctf2_git(dir, &["config", "user.email", "test@example.com"]);
    ctf2_git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("a.txt"), b"one\ntwo\nthree\n").unwrap();
    ctf2_git(dir, &["add", "-A"]);
    ctf2_git(dir, &["commit", "-q", "-m", "A"]);
    let sha_a = ctf2_head_sha(dir);
    std::fs::write(dir.join("a.txt"), b"one\nchanged\nthree\n").unwrap();
    ctf2_git(dir, &["add", "-A"]);
    ctf2_git(dir, &["commit", "-q", "-m", "B"]);
    (tmp, sha_a)
}

#[test]
fn when_written_confirms_against_the_declared_revs_own_content() {
    let (tmp, sha_a) = ctf2_fixture();
    let mut cache = DeclaredEraCache::new(tmp.path().to_path_buf(), sha_a);
    let mut row = r("path_line", Some("a.txt"));
    row.line_start = Some(2);
    let out = when_written(&mut cache, &row, &toks(&["two"])).expect("a.txt has a path hint");
    assert_eq!(out.path_state, PathState::Present);
    assert_eq!(
        out.line_state,
        LineState::Confirmed,
        "line 2 in rev A's OWN content is literally \"two\""
    );
}

#[test]
fn when_written_is_absent_for_a_path_that_never_existed_at_the_declared_rev() {
    let (tmp, sha_a) = ctf2_fixture();
    let mut cache = DeclaredEraCache::new(tmp.path().to_path_buf(), sha_a);
    let mut row = r("path_line", Some("later.txt"));
    row.line_start = Some(1);
    let out =
        when_written(&mut cache, &row, &toks(&["anything"])).expect("later.txt has a path hint");
    assert_eq!(out.path_state, PathState::Absent);
    assert_eq!(
        out.line_state,
        LineState::Absent,
        "nothing to check a line against on a path absent at the rev"
    );
}

#[test]
fn when_written_is_none_for_issue_external_and_no_path_hint_refs() {
    let (tmp, sha_a) = ctf2_fixture();
    let mut cache = DeclaredEraCache::new(tmp.path().to_path_buf(), sha_a);
    assert!(when_written(&mut cache, &r("issue", Some("acme/shopfront")), &[]).is_none());
    assert!(when_written(
        &mut cache,
        &r("external", Some("algolia-3.12.2/lib/a.rb")),
        &[]
    )
    .is_none());
    assert!(
        when_written(&mut cache, &r("path", None), &[]).is_none(),
        "D5 — no path hint at all"
    );
}

#[test]
fn declared_era_cache_memoises_one_git_call_per_path() {
    let (tmp, sha_a) = ctf2_fixture();
    let mut cache = DeclaredEraCache::new(tmp.path().to_path_buf(), sha_a);
    assert!(cache.path_exists("a.txt"));
    assert!(cache.path_exists("a.txt"));
    assert_eq!(cache.exists.len(), 1, "three calls, ONE cat-file -e");
    assert!(cache.text("a.txt").is_ok());
    assert!(cache.text("a.txt").is_ok());
    assert_eq!(cache.text.len(), 1, "three calls, ONE git show");
}

#[tokio::test]
async fn resolve_lens_refuses_a_kb_missing_from_the_doclens_allowlist() {
    // [G1] Pins that the `[doclens] kbs` gate lives INSIDE `resolve_lens`
    // itself (R8), not only in the wire handler in front of it — calling the
    // fn directly, bypassing HTTP entirely, means a future refactor that
    // hoisted the gate up into the handler would leave the handler's own
    // HTTP-level behaviour unchanged while THIS test goes green-to-red.
    let tmp = tempfile::tempdir().unwrap();
    let paths = kb_core::paths::KbPaths::rooted_at(tmp.path(), "kb-code");
    let cfg = crate::config::KbCodeConfig {
        doclens: crate::config::DoclensSection {
            kbs: vec!["allowed".to_string()],
            ..Default::default()
        },
        ..Default::default()
    };
    let state = crate::build_state_for_test(cfg, paths)
        .await
        .expect("build_state_for_test");

    let err = resolve_lens(&state, "not-allowed", "doc.html", None, false)
        .await
        .expect_err("a kb outside [doclens] kbs must be refused");
    assert_eq!(err.reason(), Some("kb_not_allowlisted"));
}
