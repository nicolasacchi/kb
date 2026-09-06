//! `kb refs [<target>] [--json] [--lint]` — the code references extracted
//! from an artifact's own bytes (DCB `coderef/1`). kb reports HINTS only:
//! it has no working tree and no symbol index, so nothing here says whether
//! a path exists or a line still holds — that is kb-code's `doc-lens`
//! (invariant #2/#4).
//!
//! With no `<target>`, walks the whole corpus feed (`coderef-feed/1`),
//! paging transparently over the server's opaque cursor — the operator
//! never sees one. `--lint` reports refs that were INFERRED rather than
//! declared with `<code data-kb-ref="…">`, with the attribute string you'd
//! paste to declare each (the authoring loop `docs/authoring-artifacts.md`
//! §"Code references" describes).
//!
//! `--by-target <PATH>` (CT-B3) flips to the reverse lookup — every doc
//! citing that exact path — via `?by_target=` on the feed route.
//!
//! `--gallery` (CT-B3) prints a ready `/?kb=<kb>&ids=<csv>` gallery deep-link
//! over whichever doc-id set the mode above resolved (invariant #35's
//! golden-pinned `?ids=` atom) — no CLI verb prints URLs today, so this
//! prints the daemon-relative URL a browser can open directly (the daemon
//! serves the SPA itself, ServeDir-mounted at `/`). Degrades LOUDLY over the
//! 500-id server/SPA cap: a warning to stderr and no URL, never a link the
//! gallery would 400 on.
//!
//! No shared Rust type with the server (the `commands::notes` convention —
//! `backlinks`/`links`): every printer here works over the raw
//! `serde_json::Value` body, so it stays decoupled from
//! `kb_server::routes::coderefs`'s wire structs.

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::http::{
    client_with_timeout_and_bearer, encode_path_segment, resolve_artifact_target,
    resolve_default_kb,
};

const DEFAULT_DAEMON: &str = "http://127.0.0.1:4000";

/// Mirrors the server/SPA `?ids=` hard cap (invariant #35, W2.3a) — kept in
/// sync by name only, same convention as `CLIENT_MAX_LIMIT` below (no shared
/// Rust type with `kb_server::routes::docs`).
const GALLERY_IDS_CAP: usize = 500;

fn base_url(daemon: Option<&str>) -> String {
    daemon
        .unwrap_or(DEFAULT_DAEMON)
        .trim_end_matches('/')
        .to_string()
}

/// The pure half of `--gallery`: `None` when there's nothing to link (an
/// empty id set) OR the set is over the gallery's hard cap (invariant #35)
/// — the `docs_query` server-side gate would 400 a >500-id link, so this
/// deliberately refuses to build one rather than handing back a broken URL.
/// Extracted so the cap boundary is unit-testable without a live daemon
/// (this module's own convention — see `clamp_client_limit`/`step_page`).
fn gallery_url_for(daemon: Option<&str>, kb_name: &str, ids: &[String]) -> Option<String> {
    if ids.is_empty() || ids.len() > GALLERY_IDS_CAP {
        return None;
    }
    let base = base_url(daemon);
    let mut url =
        reqwest::Url::parse(&format!("{base}/")).expect("base_url always yields a valid origin");
    url.query_pairs_mut()
        .append_pair("kb", kb_name)
        .append_pair("ids", &ids.join(","));
    Some(url.to_string())
}

/// `--gallery` (human mode): print the link, or a loud stderr explanation
/// of why there isn't one — empty vs. over-cap are DIFFERENT reasons and
/// must not share one message (an operator over the cap needs to know to
/// narrow the query, not that nothing matched).
fn print_gallery_link(daemon: Option<&str>, kb_name: &str, ids: &[String]) {
    match gallery_url_for(daemon, kb_name, ids) {
        Some(url) => println!("{url}"),
        None if ids.is_empty() => {
            eprintln!("kb refs --gallery: no matching docs — nothing to link")
        }
        None => eprintln!(
            "kb refs --gallery: {} matching docs exceeds the gallery's {GALLERY_IDS_CAP}-id cap \
             (invariant #35) — refusing to print a link the gallery would 400 on; narrow the \
             query (e.g. --by-target a specific path) or use --json for the full id set",
            ids.len()
        ),
    }
}

/// Format a unix timestamp as `YYYY-MM-DD HH:MM` (UTC). Mirrors
/// `commands::versions::fmt_ts`; kept local since that one isn't `pub`.
fn fmt_ts(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// `1082` → `"1,082"`. Only used for the corpus-walk summary header.
fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out.chars().rev().collect()
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    kb: Option<&str>,
    target: Option<&str>,
    by_target: Option<&str>,
    json_out: bool,
    lint: bool,
    limit: u32,
    gallery: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    if let Some(path) = by_target {
        return by_target_lookup(kb, path, json_out, lint, gallery, daemon, bearer).await;
    }
    match target {
        Some(t) => single(kb, t, json_out, lint, gallery, daemon, bearer).await,
        None => corpus(kb, json_out, lint, limit, gallery, daemon, bearer).await,
    }
}

