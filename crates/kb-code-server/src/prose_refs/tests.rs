//! Tests for `kbc-prose/1` — unit tests over the pure extractor, the
//! golden corpus walk (`grammar/prose_refs.golden.json`), and the
//! per-request resolution ladder against a fixture store (the
//! `symbol_addr::tests` pattern).

use super::*;
use crate::store::Store;

fn kinds(fr: &FieldRefs) -> Vec<(&str, &str)> {
    fr.refs
        .iter()
        .map(|r| (r.kind.as_str(), r.text.as_str()))
        .collect()
}

fn open_store() -> (tempfile::TempDir, Store, i64) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(&tmp.path().join("index.db")).unwrap();
    let repo_id = store.upsert_repo("fixture", "/irrelevant").unwrap();
    (tmp, store, repo_id)
}

#[test]
fn review_only_paths_resolve_at_the_pinned_patchset() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    fn git(root: &std::path::Path, args: &[&str], input: &str) -> String {
        let mut child = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_AUTHOR_NAME", "fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
            .env("GIT_COMMITTER_NAME", "fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "-q"], "");
    // Write immutable objects, not a checkout: feature_x.rs is absent from
    // both the working tree and mirror index, as in the Playwright fixture.
    let blob = git(root, &["hash-object", "-w", "--stdin"], "// feature\n");
    let tree = git(
        root,
        &["mktree"],
        &format!("100644 blob {blob}\tfeature_x.rs\n"),
    );
    let tip = git(root, &["commit-tree", &tree, "-m", "feature"], "");
    let store = Store::open(&root.join("index.db")).unwrap();
    let repo_id = store
        .upsert_repo("fixture", root.to_str().unwrap())
        .unwrap();
    let review_id = store
        .create_review("fixture", None, "main", "feature-x", None, 1)
        .unwrap();
    store.insert_patchset(review_id, 1, &tip, &tip, 1).unwrap();
    let ctx = RefCtx {
        repo_id,
        review_id: Some(review_id),
        ps_number: None,
    };
    let refs = field_refs(&store, &ctx, "The bug is in feature_x.rs:1.").unwrap();
    let resolved = serde_json::to_value(&refs.refs[0]).unwrap();
    assert_eq!(resolved["resolution"]["state"], "exact");
    assert_eq!(resolved["resolution"]["path"], "feature_x.rs");
    assert_eq!(resolved["resolution"]["line"], 1);
    assert_eq!(resolved["resolution"]["ref"], tip);
    assert!(store.get_file(repo_id, "feature_x.rs").unwrap().is_none());
    assert!(!root.join("feature_x.rs").exists());

    // A later snapshot replaces the file with a directory. The mirror still
    // knows the old file: neither fact may produce a live link at the new tip.
    let empty = git(root, &["mktree"], "");
    let directory = git(
        root,
        &["mktree"],
        &format!("040000 tree {empty}\tfeature_x.rs\n"),
    );
    let next_tip = git(root, &["commit-tree", &directory, "-m", "replace file"], "");
    store
        .insert_patchset(review_id, 2, &next_tip, &tip, 2)
        .unwrap();
    store
        .upsert_file(repo_id, "feature_x.rs", &blob, "rust", 11)
        .unwrap();
    let latest = field_refs(&store, &ctx, "feature_x.rs:1 and missing.rs:1").unwrap();
    assert!(latest
        .refs
        .iter()
        .all(|r| r.resolution.as_ref().unwrap().state == STATE_ORPHAN));

    let selected = field_refs(
        &store,
        &RefCtx {
            ps_number: Some(1),
            ..ctx
        },
        "feature_x.rs:1",
    )
    .unwrap();
    let resolution = selected.refs[0].resolution.as_ref().unwrap();
    assert_eq!(resolution.state, crate::resolve::CLASS_EXACT);
    assert_eq!(resolution.r#ref.as_deref(), Some(tip.as_str()));

    // Outside a review the existing mirror-index contract is unchanged.
    let generic = field_refs(
        &store,
        &RefCtx {
            review_id: None,
            ..ctx
        },
        "feature_x.rs:1",
    )
    .unwrap();
    let resolution = generic.refs[0].resolution.as_ref().unwrap();
    assert_eq!(resolution.state, crate::resolve::CLASS_EXACT);
    assert_eq!(resolution.r#ref, None);
}

// --- the golden corpus ------------------------------------------------------

/// ONE fixture pins the closed grammar: ~20 de-identified sentences shaped
/// like a real review run's finding texts (paths with ranges, `Class#method`,
/// `Foo::Bar`, backticked code, `f-slug` mentions, a bare CapWord that must
/// NOT match, a URL that must NOT match). The SPA parses NONE of this — it
/// consumes the server's `refs` — so there is deliberately no TS mirror of
/// this golden (unlike kbcq/1's and kbc-refs/1's two-parser fixtures).
#[test]
fn golden_corpus_matches_the_extractor() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../grammar/prose_refs.golden.json"))
            .expect("the golden fixture parses");
    assert_eq!(fixture["schema"], "kbc-prose-refs-golden/1");
    let cases = fixture["cases"].as_array().expect("cases is an array");
    assert!(
        cases.len() >= 20,
        "the corpus is the review's de-identified shape set — keep it >= 20 cases"
    );
    for case in cases {
        let name = case["name"].as_str().expect("case name");
        let text = case["text"].as_str().expect("case text");
        let got = extract(text);
        let got_refs = serde_json::to_value(&got.refs).unwrap();
        assert_eq!(
            &got_refs, &case["refs"],
            "case {name:?} drifted — if the grammar change is deliberate, \
             regenerate the fixture and review the diff"
        );
        assert_eq!(
            got.truncated,
            case["truncated"].as_bool().unwrap_or(false),
            "case {name:?} truncated flag"
        );
    }
}

/// A helper for reviewing a deliberate grammar change: prints the actual
/// extraction for every golden case as JSON. Not a test gate — run with
/// `cargo test -p kb-code-server prose_refs::tests::print_actual -- --nocapture`.
#[test]
fn print_actual() {
    if std::env::var("KBC_PROSE_GOLDEN_PRINT").is_err() {
        return;
    }
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../grammar/prose_refs.golden.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let text = case["text"].as_str().unwrap();
        let got = extract(text);
        eprintln!(
            "=== {}\n{}",
            case["name"],
            serde_json::to_string_pretty(&got.refs).unwrap()
        );
    }
}

// --- extractor unit tests -----------------------------------------------------

#[test]
fn a_bare_capword_is_never_a_symbol() {
    for t in ["Product", "EUR", "GET", "FIXME", "BundleDescription"] {
        let fr = extract(&format!("The {t} record is loaded twice."));
        assert!(fr.refs.is_empty(), "{t}: {:?}", kinds(&fr));
    }
}

#[test]
fn a_url_is_never_a_path() {
    let fr = extract("Introduced by https://github.com/acme/shopfront/pull/77 last week.");
    assert!(fr.refs.is_empty(), "{:?}", kinds(&fr));
}

#[test]
fn refs_inside_a_fence_are_not_extracted() {
    let fr = extract("Try this:\n\n```ruby\napp/models/order.rb:12\nOrder#total\n```\n\nDone.");
    assert!(fr.refs.is_empty(), "{:?}", kinds(&fr));
    // An UNCLOSED fence swallows the tail the same way — the renderer emits
    // it verbatim, so the extractor must not link inside it either.
    let fr = extract("Try this:\n\n```ruby\napp/models/order.rb:12\nOrder#total");
    assert!(fr.refs.is_empty(), "{:?}", kinds(&fr));
}

#[test]
fn a_ref_inside_a_markdown_link_is_not_double_linked() {
    let fr = extract("[the controller](app/controllers/ops_controller.rb:9) already links it.");
    assert!(fr.refs.is_empty(), "{:?}", kinds(&fr));
    // …and a ref AFTER the link still extracts.
    let fr = extract("[x](app/a.rb:1) then app/models/order.rb:2");
    assert_eq!(kinds(&fr), vec![("path", "app/models/order.rb:2")]);
}

#[test]
fn call_hints_live_only_inside_backticks() {
    let fr = extract("enqueue_order( is called from plain prose.");
    assert!(fr.refs.is_empty(), "{:?}", kinds(&fr));
    let fr = extract("Call `enqueue_order(` without a key.");
    assert_eq!(
        kinds(&fr),
        vec![("code", "enqueue_order("), ("call", "enqueue_order")]
    );
}

#[test]
fn a_call_skips_a_token_the_other_passes_claimed() {
    // `foo.rb` inside backticks is a PATH ref (paths in backticks are the
    // most common review shape and must link) — never ALSO a call: the
    // call scan skips the token the path pass already claimed, so the
    // trailing `(1)` mints no `call` ref.
    let fr = extract("`foo.rb(1)`");
    assert_eq!(kinds(&fr), vec![("code", "foo.rb(1)"), ("path", "foo.rb")]);
}

#[test]
fn truncation_is_honest() {
    let text: String = (0..(MAX_REFS_PER_FIELD + 5))
        .map(|i| format!("app/models/m{i}.rb"))
        .collect::<Vec<_>>()
        .join(" ");
    let fr = extract(&text);
    assert!(fr.truncated);
    assert_eq!(fr.refs.len(), MAX_REFS_PER_FIELD);
}

#[test]
fn spans_are_utf16_not_bytes() {
    // 💥 is 4 bytes and 2 UTF-16 code units; the token starts at byte 5 but
    // UTF-16 offset 3. The only consumer slices JS strings (module doc).
    let fr = extract("💥 app/models/order.rb:4 fails.");
    assert_eq!(fr.refs.len(), 1);
    assert_eq!(fr.refs[0].span, Span { start: 3, end: 24 });
    let text = "💥 app/models/order.rb:4 fails.";
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let sliced: String = String::from_utf16(&utf16[3..24]).unwrap();
    assert_eq!(sliced, "app/models/order.rb:4");
}

#[test]
fn dot_method_and_hash_method_both_parse() {
    let fr = extract("Order.new is invoked twice; Order#total is the other one.");
    assert_eq!(
        kinds(&fr),
        vec![("symbol", "Order.new"), ("symbol", "Order#total")]
    );
    assert_eq!(fr.refs[0].container.as_deref(), Some("Order"));
    assert_eq!(fr.refs[0].member.as_deref(), Some("new"));
}

#[test]
fn line_suffix_pathology_keeps_the_path() {
    for tok in [
        "foo.rb:0",
        "foo.rb:5-3",
        "foo.rb:12345678901",
        "foo.rb:5-99999999999",
    ] {
        let fr = extract(tok);
        assert_eq!(fr.refs.len(), 1, "{tok}");
        assert_eq!(fr.refs[0].path.as_deref(), Some("foo.rb"), "{tok}");
        assert_eq!(fr.refs[0].line_start, None, "{tok}");
        assert_eq!(fr.refs[0].line_end, None, "{tok}");
        assert_eq!(fr.refs[0].lines, None, "{tok}");
    }
}

#[test]
fn extraction_is_deterministic() {
    let t = "See app/a.rb:1-2 and `Billing::InvoiceService#charge` plus f-old-1.";
    assert_eq!(extract(t), extract(t));
}

// --- resolution ---------------------------------------------------------------

fn fixture_store_with_order_rb() -> (tempfile::TempDir, Store, i64) {
    let (tmp, store, repo_id) = open_store();
    let src = "class Order\n  def total\n    1\n  end\nend\n";
    let blob = crate::ingest::git_blob_hash(src.as_bytes());
    store
        .upsert_file(
            repo_id,
            "app/models/order.rb",
            &blob,
            "ruby",
            src.len() as u64,
        )
        .unwrap();
    store
        .replace_symbols(
            &blob,
            crate::lang::RUBY.symbol_salt,
            &crate::extract::extract_symbols("ruby", src.as_bytes()).unwrap(),
        )
        .unwrap();
    (tmp, store, repo_id)
}

#[test]
fn a_path_resolves_exact_when_the_mirror_has_it_and_orphan_otherwise() {
    let (_tmp, store, repo_id) = fixture_store_with_order_rb();
    let ctx = RefCtx {
        repo_id,
        review_id: None,
        ps_number: None,
    };
    let fr = field_refs(&store, &ctx, "app/models/order.rb:2 is the line.").unwrap();
    let res = fr.refs[0].resolution.as_ref().unwrap();
    assert_eq!(res.state, crate::resolve::CLASS_EXACT);
    assert_eq!(res.path.as_deref(), Some("app/models/order.rb"));
    // The hinted line is ECHOED, never guessed or clamped here.
    assert_eq!(res.line, Some(2));

    let fr = field_refs(&store, &ctx, "app/models/ghost.rb:2 is gone.").unwrap();
    let res = fr.refs[0].resolution.as_ref().unwrap();
    assert_eq!(res.state, STATE_ORPHAN);
    assert!(res.caption.is_some());
    assert!(res.path.is_none(), "an orphan carries no landing target");
}

#[test]
fn a_method_resolves_through_the_symbols_ladder() {
    let (_tmp, store, repo_id) = fixture_store_with_order_rb();
    let ctx = RefCtx {
        repo_id,
        review_id: None,
        ps_number: None,
    };
    let fr = field_refs(&store, &ctx, "Order#total races with the webhook.").unwrap();
    let res = fr.refs[0].resolution.as_ref().unwrap();
    assert_eq!(res.state, crate::resolve::CLASS_EXACT, "{res:?}");
    assert_eq!(res.path.as_deref(), Some("app/models/order.rb"));
    assert_eq!(res.line, Some(2));

    let fr = field_refs(&store, &ctx, "Order#missing does not exist.").unwrap();
    assert_eq!(fr.refs[0].resolution.as_ref().unwrap().state, STATE_ORPHAN);
}

#[test]
fn a_const_resolves_through_the_entity_index() {
    let (tmp, store, repo_id) = open_store();
    let src = "module Billing\n  class InvoiceService\n  end\nend\n";
    let blob = crate::ingest::git_blob_hash(src.as_bytes());
    store
        .upsert_file(
            repo_id,
            "app/services/billing/invoice_service.rb",
            &blob,
            "ruby",
            src.len() as u64,
        )
        .unwrap();
    store
        .replace_entity_defs(
            repo_id,
            "(main)",
            "app/services/billing/invoice_service.rb",
            &blob,
            crate::entities::zeitwerk::STATE_READ,
            &[crate::entities::EntityDefClaim {
                fqn: "Billing::InvoiceService".to_string(),
                kind: "class".to_string(),
                nesting: crate::entities::NESTING_LEXICAL,
                line_start: 2,
                line_end: 3,
                zeitwerk_fqn: Some("Billing::InvoiceService".to_string()),
            }],
        )
        .unwrap();
    let _ = tmp;
    let ctx = RefCtx {
        repo_id,
        review_id: None,
        ps_number: None,
    };
    let fr = field_refs(&store, &ctx, "Billing::InvoiceService double-charges.").unwrap();
    let res = fr.refs[0].resolution.as_ref().unwrap();
    // Nesting-proved, live blob: the ONE shape `entities::class_for` mints `exact` for.
    assert_eq!(res.state, crate::resolve::CLASS_EXACT, "{res:?}");
    assert_eq!(res.ent.as_deref(), Some("Billing::InvoiceService"));
    assert_eq!(res.line, Some(2));

    let fr = field_refs(&store, &ctx, "Billing::NoSuchThing is made up.").unwrap();
    assert_eq!(fr.refs[0].resolution.as_ref().unwrap().state, STATE_ORPHAN);
}

#[test]
fn a_finding_resolves_only_against_a_review_in_context() {
    let (_tmp, store, repo_id) = open_store();
    let ctx = RefCtx {
        repo_id,
        review_id: None,
        ps_number: None,
    };
    let fr = field_refs(&store, &ctx, "see f-double-charge for the earlier report.").unwrap();
    let res = fr.refs[0].resolution.as_ref().unwrap();
    assert_eq!(res.state, STATE_ORPHAN);
    assert!(res.caption.as_deref().unwrap().contains("no review"));

    let ctx = RefCtx {
        repo_id,
        review_id: Some(1),
        ps_number: None,
    };
    let fr = field_refs(&store, &ctx, "see f-double-charge for the earlier report.").unwrap();
    let res = fr.refs[0].resolution.as_ref().unwrap();
    assert_eq!(res.state, STATE_ORPHAN);
    assert!(res
        .caption
        .as_deref()
        .unwrap()
        .contains("review 1 has no finding f-double-charge"));
}

#[test]
fn a_call_is_likely_at_best_and_never_exact() {
    let (_tmp, store, repo_id) = open_store();
    let src = "fn enqueue_order() -> i32 {\n    1\n}\n";
    let blob = crate::ingest::git_blob_hash(src.as_bytes());
    store
        .upsert_file(repo_id, "a.rs", &blob, "rust", src.len() as u64)
        .unwrap();
    store
        .replace_symbols(
            &blob,
            crate::lang::RUST.symbol_salt,
            &crate::extract::extract_symbols("rust", src.as_bytes()).unwrap(),
        )
        .unwrap();
    let ctx = RefCtx {
        repo_id,
        review_id: None,
        ps_number: None,
    };
    let fr = field_refs(&store, &ctx, "Call `enqueue_order(` without a key.").unwrap();
    let call = fr.refs.iter().find(|r| r.kind == "call").unwrap();
    let res = call.resolution.as_ref().unwrap();
    assert_eq!(res.state, crate::resolve::CLASS_LIKELY, "{res:?}");
    assert_eq!(res.path.as_deref(), Some("a.rs"));

    // No candidate at all: NO resolution object, plain code rendering.
    let fr = field_refs(&store, &ctx, "Call `totally_bogus_fn(` nowhere.").unwrap();
    let call = fr.refs.iter().find(|r| r.kind == "call").unwrap();
    assert!(call.resolution.is_none());
}

#[test]
fn code_hints_are_never_resolved() {
    let (_tmp, store, repo_id) = open_store();
    let ctx = RefCtx {
        repo_id,
        review_id: None,
        ps_number: None,
    };
    let fr = field_refs(&store, &ctx, "`status: 'in_queue'` is never set.").unwrap();
    let code = fr.refs.iter().find(|r| r.kind == "code").unwrap();
    assert!(code.resolution.is_none());
}

const ROUTER_SRC: &str = include_str!("../router.rs");

#[test]
fn every_declared_v76_b3_route_is_registered_and_requires_its_params() {
    assert!(!V76_B3_ROUTES.is_empty());
    for c in V76_B3_ROUTES {
        let nested = c
            .path
            .strip_prefix("/api")
            .expect("every route path is /api-nested");
        assert!(
            ROUTER_SRC.contains(&format!("\"{nested}\"")),
            "{}: declared but never registered in router.rs — the v7.0 dead-surface defect",
            c.path
        );
        assert!(
            ROUTER_SRC.contains(c.handler),
            "{}: registered path but no {} handler named in router.rs",
            c.path,
            c.handler
        );
        assert!(
            (c.params_accept_without)(""),
            "{}: its own params struct rejects a COMPLETE body",
            c.path
        );
        for p in c.required_params {
            assert!(
                !(c.params_accept_without)(p),
                "{}: declares {p:?} required, but its params struct accepts a request without it",
                c.path
            );
        }
    }
}