/// `--gallery`: its own output mode (the `--lint` precedent) — emits either
/// the `/?kb=&ids=` URL (human) or a JSON envelope with the resolved id set
/// + url/over-cap fields, and never alongside the full report.
fn gallery_output(
    daemon: Option<&str>,
    kb_name: &str,
    ids: &[String],
    json_out: bool,
) -> Result<()> {
    if json_out {
        let over_cap = ids.len() > GALLERY_IDS_CAP;
        let url = gallery_url_for(daemon, kb_name, ids);
        let out = json!({
            "kb": kb_name,
            "ids": ids,
            "count": ids.len(),
            "cap": GALLERY_IDS_CAP,
            "over_cap": over_cap,
            "url": url,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        print_gallery_link(daemon, kb_name, ids);
    }
    Ok(())
}

async fn single(
    kb: Option<&str>,
    target: &str,
    json_out: bool,
    lint: bool,
    gallery: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let (kb_name, id) = resolve_artifact_target(kb, target, daemon, bearer).await?;
    if gallery {
        return gallery_output(daemon, &kb_name, std::slice::from_ref(&id), json_out);
    }
    let base = base_url(daemon);
    let url = format!(
        "{base}/api/kb/{}/docs/{}/code-refs",
        encode_path_segment(&kb_name),
        encode_path_segment(&id)
    );
    let client = client_with_timeout_and_bearer(10, bearer)?;
    let body: Value = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()?
        .json()
        .await
        .with_context(|| format!("parse JSON from {url}"))?;

    if lint {
        let report = build_lint_report(&kb_name, std::slice::from_ref(&body));
        if json_out {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            print!("{}", render_lint_human(&kb_name, &report));
        }
        return Ok(());
    }

    if json_out {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    print!("{}", render_doc_human(&body, &kb_name));
    Ok(())
}

/// `kb refs --by-target <PATH>` — the reverse lookup: every doc citing this
/// exact path, via `?by_target=` on the feed route (CT-B3). Unlike the
/// corpus walk this is ONE request — the server resolves it completely, no
/// cursor.
async fn by_target_lookup(
    kb: Option<&str>,
    path: &str,
    json_out: bool,
    lint: bool,
    gallery: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let kb_name = resolve_default_kb(kb, daemon, bearer).await?;
    let base = base_url(daemon);
    let url = format!("{base}/api/kb/{}/code-refs", encode_path_segment(&kb_name));
    let client = client_with_timeout_and_bearer(10, bearer)?;
    let body: Value = client
        .get(&url)
        .query(&[("by_target", path)])
        .send()
        .await
        .with_context(|| format!("GET {url}?by_target={path}"))?
        .error_for_status()?
        .json()
        .await
        .with_context(|| format!("parse JSON from {url}?by_target={path}"))?;
    let docs: Vec<Value> = body
        .get("docs")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    if gallery {
        let ids = doc_ids(&docs);
        return gallery_output(daemon, &kb_name, &ids, json_out);
    }

    if lint {
        let report = build_lint_report(&kb_name, &docs);
        if json_out {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            print!("{}", render_lint_human(&kb_name, &report));
        }
        return Ok(());
    }

    if json_out {
        let out =
            json!({"schema": "coderef-feed/1", "kb": kb_name, "by_target": path, "docs": docs});
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    print!("{}", render_by_target_human(&kb_name, path, &docs));
    Ok(())
}

/// Pull every `doc_id` out of a `docs[]` array — the shared id-extraction
/// step behind `--gallery` for the corpus walk and the `--by-target`
/// reverse lookup alike.
fn doc_ids(docs: &[Value]) -> Vec<String> {
    docs.iter()
        .filter_map(|d| d.get("doc_id").and_then(|v| v.as_str()).map(str::to_string))
        .collect()
}

fn render_by_target_human(kb_name: &str, path: &str, docs: &[Value]) -> String {
    if docs.is_empty() {
        return format!("{kb_name} · no doc cites {path}\n");
    }
    let mut out = format!(
        "{kb_name} · {} doc{} cite {path}\n",
        docs.len(),
        if docs.len() == 1 { "" } else { "s" }
    );
    for d in docs {
        let doc_path = d.get("doc_path").and_then(|v| v.as_str()).unwrap_or("");
        let ref_count = d.get("ref_count").and_then(|v| v.as_u64()).unwrap_or(0);
        out.push_str(&format!("  {doc_path}  ({ref_count} refs)\n"));
    }
    out
}

/// The server clamps `?limit=` to `1..=100`
/// (`kb_server::routes::coderefs::FEED_MAX_LIMIT`) — requesting above that
/// silently gets a SMALLER page than asked. That mismatch used to trick the
/// old "did I get fewer rows than I asked for" heuristic into treating a
/// full 100-row page as the corpus walk's last page and truncating it
/// (DCB-W1.B.R). Clamping here keeps the requested and effective page sizes
/// in lockstep. Kept in sync with the server constant by name only — no
/// shared Rust type (this module's own doc comment: no shared wire structs
/// with `kb_server::routes::coderefs`).
const CLIENT_MAX_LIMIT: u32 = 100;

fn clamp_client_limit(limit: u32) -> u32 {
    limit.clamp(1, CLIENT_MAX_LIMIT)
}

/// One page's worth of walk state, extracted from `corpus()`'s HTTP loop so
/// the termination logic is unit-testable without a live daemon (kb-cli has
/// no HTTP-mocking crate; this keeps the logic itself pure instead).
struct PageStep {
    docs: Vec<Value>,
    next_cursor: Option<String>,
    /// Primary signal: the server's own `next_cursor` says there's no more
    /// (`None`). NOT derived from page size vs `limit` (that comparison is
    /// exactly the truncation bug DCB-W1.B.R fixed — the server is free to
    /// return a page smaller than `limit` for reasons that have nothing to
    /// do with corpus exhaustion... but also, symmetrically, must never be
    /// trusted to return a FULL page as proof there's more). A secondary
    /// safety guard fires if `next_cursor` comes back IDENTICAL to the
    /// cursor just sent — a stalled/misbehaving server — so the walk can
    /// never spin forever even though `next_cursor.is_none()` is the only
    /// condition a well-behaved server is expected to hit.
    done: bool,
}

fn step_page(body: &Value, prev_cursor: &Option<String>) -> PageStep {
    let docs: Vec<Value> = body
        .get("docs")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let next_cursor = body
        .get("next_cursor")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let stalled = next_cursor.is_some() && next_cursor == *prev_cursor;
    let done = next_cursor.is_none() || stalled;
    PageStep {
        docs,
        next_cursor,
        done,
    }
}

async fn corpus(
    kb: Option<&str>,
    json_out: bool,
    lint: bool,
    limit: u32,
    gallery: bool,
    daemon: Option<&str>,
    bearer: Option<&str>,
) -> Result<()> {
    let kb_name = resolve_default_kb(kb, daemon, bearer).await?;
    let base = base_url(daemon);
    let client = client_with_timeout_and_bearer(30, bearer)?;
    let limit = clamp_client_limit(limit);

    let mut docs: Vec<Value> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let url = format!("{base}/api/kb/{}/code-refs", encode_path_segment(&kb_name));
        let mut q: Vec<(&str, String)> = vec![("limit", limit.to_string())];
        if let Some(c) = &cursor {
            q.push(("cursor", c.clone()));
        }
        let body: Value = client
            .get(&url)
            .query(&q)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()?
            .json()
            .await
            .with_context(|| format!("parse JSON from {url}"))?;
        let step = step_page(&body, &cursor);
        docs.extend(step.docs);
        cursor = step.next_cursor;
        if step.done {
            break;
        }
    }

    if gallery {
        let ids = doc_ids(&docs);
        return gallery_output(daemon, &kb_name, &ids, json_out);
    }

    if lint {
        let report = build_lint_report(&kb_name, &docs);
        if json_out {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            print!("{}", render_lint_human(&kb_name, &report));
        }
        return Ok(());
    }

    if json_out {
        let out = json!({"schema": "coderef-feed/1", "kb": kb_name, "docs": docs});
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    // "docs never scanned" needs the corpus's total doc count — the feed
    // only enumerates docs with a `code_refs_docs` row, so an unscanned doc
    // (no row at all) never appears in `docs` above and must be inferred by
    // subtraction against `/api/kbs`'s `doc_count`. DCB-W1.B.R: since the
    // substring pre-gate moved inside the hook's `enrich()`, this bucket is
    // now ONLY pre-DCB-index docs (never enriched at all) and memory-session
    // transcripts (`CodeRefHook::interested`'s one remaining exclusion) —
    // NOT prose-only docs, which now get a header row (0 refs) like any
    // other scanned doc.
    let never_scanned = doc_count(&base, &kb_name, bearer)
        .await
        .ok()
        .map(|total| total.saturating_sub(docs.len() as u64));
    print!("{}", render_corpus_human(&kb_name, &docs, never_scanned));
    Ok(())
}

async fn doc_count(base: &str, kb_name: &str, bearer: Option<&str>) -> Result<u64> {
    let url = format!("{base}/api/kbs");
    let client = client_with_timeout_and_bearer(10, bearer)?;
    let body: Value = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()?
        .json()
        .await
        .with_context(|| format!("parse JSON from {url}"))?;
    body.as_array()
        .and_then(|kbs| {
            kbs.iter()
                .find(|k| k.get("name").and_then(|n| n.as_str()) == Some(kb_name))
        })
        .and_then(|k| k.get("doc_count"))
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("kb {kb_name:?} missing from /api/kbs"))
}

// --- human printers (pure functions over the parsed JSON body) -----------

/// `kb refs <doc>` — single doc, human output.
fn render_doc_human(body: &Value, kb_name: &str) -> String {
    let doc_path = body.get("doc_path").and_then(|v| v.as_str()).unwrap_or("");
    let never_scanned = body
        .get("never_scanned")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if never_scanned {
        return format!("{doc_path} · not scanned yet — run 'kb reindex --kb {kb_name}'\n");
    }
    let ref_count = body.get("ref_count").and_then(|v| v.as_u64()).unwrap_or(0);
    if ref_count == 0 {
        return format!("{doc_path} · 0 refs\n");
    }

    let groups = body
        .get("groups")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let refs = body
        .get("refs")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let ungrouped_count = body
        .get("ungrouped_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let extracted_at = body.get("extracted_at").and_then(|v| v.as_i64());
    let code_rev = body.get("code_rev").filter(|v| !v.is_null());

    let mut out = String::new();
    out.push_str(&format!(
        "{doc_path} · {ref_count} refs · {} groups",
        groups.len()
    ));
    if let Some(ts) = extracted_at {
        out.push_str(&format!(" · extracted {}", fmt_ts(ts)));
    }
    if let Some(rev) = code_rev {
        let label = rev.get("label").and_then(|v| v.as_str()).unwrap_or("");
        let sha = rev.get("sha").and_then(|v| v.as_str()).unwrap_or("");
        let dirty = rev.get("dirty").and_then(|v| v.as_bool()).unwrap_or(false);
        out.push_str(&format!(
            " · rev {label}@{sha}{}",
            if dirty { "+dirty" } else { "" }
        ));
    }
    out.push('\n');

    // Duplicate-slug groups: `groups[]` can hold MORE THAN ONE entry sharing
    // the same `key` — the server's `build_groups_and_refs` deliberately
    // keeps a separate entry per (key, label, anchor) triple so a genuine
    // same-key/different-label heading collision doesn't silently drop a
    // label. But `ref.group` is only ever the shared `key` (it's an FK into
    // `groups[]` BY KEY, not a unique row id — see that doc comment), so
    // rendering once per groups[] ENTRY would print that key's whole ref
    // list again for every duplicate. Fix: dedupe the render by key and
    // combine the colliding labels into one heading instead — no ref
    // dropped, no ref duplicated, and the collision is still visible.
    let mut seen_keys: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for g in &groups {
        let key = g.get("key").and_then(|v| v.as_str()).unwrap_or("");
        if !seen_keys.insert(key) {
            continue;
        }
        let labels: Vec<&str> = groups
            .iter()
            .filter(|g2| g2.get("key").and_then(|v| v.as_str()) == Some(key))
            .filter_map(|g2| g2.get("label").and_then(|v| v.as_str()))
            .collect();
        let heading = labels.join(" / ");
        out.push_str(&format!("\n§ {heading}\n"));
        for r in refs
            .iter()
            .filter(|r| r.get("group").and_then(|v| v.as_str()) == Some(key))
        {
            out.push_str(&render_ref_line(r));
        }
    }
    if ungrouped_count > 0 {
        out.push_str("\n(ungrouped)\n");
        for r in refs
            .iter()
            .filter(|r| r.get("group").map(|v| v.is_null()).unwrap_or(true))
        {
            out.push_str(&render_ref_line(r));
        }
    }
    out.push_str("\nkb reports hints only — run `kb-code doclens show` to resolve them against a checkout.\n");
    out
}

fn render_ref_line(r: &Value) -> String {
    let kind = r.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    let raw = r.get("raw").and_then(|v| v.as_str()).unwrap_or("");
    let declared = r.get("declared").and_then(|v| v.as_bool()).unwrap_or(false);
    let mark = if declared { "✓" } else { " " };
    let mut out = format!("  {mark}{kind:<10}{raw}\n");
    if matches!(kind, "path_line" | "path_range" | "path_list") {
        let ctx = r.get("context").and_then(|v| v.as_str()).unwrap_or("");
        if !ctx.is_empty() {
            let truncated: String = ctx.chars().take(100).collect();
            let ellipsis = if ctx.chars().count() > 100 { "…" } else { "" };
            out.push_str(&format!("              ctx  {truncated}{ellipsis}\n"));
        }
    }
    out
}

/// `kb refs --kb <kb>` — whole-corpus walk, human output.
fn render_corpus_human(kb_name: &str, docs: &[Value], never_scanned: Option<u64>) -> String {
    let mut ref_total: u64 = 0;
    let mut by_kind: std::collections::BTreeMap<&'static str, u64> =
        std::collections::BTreeMap::new();
    let mut declared_count: u64 = 0;
    let mut inferred_count: u64 = 0;
    let mut docs_with_refs: u64 = 0;
    let mut docs_with_none: u64 = 0;

    for d in docs {
        let rc = d.get("ref_count").and_then(|v| v.as_u64()).unwrap_or(0);
        if rc > 0 {
            docs_with_refs += 1;
        } else {
            docs_with_none += 1;
        }
        for r in d
            .get("refs")
            .and_then(|v| v.as_array())
            .map(|a| a.as_slice())
            .unwrap_or(&[])
        {
            ref_total += 1;
            let kind = canonical_kind(r.get("kind").and_then(|v| v.as_str()).unwrap_or(""));
            *by_kind.entry(kind).or_insert(0) += 1;
            if r.get("declared").and_then(|v| v.as_bool()).unwrap_or(false) {
                declared_count += 1;
            } else {
                inferred_count += 1;
            }
        }
    }

    let mut out = format!(
        "{kb_name} · {} docs scanned · {} refs",
        thousands(docs.len() as u64),
        thousands(ref_total)
    );
    if let Some(n) = never_scanned {
        out.push_str(&format!(" · {} docs never scanned", thousands(n)));
    }
    out.push_str("\n\n");
    out.push_str(&format!("  {:>4}  refs total\n", thousands(ref_total)));

    let kinds = [
        "path",
        "path_line",
        "path_range",
        "path_list",
        "symbol_method",
        "symbol_const",
        "external",
        "issue",
        "other",
    ];
    for chunk in kinds.chunks(4) {
        let mut line = String::from(" ");
        for &k in chunk {
            let n = by_kind.get(k).copied().unwrap_or(0);
            line.push_str(&format!(" {n:>4}  {k:<15}"));
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    // W1.gate: the SECOND field on each of these two lines butts directly up
    // against a literal word with no space between them in the format
    // string ("declared{}", "docs with refs{}") — the only separation comes
    // from the field's own left-padding. `{:>5}` stopped padding at all once
    // `thousands(n)` exceeded 5 rendered chars (>= 10,000, e.g. "12,194" is
    // 6 chars), gluing the literal word straight onto the number
    // ("declared12,194"). Widened to `{:>7}` so five-digit counts still get
    // at least one separating space; the FIRST field on each line is
    // untouched — it's flanked by literal spaces on both sides already, so
    // it was never at risk of gluing regardless of width.
    out.push_str(&format!(
        "  {:>4}  declared{:>7}  inferred\n",
        thousands(declared_count),
        thousands(inferred_count)
    ));
    out.push_str(&format!(
        "  {:>4}  docs with refs{:>7}  docs with none\n",
        thousands(docs_with_refs),
        thousands(docs_with_none)
    ));
    out
}

/// Every ref kind on `kb_core::coderefs::CodeRefKind::as_str`'s closed set
/// maps to itself; anything else (a future kind the server knows about that
/// this CLI build doesn't) buckets into `"other"` — NEVER `"path"`. Folding
/// an unrecognized kind into `"path"` would silently inflate that count with
/// ref kinds that aren't paths at all; `"other"` keeps the corpus summary
/// honest (forward-compat: an unrecognized kind still counts, visibly, under
/// its own bucket) while still rendering (`render_corpus_human`'s `kinds`
/// list includes `"other"`).
fn canonical_kind(kind: &str) -> &'static str {
    match kind {
        "path" => "path",
        "path_line" => "path_line",
        "path_range" => "path_range",
        "path_list" => "path_list",
        "symbol_method" => "symbol_method",
        "symbol_const" => "symbol_const",
        "external" => "external",
        "issue" => "issue",
        _ => "other",
    }
}

// --- --lint --------------------------------------------------------------

/// Kinds `data-kb-ref` can express — a path, optionally `#Lstart[-Lend]`.
/// Symbol and issue kinds have no path to declare and are excluded from
/// suggestions (their counts still land in `totals`, so the omission is
/// visible rather than silent).
fn lintable(kind: &str) -> bool {
    matches!(
        kind,
        "path" | "path_line" | "path_range" | "path_list" | "external"
    )
}

fn suggested_attr(r: &Value) -> Option<(String, bool)> {
    let kind = r.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    if !lintable(kind) {
        return None;
    }
    let path = r.get("path_hint").and_then(|v| v.as_str())?;
    let start = r.get("line_start").and_then(|v| v.as_i64());
    let end = r.get("line_end").and_then(|v| v.as_i64());
    let first_span_only = kind == "path_list";
    // `path_line` always carries `line_end == line_start` (a single-line
    // span stored redundantly across both columns) — the declared-ref
    // grammar's single-line form (`#Lstart`, no range) is correct there
    // even though `end` is `Some`. `path_range`/`path_list` (first span
    // only) DO need the range form when `end` differs.
    let attr = match kind {
        "path" | "external" => path.to_string(),
        "path_line" => match start {
            Some(s) => format!("{path}#L{s}"),
            None => path.to_string(),
        },
        _ => match (start, end) {
            (Some(s), Some(e)) => format!("{path}#L{s}-L{e}"),
            (Some(s), None) => format!("{path}#L{s}"),
            _ => path.to_string(),
        },
    };
    Some((attr, first_span_only))
}

/// `{"schema":"coderef-lint/1", kb, docs:[{doc_id, doc_path, inferred,
/// declared, suggestions:[{ordinal, raw, attr}]}], totals:{…}}` (R1).
fn build_lint_report(kb_name: &str, docs: &[Value]) -> Value {
    let mut out_docs = Vec::new();
    let mut total_inferred = 0u64;
    let mut total_declared = 0u64;
    let mut total_excluded_symbol = 0u64;
    let mut total_excluded_issue = 0u64;
    let mut docs_with_findings = 0u64;

    for d in docs {
        let doc_id = d.get("doc_id").and_then(|v| v.as_str()).unwrap_or("");
        let doc_path = d.get("doc_path").and_then(|v| v.as_str()).unwrap_or("");
        let mut inferred = 0u64;
        let mut declared = 0u64;
        let mut suggestions = Vec::new();
        for r in d
            .get("refs")
            .and_then(|v| v.as_array())
            .map(|a| a.as_slice())
            .unwrap_or(&[])
        {
            let kind = r.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            let is_declared = r.get("declared").and_then(|v| v.as_bool()).unwrap_or(false);
            if is_declared {
                declared += 1;
                continue;
            }
            match kind {
                "symbol_method" | "symbol_const" => total_excluded_symbol += 1,
                "issue" => total_excluded_issue += 1,
                _ => {}
            }
            inferred += 1;
            if let Some((attr, first_span_only)) = suggested_attr(r) {
                suggestions.push(json!({
                    "ordinal": r.get("ordinal").cloned().unwrap_or(Value::Null),
                    "raw": r.get("raw").cloned().unwrap_or(Value::Null),
                    "attr": attr,
                    "first_span_only": first_span_only,
                }));
            }
        }
        total_inferred += inferred;
        total_declared += declared;
        if inferred > 0 || declared > 0 {
            docs_with_findings += 1;
        }
        out_docs.push(json!({
            "doc_id": doc_id,
            "doc_path": doc_path,
            "inferred": inferred,
            "declared": declared,
            "suggestions": suggestions,
        }));
    }

    json!({
        "schema": "coderef-lint/1",
        "kb": kb_name,
        "docs": out_docs,
        "totals": {
            "inferred": total_inferred,
            "declared": total_declared,
            "docs_with_findings": docs_with_findings,
            "excluded_symbol": total_excluded_symbol,
            "excluded_issue": total_excluded_issue,
        },
    })
}

fn render_lint_human(kb_name: &str, report: &Value) -> String {
    let totals = report.get("totals").cloned().unwrap_or_default();
    let inferred = totals.get("inferred").and_then(|v| v.as_u64()).unwrap_or(0);
    let declared = totals.get("declared").and_then(|v| v.as_u64()).unwrap_or(0);
    let docs_with_findings = totals
        .get("docs_with_findings")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let excluded_symbol = totals
        .get("excluded_symbol")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let excluded_issue = totals
        .get("excluded_issue")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let mut out = format!(
        "{kb_name} · lint: {inferred} inferred refs in {docs_with_findings} docs ({declared} already declared)\n"
    );
    if excluded_symbol > 0 || excluded_issue > 0 {
        out.push_str(&format!(
            "  ({excluded_symbol} symbol / {excluded_issue} issue refs excluded — no path to declare)\n"
        ));
    }

    for d in report
        .get("docs")
        .and_then(|v| v.as_array())
        .map(|a| a.as_slice())
        .unwrap_or(&[])
    {
        let suggestions = d
            .get("suggestions")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if suggestions.is_empty() {
            continue;
        }
        let doc_path = d.get("doc_path").and_then(|v| v.as_str()).unwrap_or("");
        let doc_inferred = d.get("inferred").and_then(|v| v.as_u64()).unwrap_or(0);
        let doc_declared = d.get("declared").and_then(|v| v.as_u64()).unwrap_or(0);
        out.push_str(&format!(
            "\n{doc_path}   ({doc_inferred} inferred, {doc_declared} declared)\n"
        ));
        for s in &suggestions {
            let raw = s.get("raw").and_then(|v| v.as_str()).unwrap_or("");
            let attr = s.get("attr").and_then(|v| v.as_str()).unwrap_or("");
            let note = if s
                .get("first_span_only")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                "   # first span only"
            } else {
                ""
            };
            out.push_str(&format!(
                "    {raw}  →  <code data-kb-ref=\"{attr}\">{raw}</code>{note}\n"
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_client_limit_caps_above_server_max() {
        assert_eq!(clamp_client_limit(500), 100);
        assert_eq!(clamp_client_limit(100), 100);
        assert_eq!(clamp_client_limit(1), 1);
        assert_eq!(clamp_client_limit(0), 1);
    }

    // --- CT-B3: --gallery / --by-target ------------------------------------

    #[test]
    fn gallery_url_for_builds_a_kb_scoped_ids_link() {
        let ids = vec!["a1".to_string(), "b2".to_string()];
        let url = gallery_url_for(None, "canon", &ids).expect("under cap");
        assert_eq!(url, "http://127.0.0.1:4000/?kb=canon&ids=a1%2Cb2");
    }

    #[test]
    fn gallery_url_for_honors_an_explicit_daemon_base() {
        let ids = vec!["a1".to_string()];
        let url = gallery_url_for(Some("http://example.test:9000"), "canon", &ids).unwrap();
        assert_eq!(url, "http://example.test:9000/?kb=canon&ids=a1");
    }

    #[test]
    fn gallery_url_for_is_none_when_ids_are_empty() {
        assert_eq!(gallery_url_for(None, "canon", &[]), None);
    }

    /// Invariant #35's hard 500-id cap — AT the cap still links, one over
    /// refuses (never a truncated/silently-trimmed link).
    #[test]
    fn gallery_url_for_refuses_over_the_500_id_cap() {
        let at_cap: Vec<String> = (0..GALLERY_IDS_CAP).map(|i| format!("id{i}")).collect();
        assert!(gallery_url_for(None, "canon", &at_cap).is_some());

        let over_cap: Vec<String> = (0..=GALLERY_IDS_CAP).map(|i| format!("id{i}")).collect();
        assert_eq!(gallery_url_for(None, "canon", &over_cap), None);
    }

    #[test]
    fn doc_ids_pulls_doc_id_out_of_a_docs_array() {
        let docs = vec![
            json!({"doc_id": "a1", "doc_path": "a.html"}),
            json!({"doc_id": "b2", "doc_path": "b.html"}),
            json!({"doc_path": "c.html"}), // no doc_id — skipped, not a panic
        ];
        assert_eq!(doc_ids(&docs), vec!["a1".to_string(), "b2".to_string()]);
    }

    fn feed_page(docs: Vec<Value>, next_cursor: Option<&str>) -> Value {
        let mut body = json!({"schema": "coderef-feed/1", "kb": "canon", "docs": docs});
        if let Some(c) = next_cursor {
            body["next_cursor"] = json!(c);
        }
        body
    }

    /// DCB-W1.B.R regression: requesting `--limit 500` (clamped to the
    /// server's 100) must NOT stop after a single full page. The pre-fix
    /// heuristic (`page.len() < limit`) compared the returned page against
    /// the CLIENT's original, unclamped 500 and treated a full 100-row page
    /// as "short" — truncating the walk after page 1 even though the server
    /// handed back a valid `next_cursor`. `step_page` must keep going.
    #[test]
    fn limit_above_server_max_still_walks_past_page_one() {
        let page1 = feed_page(vec![json!({"doc_id": "d1"}); 100], Some("1700000000:zzz"));
        let step1 = step_page(&page1, &None);
        assert!(
            !step1.done,
            "a full page with a next_cursor must not end the walk"
        );
        assert_eq!(step1.docs.len(), 100);

        let page2 = feed_page(vec![json!({"doc_id": "d2"}); 20], None);
        let step2 = step_page(&page2, &step1.next_cursor);
        assert!(step2.done, "next_cursor: null must end the walk");
        assert_eq!(step2.docs.len(), 20);

        let mut total = step1.docs;
        total.extend(step2.docs);
        assert_eq!(
            total.len(),
            120,
            "walk must accumulate BOTH pages, proving it continued past page 1"
        );
    }

    #[test]
    fn step_page_stalled_cursor_is_a_safety_stop_not_an_infinite_loop() {
        let prev = Some("1700000000:aaa".to_string());
        // A misbehaving server echoes back the exact cursor it was just
        // sent — the walk must stop rather than re-request forever.
        let body = feed_page(vec![json!({"doc_id": "d1"})], Some("1700000000:aaa"));
        let step = step_page(&body, &prev);
        assert!(step.done, "an unchanged cursor must be treated as done");
    }

    #[test]
    fn step_page_advancing_cursor_continues() {
        let prev = Some("1700000000:aaa".to_string());
        let body = feed_page(vec![json!({"doc_id": "d1"})], Some("1700000000:bbb"));
        let step = step_page(&body, &prev);
        assert!(!step.done);
        assert_eq!(step.next_cursor.as_deref(), Some("1700000000:bbb"));
    }

    fn sample_doc() -> Value {
        json!({
            "schema": "coderef/1",
            "kb": "canon",
            "doc_id": "abc123def456",
            "doc_path": "kitchen-sink.html",
            "title": "Kitchen Sink",
            "doc_hash": "deadbeef0001",
            "extracted_at": 1_754_563_200i64,
            "never_scanned": false,
            "code_rev": {"label": "app", "sha": "abc1234", "dirty": false},
            "ref_count": 3,
            "ungrouped_count": 1,
            "truncated": false,
            "groups": [
                {"ordinal": 0, "key": "kb-h-work-items", "label": "Work items", "anchor": "kb-h-work-items"}
            ],
            "refs": [
                {"ordinal": 0, "group": null, "kind": "path", "raw": "config/importmap.rb",
                 "path_hint": "config/importmap.rb", "line_start": null, "line_end": null,
                 "line_spans": null, "symbol_container": null, "symbol_member": null,
                 "context": "", "context_tokens": [], "declared": false},
                {"ordinal": 1, "group": "kb-h-work-items", "kind": "path_line", "raw": "carts_controller.rb:284",
                 "path_hint": "carts_controller.rb", "line_start": 284, "line_end": 284,
                 "line_spans": null, "symbol_container": null, "symbol_member": null,
                 "context": "some context text", "context_tokens": ["carts_controller"], "declared": false},
                {"ordinal": 2, "group": "kb-h-work-items", "kind": "path", "raw": "order.rb",
                 "path_hint": "order.rb", "line_start": null, "line_end": null,
                 "line_spans": null, "symbol_container": null, "symbol_member": null,
                 "context": "", "context_tokens": [], "declared": true},
            ],
        })
    }

    #[test]
    fn human_output_groups_then_ungrouped_trailer() {
        let out = render_doc_human(&sample_doc(), "canon");
        let work_idx = out.find("§ Work items").unwrap();
        let ungrouped_idx = out.find("(ungrouped)").unwrap();
        assert!(
            work_idx < ungrouped_idx,
            "groups must render before the ungrouped trailer:\n{out}"
        );
        assert!(out
            .trim_end()
            .ends_with("run `kb-code doclens show` to resolve them against a checkout."));
    }

    #[test]
    fn human_output_marks_declared_refs() {
        let out = render_doc_human(&sample_doc(), "canon");
        assert!(
            out.contains("✓path"),
            "declared ref must carry a ✓ marker:\n{out}"
        );
    }

    /// Duplicate-slug groups (same `key`, different `label` — the server's
    /// documented SPLIT case): the render must print each group's refs
    /// exactly ONCE (not once per colliding groups[] entry) under a
    /// combined heading, never dropping either label.
    #[test]
    fn human_output_dedupes_duplicate_key_groups_instead_of_repeating_refs() {
        let body = json!({
            "doc_path": "collide.html",
            "never_scanned": false,
            "ref_count": 2,
            "ungrouped_count": 0,
            "groups": [
                {"ordinal": 0, "key": "kb-h-findings", "label": "Findings", "anchor": "kb-h-findings"},
                {"ordinal": 1, "key": "kb-h-findings", "label": "Findings (again)", "anchor": "kb-h-findings-2"},
            ],
            "refs": [
                {"ordinal": 0, "group": "kb-h-findings", "kind": "path", "raw": "a.rb",
                 "path_hint": "a.rb", "line_start": null, "line_end": null,
                 "context": "", "declared": false},
                {"ordinal": 1, "group": "kb-h-findings", "kind": "path", "raw": "b.rb",
                 "path_hint": "b.rb", "line_start": null, "line_end": null,
                 "context": "", "declared": false},
            ],
        });
        let out = render_doc_human(&body, "canon");
        assert_eq!(
            out.matches("a.rb").count(),
            1,
            "a.rb must render exactly once, not once per colliding group entry:\n{out}"
        );
        assert_eq!(
            out.matches("b.rb").count(),
            1,
            "b.rb must render exactly once:\n{out}"
        );
        assert_eq!(
            out.matches("§ ").count(),
            1,
            "the two same-key groups must collapse into ONE heading:\n{out}"
        );
        assert!(
            out.contains("Findings / Findings (again)"),
            "neither colliding label may be silently dropped:\n{out}"
        );
    }

    #[test]
    fn human_output_never_scanned_line() {
        let body = json!({"doc_path": "foo.html", "never_scanned": true});
        let out = render_doc_human(&body, "canon");
        assert_eq!(
            out,
            "foo.html · not scanned yet — run 'kb reindex --kb canon'\n"
        );
    }

    #[test]
    fn human_output_zero_refs_line() {
        let body = json!({"doc_path": "foo.html", "never_scanned": false, "ref_count": 0});
        let out = render_doc_human(&body, "canon");
        assert_eq!(out, "foo.html · 0 refs\n");
    }

    #[test]
    fn lint_suggests_data_kb_ref_attributes() {
        let docs = vec![json!({
            "doc_id": "d1", "doc_path": "a.html",
            "refs": [
                {"ordinal": 0, "kind": "path", "raw": "order.rb", "path_hint": "order.rb",
                 "line_start": null, "line_end": null, "declared": false},
                {"ordinal": 1, "kind": "path_line", "raw": "order.rb:42", "path_hint": "order.rb",
                 "line_start": 42, "line_end": 42, "declared": false},
                {"ordinal": 2, "kind": "path_range", "raw": "order.rb:5-10", "path_hint": "order.rb",
                 "line_start": 5, "line_end": 10, "declared": false},
                {"ordinal": 3, "kind": "path_list", "raw": "order.rb:5,19,29", "path_hint": "order.rb",
                 "line_start": 5, "line_end": null, "declared": false},
            ],
        })];
        let report = build_lint_report("canon", &docs);
        let suggestions = report["docs"][0]["suggestions"].as_array().unwrap();
        assert_eq!(suggestions.len(), 4);
        assert_eq!(suggestions[0]["attr"], "order.rb");
        assert_eq!(suggestions[1]["attr"], "order.rb#L42");
        assert_eq!(suggestions[2]["attr"], "order.rb#L5-L10");
        assert_eq!(suggestions[3]["attr"], "order.rb#L5");
        assert_eq!(suggestions[3]["first_span_only"], true);
    }

    #[test]
    fn lint_excludes_symbol_and_issue_kinds() {
        let docs = vec![json!({
            "doc_id": "d1", "doc_path": "a.html",
            "refs": [
                {"ordinal": 0, "kind": "symbol_const", "raw": "Foo::Bar", "path_hint": null,
                 "line_start": null, "line_end": null, "declared": false},
                {"ordinal": 1, "kind": "issue", "raw": "#15351", "path_hint": "acme/shopfront",
                 "line_start": 15351, "line_end": null, "declared": false},
            ],
        })];
        let report = build_lint_report("canon", &docs);
        assert!(report["docs"][0]["suggestions"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(report["totals"]["excluded_symbol"], 1);
        assert_eq!(report["totals"]["excluded_issue"], 1);
        assert_eq!(report["totals"]["inferred"], 2);
    }

    #[test]
    fn lint_json_schema_shape() {
        let report = build_lint_report("canon", &[]);
        assert_eq!(report["schema"], "coderef-lint/1");
        assert_eq!(report["kb"], "canon");
        assert!(report["docs"].as_array().unwrap().is_empty());
        assert!(report["totals"].is_object());
    }

    #[test]
    fn canonical_kind_unknown_falls_into_other_not_path() {
        assert_eq!(canonical_kind("path"), "path");
        assert_eq!(canonical_kind("some_future_kind"), "other");
        assert_ne!(canonical_kind("some_future_kind"), "path");
    }

    #[test]
    fn corpus_summary_does_not_inflate_path_with_unknown_kinds() {
        let docs = vec![json!({
            "doc_id": "d1",
            "ref_count": 2,
            "refs": [
                {"kind": "path", "declared": false},
                {"kind": "a_future_kind_this_build_does_not_know", "declared": false},
            ],
        })];
        let out = render_corpus_human("canon", &docs, None);
        assert!(
            out.contains("   1  path"),
            "an unknown kind must not be counted under path:\n{out}"
        );
        assert!(
            out.contains("   1  other"),
            "an unknown kind must surface under its own other bucket:\n{out}"
        );
    }

    /// W1.gate: the "declared/inferred" and "docs with refs/docs with none"
    /// lines used a `{:>5}` field for the SECOND count on each line, glued
    /// directly onto the preceding literal word with no space in the format
    /// string. `thousands(n)` for `n >= 10,000` renders 6+ chars ("12,194"),
    /// which overflowed that field with zero padding — "declared12,194",
    /// "docs with refs12,194" — no separator at all. Widened to `{:>7}`.
    #[test]
    fn corpus_summary_keeps_a_separator_for_counts_over_ten_thousand() {
        const N: usize = 12_194;
        // ONE doc carrying N inferred refs (pushes `inferred_count` past the
        // old field's 5-char capacity without needing N separate doc
        // objects — `ref_count`/`refs` are read independently below).
        let mut refs = Vec::with_capacity(N);
        for _ in 0..N {
            refs.push(json!({"kind": "path", "declared": false}));
        }
        let mut docs = vec![json!({"doc_id": "big", "ref_count": N as u64, "refs": refs})];
        // N docs with `ref_count: 0` — `docs_with_none` is tallied one per
        // DOC (not per ref), so there is no shortcut here; each entry is a
        // tiny single-field object.
        for i in 0..N {
            docs.push(json!({"doc_id": format!("empty{i}"), "ref_count": 0}));
        }
        let out = render_corpus_human("canon", &docs, None);
        assert!(
            out.contains("declared 12,194  inferred"),
            "an inferred count >= 10,000 must stay separated from 'declared':\n{out}"
        );
        assert!(
            out.contains("docs with refs 12,194  docs with none"),
            "a docs_with_none count >= 10,000 must stay separated from 'docs with refs':\n{out}"
        );
    }
}
