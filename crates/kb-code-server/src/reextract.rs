//! `reextract-bill/1` (V72-H2b, D7) — what a salt bump would cost.
//!
//! D7 asks for the re-extract bill to be **measured** and recorded per
//! milestone, and the reason is the shape of the thing being priced: a
//! `symbol_salt` or `highlight_salt` bump (`crate::lang`) invalidates every
//! cached row of that family across the whole mirror, and the only honest
//! way to know what that means for a 6,000-file monolith is to time the
//! extractors on that monolith's own bytes. A hand estimate ("Ruby is
//! slow-ish") is exactly the kind of claim this instrument refuses to make
//! elsewhere.
//!
//! The bill has three parts, and each is labelled with how it was
//! obtained, because they are not equally certain:
//!
//! 1. **The census — EXACT.** Files and bytes per `files.lang` for the
//!    repo (one indexed `GROUP BY`), and rows per derived table
//!    (`store::BILL_TABLES`) over the blobs this repo's `files` rows
//!    reach. The row census is PAGED and budgeted for the V72-B0 reason:
//!    the un-paged form is O(every live blob) random seeks per table with
//!    the store's single connection mutex held, which on a production
//!    mirror is an outage, not a query. When the budget runs out the bill
//!    says so (`census_complete: false`) and reports how far it got rather
//!    than presenting a partial count as a total.
//! 2. **The timed sample — MEASURED, on a bounded subset.** Up to
//!    [`DEFAULT_SAMPLE`] files per language (deterministically the first
//!    ones in `path` order, so two runs are comparable), read from the
//!    working tree and pushed through the REAL extractors —
//!    `extract::extract_symbols` and `highlight::extract_highlights`, the
//!    same calls `ingest::index_file` makes — with the results discarded.
//!    Nothing is written: a bill that mutated the cache it is pricing
//!    would be measuring its own second run.
//! 3. **The projection — EXTRAPOLATED, and named as such.** The sample's
//!    measured bytes-per-second scaled to the language's total indexed
//!    bytes. Every projected field carries `projection` explaining the
//!    scale factor, and a language with no timed sample reports `null`
//!    rather than a zero that reads like "free".
//!
//! Deliberately NOT here: a verb that performs the re-extract. Bumping a
//! salt is an edit to `crate::lang` and a deploy; the mirror re-derives
//! itself on the next visit through the ordinary two gates
//! (`ingest::index_file`). A "force re-extract now" button would be a
//! whole-corpus maintenance pass with a trigger, which is the shape
//! invariant 11(a)/(b) exists to keep out of this daemon.

use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::routes::{find_repo, ApiError};
use crate::state::SharedState;
use crate::store::StoreBlocking;

pub const BILL_SCHEMA: &str = "reextract-bill/1";

/// Files per language in the timed sample, when the caller names no
/// `sample`. D7's own wording ("a bounded sample") — 200 files is enough
/// for a stable bytes/second on every language in the target repos and
/// small enough that the whole pass stays inside [`SAMPLE_BUDGET`].
pub const DEFAULT_SAMPLE: usize = 200;
/// Hard cap on `?sample=`. A caller asking for more is asking for a full
/// re-extract with the results thrown away.
pub const MAX_SAMPLE: usize = 1_000;
/// Wall-clock budget for the WHOLE timed pass. Languages reached after it
/// expires report no sample and say why — never a silent zero.
pub const SAMPLE_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);
/// Wall-clock budget for the paged row census, same posture.
pub const CENSUS_BUDGET: std::time::Duration = std::time::Duration::from_secs(10);
/// Distinct `files.blob_hash` values per census page — the unit of
/// connection-mutex hold time (`store::STALE_SALT_SWEEP_PAGE`'s reason).
pub const CENSUS_PAGE: usize = 256;

#[derive(Debug, Deserialize)]
pub struct BillParams {
    pub repo: String,
    /// Files per language in the timed sample (default [`DEFAULT_SAMPLE`],
    /// capped at [`MAX_SAMPLE`]). `0` skips the timed pass entirely and
    /// returns the census alone.
    pub sample: Option<usize>,
}

/// One language's line of the bill.
#[derive(Debug, Clone, Serialize)]
pub struct LangBill {
    /// The `files.lang` value — a language id, or one of the content skip
    /// markers (`unknown`/`binary`/`too-large`/`lfs`), which cost nothing
    /// to re-extract and are reported so the total adds up.
    pub lang: String,
    pub files: u64,
    pub bytes: u64,
    /// `null` for a skip-marker row and for a registry row with no engine.
    pub symbol_salt: Option<&'static str>,
    pub highlight_salt: Option<&'static str>,
    /// What a bump of each salt would actually re-run for this language,
    /// from `syntax::SyntaxRow::plan` — the SAME short-circuit ingest
    /// consults, so the bill cannot price a pass the pipeline skips.
    pub derives_symbols: bool,
    pub derives_highlights: bool,
    /// Files actually read and timed. `0` when the language derives
    /// nothing, when `sample=0`, or when the budget expired first.
    pub sampled_files: u64,
    pub sampled_bytes: u64,
    /// V77-P3 (Task 2) — this language's OWN slice of the bill's shared
    /// wall-clock budget (`per_lang_allotments`), surfaced so a "not timed"
    /// row's honesty is checkable against a real number rather than only
    /// the total `SAMPLE_BUDGET`. `null` for a skip-marker row, a
    /// registry row with no engine, or `sample=0` — the same set of rows
    /// that never reach the allotment split at all.
    pub sample_allotment_ms: Option<f64>,
    /// Measured wall clock over the sample, per family. `null` when that
    /// family was not run.
    pub symbol_ms: Option<f64>,
    pub highlight_ms: Option<f64>,
    /// `bytes / sampled_bytes` — the number the projections multiply by.
    /// `null` when there is no sample to scale.
    pub scale: Option<f64>,
    pub projected_symbol_ms: Option<f64>,
    pub projected_highlight_ms: Option<f64>,
    /// How the projected numbers were obtained, in words, on every row —
    /// including the rows that have none.
    pub projection: String,
}

/// One derived table's row count for this repo's blobs.
#[derive(Debug, Clone, Serialize)]
pub struct TableRows {
    pub table: &'static str,
    pub rows: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BillTotals {
    pub files: u64,
    pub bytes: u64,
    pub rows: u64,
    pub projected_symbol_ms: f64,
    pub projected_highlight_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct BillOut {
    pub schema: &'static str,
    pub repo: String,
    pub generated_at: i64,
    pub role_table_version: u32,
    pub languages: Vec<LangBill>,
    pub tables: Vec<TableRows>,
    pub totals: BillTotals,
    /// The requested sample size per language, after the cap.
    pub sample: usize,
    /// `false` when [`CENSUS_BUDGET`] expired before every blob was
    /// counted — `tables` is then a floor, not a total.
    pub census_complete: bool,
    pub census_blobs: u64,
    /// `false` when [`SAMPLE_BUDGET`] expired before every language was
    /// timed.
    pub sample_complete: bool,
    /// One sentence a reader should not have to reconstruct.
    pub honesty: &'static str,
}

const HONESTY: &str = "files, bytes and rows are exact for this repo's current files rows; \
     the per-language milliseconds are MEASURED on the sampled files only and the projected_* \
     fields are those measurements scaled by bytes — an extrapolation, not a measurement";

/// V77-P3 (Task 2, the E6 finding) — the fraction of [`SAMPLE_BUDGET`]
/// reserved and split EVENLY across every billable language, before the
/// rest is split by byte share (see [`per_lang_allotments`]). Guarantees a
/// non-zero allotment for every present language regardless of its byte
/// share, so a genuinely tiny language still gets a chance to report a real
/// (if noisy) timed row instead of a permanent "not timed".
const ALLOTMENT_FLOOR_FRACTION: f64 = 0.1;

/// The `syntax/1` plan for a `files.lang` value — the SAME short-circuit
/// `ingest::index_file` consults, factored out so both the allotment split
/// below and the per-language loop ask the identical question.
fn plan_for_lang(lang: &str) -> crate::syntax::Plan {
    crate::syntax::REGISTRY
        .iter()
        .find(|r| r.lang == lang)
        .map(|r| r.plan())
        .unwrap_or(crate::syntax::Plan {
            highlight: false,
            symbols: false,
        })
}

/// Per-language wall-clock allotments for the timed sample pass, summing to
/// EXACTLY [`SAMPLE_BUDGET`] (the total ceiling is unchanged — only its
/// distribution). `billable` is `(lang, bytes)` for every language this
/// repo has that actually derives something (`plan.symbols ||
/// plan.highlight`) — the E6 finding was HAML (small, but early in the OLD
/// alphabetical order) starving Ruby (the repo's dominant language) out of
/// a SHARED clock entirely; giving each language its own slice, sized
/// mostly by its byte SHARE of the billable total, means the dominant
/// language's allotment no longer depends on where some other language
/// happens to sit in the list. [`ALLOTMENT_FLOOR_FRACTION`] of the budget
/// is reserved and divided evenly first, so every entry in `billable` gets
/// a strictly positive allotment even when its byte share alone would
/// round to zero.
fn per_lang_allotments(
    billable: &[(String, u64)],
) -> std::collections::HashMap<String, std::time::Duration> {
    let n = billable.len();
    if n == 0 {
        return std::collections::HashMap::new();
    }
    let total_bytes: u64 = billable.iter().map(|(_, b)| *b).sum();
    let budget_secs = SAMPLE_BUDGET.as_secs_f64();
    let floor_total = budget_secs * ALLOTMENT_FLOOR_FRACTION;
    let floor_each = floor_total / n as f64;
    let remaining = budget_secs - floor_total;
    billable
        .iter()
        .map(|(lang, bytes)| {
            let share = if total_bytes == 0 {
                1.0 / n as f64
            } else {
                *bytes as f64 / total_bytes as f64
            };
            let secs = (floor_each + remaining * share).max(0.0);
            (lang.clone(), std::time::Duration::from_secs_f64(secs))
        })
        .collect()
}

/// The census + the timed sample, as one blocking unit. Split out from the
/// route so it is testable without an axum stack, and `pub(crate)` so the
/// in-crate tests can drive it against a fixture repo.
pub(crate) fn build_bill(
    store: &crate::store::Store,
    repo_root: &std::path::Path,
    repo_id: i64,
    repo_name: &str,
    sample: usize,
) -> Result<BillOut, crate::store::StoreError> {
    let per_lang = store.files_by_lang(repo_id)?;

    // --- part 1: the paged, budgeted row census (EXACT, or honest) ------
    let census_started = std::time::Instant::now();
    let mut table_rows: std::collections::BTreeMap<&'static str, u64> = crate::store::BILL_TABLES
        .iter()
        .map(|t| (*t, 0u64))
        .collect();
    let mut cursor: Option<String> = None;
    let mut census_blobs: u64 = 0;
    let mut census_complete = true;
    loop {
        let page = store.derived_row_census_page(cursor.as_deref(), CENSUS_PAGE)?;
        for (table, n) in page.counts {
            *table_rows.entry(table).or_default() += n;
        }
        census_blobs += page.blobs as u64;
        match page.next {
            None => break,
            Some(c) => cursor = Some(c),
        }
        if census_started.elapsed() >= CENSUS_BUDGET {
            census_complete = false;
            break;
        }
    }

    // --- part 2 + 3: the timed sample, then the projection --------------
    //
    // V77-P3 (Task 2) — every billable language's OWN slice of
    // `SAMPLE_BUDGET`, computed ONCE up front from `per_lang` (already
    // ordered biggest-bytes-first by `Store::files_by_lang`) so the E6
    // starvation scenario cannot recur: a language's timed-sample chance no
    // longer depends on which OTHER language happened to run before it.
    let billable: Vec<(String, u64)> = per_lang
        .iter()
        .filter(|(lang, _, _)| {
            crate::lang::for_id(lang).is_some() && {
                let plan = plan_for_lang(lang);
                plan.symbols || plan.highlight
            }
        })
        .map(|(lang, _, bytes)| (lang.clone(), *bytes))
        .collect();
    let allotments = per_lang_allotments(&billable);

    let sample_started = std::time::Instant::now();
    let mut sample_complete = true;
    let mut languages: Vec<LangBill> = Vec::with_capacity(per_lang.len());
    for (lang, files, bytes) in per_lang {
        let info = crate::lang::for_id(&lang);
        // The tier's plan is the SAME short-circuit `ingest::index_file`
        // consults — never a second opinion about what runs.
        let plan = plan_for_lang(&lang);
        let mut row = LangBill {
            lang: lang.clone(),
            files,
            bytes,
            symbol_salt: info.map(|i| i.symbol_salt),
            highlight_salt: info.map(|i| i.highlight_salt),
            derives_symbols: plan.symbols,
            derives_highlights: plan.highlight,
            sampled_files: 0,
            sampled_bytes: 0,
            sample_allotment_ms: None,
            symbol_ms: None,
            highlight_ms: None,
            scale: None,
            projected_symbol_ms: None,
            projected_highlight_ms: None,
            projection: String::new(),
        };
        let Some(info) = info else {
            row.projection = format!(
                "not a parsed language ({lang}) — a salt bump re-derives nothing for these files"
            );
            languages.push(row);
            continue;
        };
        if !plan.symbols && !plan.highlight {
            row.projection =
                "tier none — neither family is derived for this type, so a bump costs nothing"
                    .to_string();
            languages.push(row);
            continue;
        }
        if sample == 0 {
            row.projection = "no timed sample requested (sample=0)".to_string();
            languages.push(row);
            continue;
        }
        if sample_started.elapsed() >= SAMPLE_BUDGET {
            sample_complete = false;
            row.projection =
                "not timed — the bill's wall-clock budget expired before this language".to_string();
            languages.push(row);
            continue;
        }

        // V77-P3 (Task 2) — this language's OWN slice of the remaining
        // budget (see `per_lang_allotments`'s doc). Every billable language
        // reaching this point has a strictly positive allotment (the
        // `ALLOTMENT_FLOOR_FRACTION` floor), so `unwrap_or` never actually
        // falls back to the whole `SAMPLE_BUDGET` in practice — it exists
        // only as a defensive default, never as the value driving a real
        // "not timed" row.
        let allotment = allotments.get(&lang).copied().unwrap_or(SAMPLE_BUDGET);
        row.sample_allotment_ms = Some(allotment.as_secs_f64() * 1000.0);
        let lang_started = std::time::Instant::now();

        let mut sym_nanos: u128 = 0;
        let mut hl_nanos: u128 = 0;
        for path in store.sample_paths_for_lang(repo_id, &lang, sample)? {
            let abs = repo_root.join(&path);
            let Ok(content) = std::fs::read(&abs) else {
                // The mirror can be a commit ahead of the working tree.
                // A file we cannot read is skipped, never guessed at.
                continue;
            };
            if content.len() as u64 > crate::ingest::MAX_PARSE_BYTES
                || std::str::from_utf8(&content).is_err()
            {
                continue;
            }
            row.sampled_files += 1;
            row.sampled_bytes += content.len() as u64;
            if plan.symbols {
                let t = std::time::Instant::now();
                let _ = crate::extract::extract_symbols(info.id, &content);
                sym_nanos += t.elapsed().as_nanos();
            }
            if plan.highlight {
                let t = std::time::Instant::now();
                let _ = crate::highlight::extract_highlights(info.id, &content);
                hl_nanos += t.elapsed().as_nanos();
            }
            // `lang_started`/`allotment`, not the shared `sample_started`/
            // `SAMPLE_BUDGET` clock this used to check — a language that
            // genuinely exceeds ITS OWN slice still stops honestly (a
            // partial sample, same "measured on X of Y" projection below),
            // but no longer at the cost of a language that hasn't even
            // started sampling yet.
            if lang_started.elapsed() >= allotment {
                sample_complete = false;
                break;
            }
        }
        if plan.symbols {
            row.symbol_ms = Some(sym_nanos as f64 / 1e6);
        }
        if plan.highlight {
            row.highlight_ms = Some(hl_nanos as f64 / 1e6);
        }
        if row.sampled_bytes > 0 {
            let scale = bytes as f64 / row.sampled_bytes as f64;
            row.scale = Some(scale);
            row.projected_symbol_ms = row.symbol_ms.map(|ms| ms * scale);
            row.projected_highlight_ms = row.highlight_ms.map(|ms| ms * scale);
            row.projection = format!(
                "measured on {} of {} file(s) ({} of {} bytes), scaled by {scale:.2}× — an \
                 extrapolation",
                row.sampled_files, files, row.sampled_bytes, bytes
            );
        } else {
            row.projection =
                "no readable sample file — nothing measured, nothing projected".to_string();
        }
        languages.push(row);
    }

    let totals = BillTotals {
        files: languages.iter().map(|l| l.files).sum(),
        bytes: languages.iter().map(|l| l.bytes).sum(),
        rows: table_rows.values().sum(),
        projected_symbol_ms: languages.iter().filter_map(|l| l.projected_symbol_ms).sum(),
        projected_highlight_ms: languages
            .iter()
            .filter_map(|l| l.projected_highlight_ms)
            .sum(),
    };
    Ok(BillOut {
        schema: BILL_SCHEMA,
        repo: repo_name.to_string(),
        generated_at: chrono::Utc::now().timestamp(),
        role_table_version: crate::highlight::ROLE_TABLE_VERSION,
        languages,
        tables: crate::store::BILL_TABLES
            .iter()
            .map(|t| TableRows {
                table: t,
                rows: table_rows.get(t).copied().unwrap_or(0),
            })
            .collect(),
        totals,
        sample,
        census_complete,
        census_blobs,
        sample_complete,
        honesty: HONESTY,
    })
}

/// `GET /api/reextract/bill?repo=[&sample=]` — see the module doc.
/// An ordinary `auth_bearer` READ: it writes nothing, and the extractors
/// it times are the same pure functions the ingest pipeline calls.
pub async fn bill_route(
    State(state): State<SharedState>,
    Query(params): Query<BillParams>,
) -> Result<impl IntoResponse, ApiError> {
    let (repo, repo_id) = find_repo(&state, &params.repo)?;
    let repo_root = repo.path.clone();
    let repo_name = params.repo.clone();
    let sample = params.sample.unwrap_or(DEFAULT_SAMPLE).min(MAX_SAMPLE);
    let body = state
        .store
        .run_blocking(move |store| -> Result<BillOut, ApiError> {
            Ok(build_bill(store, &repo_root, repo_id, &repo_name, sample)?)
        })
        .await?;
    Ok(Json(body))
}

fn bill_params_accept_without(omit: &str) -> bool {
    let mut map = serde_json::Map::new();
    for (k, v) in [("repo", "r")] {
        if k != omit {
            map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
        }
    }
    serde_json::from_value::<BillParams>(serde_json::Value::Object(map)).is_ok()
}

pub const BILL_ROUTE: crate::entities::RouteContract = crate::entities::RouteContract {
    path: "/api/reextract/bill",
    handler: "reextract::bill_route",
    required_params: &["repo"],
    params_accept_without: bill_params_accept_without,
};

/// Every route V72-H2b adds. Walked from BOTH sides exactly as
/// `syntax::V72_H1_ROUTES` is (invariant 15).
pub const V72_H2B_ROUTES: &[crate::entities::RouteContract] = &[BILL_ROUTE];

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTER_SRC: &str = include_str!("router.rs");

    /// A tiny mirrored repo: a store, a working tree on disk, and every
    /// file indexed through the REAL `ingest::index_file` — so the census
    /// counts rows a real derivation wrote, not rows a test inserted.
    fn bill_fixture() -> (
        tempfile::TempDir,
        crate::store::Store,
        std::path::PathBuf,
        i64,
    ) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(root.join("src")).unwrap();
        let files: &[(&str, &str)] = &[
            (
                "src/lib.rs",
                "//! doc\npub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n",
            ),
            (
                "src/other.rs",
                "pub struct Widget;\nimpl Widget {\n    pub fn new() -> Self {\n        Self\n    }\n}\n",
            ),
            ("src/app.py", "def greet(name):\n    return f\"hi {name}\"\n"),
            ("src/style.scss", "$brand: #336699;\n.card { color: $brand; }\n"),
            ("src/view.html.erb", "<%= render 'row' %>\n"),
            ("README", "not a language\n"),
        ];
        let store = crate::store::Store::open(&tmp.path().join("index.db")).expect("store");
        let repo_id = store
            .upsert_repo("fixture", &root.to_string_lossy())
            .expect("repo");
        for (rel, body) in files {
            let abs = root.join(rel);
            std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
            std::fs::write(&abs, body).unwrap();
            let hash = crate::ingest::git_blob_hash(body.as_bytes());
            crate::ingest::index_file(
                &store,
                repo_id,
                rel,
                body.as_bytes(),
                &hash,
                true,
                false,
                &crate::comments::KeywordSet::defaults(),
            )
            .expect("index");
        }
        (tmp, store, root, repo_id)
    }

    /// Same shape as [`bill_fixture`], but with an explicit `(path, body)`
    /// list — lets the byte-desc / per-language-allotment tests (V77-P3,
    /// Task 2) control exactly which language is big and which is small,
    /// independent of the fixed corpus the other tests pin against.
    fn bill_fixture_from(
        files: &[(&str, String)],
    ) -> (
        tempfile::TempDir,
        crate::store::Store,
        std::path::PathBuf,
        i64,
    ) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("repo");
        let store = crate::store::Store::open(&tmp.path().join("index.db")).expect("store");
        let repo_id = store
            .upsert_repo("fixture", &root.to_string_lossy())
            .expect("repo");
        for (rel, body) in files {
            let abs = root.join(rel);
            std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
            std::fs::write(&abs, body).unwrap();
            let hash = crate::ingest::git_blob_hash(body.as_bytes());
            crate::ingest::index_file(
                &store,
                repo_id,
                rel,
                body.as_bytes(),
                &hash,
                true,
                false,
                &crate::comments::KeywordSet::defaults(),
            )
            .expect("index");
        }
        (tmp, store, root, repo_id)
    }

    /// `unit` repeated until the result is at least `min_bytes` long —
    /// deterministic, syntactically-plausible filler content for a given
    /// language (real parse/highlight errors are swallowed by the sample
    /// pass regardless, so this only needs to be long, not perfectly
    /// valid).
    fn repeat_to_at_least(unit: &str, min_bytes: usize) -> String {
        let mut s = String::with_capacity(min_bytes + unit.len());
        while s.len() < min_bytes {
            s.push_str(unit);
        }
        s
    }

    #[test]
    fn every_declared_v72_h2b_route_is_registered_and_requires_its_params() {
        assert!(!V72_H2B_ROUTES.is_empty());
        for c in V72_H2B_ROUTES {
            let nested = c
                .path
                .strip_prefix("/api")
                .expect("every route path is /api-nested");
            assert!(
                ROUTER_SRC.contains(&format!("\"{nested}\"")),
                "{}: declared but never registered in router.rs",
                c.path
            );
            assert!(
                ROUTER_SRC.contains(c.handler),
                "{}: registered path but no {} handler named in router.rs",
                c.path,
                c.handler
            );
            assert!((c.params_accept_without)(""));
            for p in c.required_params {
                assert!(
                    !(c.params_accept_without)(p),
                    "{}: declares {p:?} required but its params struct accepts a request \
                     without it",
                    c.path
                );
            }
        }
    }

    /// The bill's SHAPE, against the e2e fixture repo: every number
    /// present and internally consistent. Values are deliberately NOT
    /// asserted — a wall-clock measurement is not a golden — but their
    /// RELATIONSHIPS are, because those are what make the bill readable.
    #[test]
    fn the_bill_is_internally_consistent_over_a_fixture_repo() {
        let (_tmp, store, root, repo_id) = bill_fixture();
        let bill = build_bill(&store, &root, repo_id, "fixture", 50).expect("bill");

        assert_eq!(bill.schema, BILL_SCHEMA);
        assert_eq!(
            bill.role_table_version,
            crate::highlight::ROLE_TABLE_VERSION
        );
        assert!(bill.census_complete, "a fixture repo fits in the budget");
        assert!(bill.sample_complete);
        assert!(!bill.languages.is_empty());

        // The census adds up to the totals, and every declared table has a
        // row (a missing one would be a table the bill silently ignores).
        assert_eq!(bill.tables.len(), crate::store::BILL_TABLES.len());
        assert_eq!(
            bill.totals.rows,
            bill.tables.iter().map(|t| t.rows).sum::<u64>()
        );
        assert_eq!(
            bill.totals.files,
            bill.languages.iter().map(|l| l.files).sum::<u64>()
        );
        assert!(bill.totals.bytes > 0);

        let rust = bill
            .languages
            .iter()
            .find(|l| l.lang == "rust")
            .expect("the fixture has rust files");
        assert_eq!(rust.symbol_salt, Some(crate::lang::RUST.symbol_salt));
        assert_eq!(rust.highlight_salt, Some(crate::lang::RUST.highlight_salt));
        assert!(rust.derives_symbols && rust.derives_highlights);
        assert!(rust.sampled_files > 0, "a sample was actually timed");
        assert!(rust.sampled_bytes > 0);
        // MEASURED and PROJECTED are different fields, and the projection
        // is the measurement times the byte scale — stated, not implied.
        let scale = rust.scale.expect("a scale");
        assert!((scale - rust.bytes as f64 / rust.sampled_bytes as f64).abs() < 1e-9);
        let sym = rust.symbol_ms.expect("symbols were timed");
        let proj = rust.projected_symbol_ms.expect("and projected");
        assert!((proj - sym * scale).abs() < 1e-6);
        assert!(rust.projection.contains("extrapolation"));

        // Every row explains itself, including the ones with no numbers.
        for l in &bill.languages {
            assert!(!l.projection.is_empty(), "{}: unexplained row", l.lang);
            if l.sampled_bytes == 0 {
                assert!(l.projected_symbol_ms.is_none());
                assert!(l.projected_highlight_ms.is_none());
            }
        }
    }

    /// `sample=0` is the census alone — and it must still be a complete,
    /// self-describing bill rather than a half-populated one.
    #[test]
    fn sample_zero_returns_the_census_with_no_timings() {
        let (_tmp, store, root, repo_id) = bill_fixture();
        let bill = build_bill(&store, &root, repo_id, "fixture", 0).expect("bill");
        assert_eq!(bill.sample, 0);
        assert_eq!(bill.totals.projected_symbol_ms, 0.0);
        for l in &bill.languages {
            assert_eq!(l.sampled_files, 0);
            assert!(l.symbol_ms.is_none());
            assert!(!l.projection.is_empty());
        }
        assert!(bill.totals.rows > 0, "the census is independent of timing");
    }

    // --- V77-P3 (Task 2, the E6 finding) ---------------------------------

    /// Unit — `per_lang_allotments`'s own contract: the split always sums
    /// to EXACTLY `SAMPLE_BUDGET` (the total ceiling is unchanged, only its
    /// distribution) and every present language gets a strictly positive
    /// slice, regardless of how lopsided the byte shares are.
    #[test]
    fn per_lang_allotments_sums_to_the_budget_and_every_language_is_positive() {
        let billable = vec![
            ("ruby".to_string(), 990_000u64),
            ("haml".to_string(), 1_000u64),
            ("erb".to_string(), 9_000u64),
        ];
        let allotments = per_lang_allotments(&billable);
        assert_eq!(allotments.len(), billable.len());

        let sum: std::time::Duration = allotments.values().copied().sum();
        // Tolerance is microseconds, not nanoseconds: each allotment is
        // independently rounded to the nearest nanosecond by
        // `Duration::from_secs_f64`, so summing a handful of them can
        // accumulate a few ns of rounding error even when the underlying
        // f64 arithmetic is exact — a real bug in the split would be off
        // by whole seconds, not a few billionths of one.
        assert!(
            (sum.as_secs_f64() - SAMPLE_BUDGET.as_secs_f64()).abs() < 1e-6,
            "allotments must sum to (within float/duration rounding of) SAMPLE_BUDGET: got {sum:?}"
        );
        for (lang, _) in &billable {
            let a = allotments[lang];
            assert!(
                a > std::time::Duration::ZERO,
                "{lang}: allotment must be > 0"
            );
        }
        // The dominant language's slice is still the biggest, despite the
        // floor — the floor guarantees a MINIMUM, it does not flatten the
        // distribution.
        assert!(allotments["ruby"] > allotments["haml"]);
        assert!(allotments["ruby"] > allotments["erb"]);
    }

    /// Unit — a single billable language still gets the WHOLE budget (no
    /// floor arithmetic edge case at `n == 1`).
    #[test]
    fn per_lang_allotments_gives_the_whole_budget_to_a_single_language() {
        let billable = vec![("rust".to_string(), 1_000u64)];
        let allotments = per_lang_allotments(&billable);
        assert_eq!(allotments.len(), 1);
        assert!((allotments["rust"].as_secs_f64() - SAMPLE_BUDGET.as_secs_f64()).abs() < 1e-9);
    }

    /// Unit — no billable languages at all is a valid, empty answer (never
    /// a divide-by-zero).
    #[test]
    fn per_lang_allotments_is_empty_for_no_billable_languages() {
        assert!(per_lang_allotments(&[]).is_empty());
    }

    /// `Store::files_by_lang` (and therefore `build_bill`'s own iteration
    /// order) is bytes DESC, not alphabetical — a fixture whose
    /// alphabetically-EARLY language ("bash") is byte-SMALL and whose
    /// alphabetically-LATE language ("yaml") is byte-BIG must still report
    /// yaml first.
    #[test]
    fn build_bill_orders_languages_by_bytes_desc_not_alphabetically() {
        let small_bash = "echo hi\n".to_string();
        let big_yaml = repeat_to_at_least("key: value\n", 50_000);
        let files: Vec<(&str, String)> = vec![("small.sh", small_bash), ("big.yml", big_yaml)];
        let (_tmp, store, root, repo_id) = bill_fixture_from(&files);

        let bill = build_bill(&store, &root, repo_id, "fixture", 50).expect("bill");
        let real_langs: Vec<&str> = bill
            .languages
            .iter()
            .filter(|l| l.lang == "bash" || l.lang == "yaml")
            .map(|l| l.lang.as_str())
            .collect();
        assert_eq!(
            real_langs,
            vec!["yaml", "bash"],
            "bytes-desc order must put the byte-BIG yaml row before the byte-small bash row, \
             despite bash sorting first alphabetically: {:?}",
            bill.languages
                .iter()
                .map(|l| (l.lang.as_str(), l.bytes))
                .collect::<Vec<_>>()
        );
    }

    /// HTTP route logic (`reextract::build_bill`, what `bill_route` calls) —
    /// the E6-shaped fixture: a BIG early-alphabet language ("bash") beside
    /// a SMALL late-alphabet one ("yaml"). Both must come back with a
    /// TIMED row (`sampled_files > 0`) and a strictly positive
    /// `sample_allotment_ms` — the floor guarantee closing the exact
    /// starvation the E6 finding described (a small-but-early language
    /// consuming a SHARED clock and leaving nothing for whatever came
    /// after it in the list).
    #[test]
    fn build_bill_gives_a_small_late_language_a_timed_row_beside_a_big_early_one() {
        let big_bash = repeat_to_at_least("echo \"line\"\n", 200_000);
        let small_yaml = "key: value\n".to_string();
        let files: Vec<(&str, String)> = vec![("big.sh", big_bash), ("small.yml", small_yaml)];
        let (_tmp, store, root, repo_id) = bill_fixture_from(&files);

        let bill = build_bill(&store, &root, repo_id, "fixture", 50).expect("bill");
        let bash = bill
            .languages
            .iter()
            .find(|l| l.lang == "bash")
            .expect("bash row");
        let yaml = bill
            .languages
            .iter()
            .find(|l| l.lang == "yaml")
            .expect("yaml row");

        // bash is listed first (bytes-desc) and gets the bigger allotment...
        assert!(bash.bytes > yaml.bytes);
        let bash_allotment = bash.sample_allotment_ms.expect("bash allotment");
        let yaml_allotment = yaml.sample_allotment_ms.expect("yaml allotment");
        assert!(bash_allotment > yaml_allotment);
        // ...but yaml — small AND alphabetically after bash — still gets a
        // strictly positive slice and an actual timed row, not a permanent
        // "not timed".
        assert!(
            yaml_allotment > 0.0,
            "yaml must get a non-zero floor allotment"
        );
        assert!(
            yaml.sampled_files > 0,
            "yaml must still get a timed row: {yaml:?}"
        );
        assert!(yaml.symbol_ms.is_some() || yaml.highlight_ms.is_some());
        assert!(bash.sampled_files > 0, "bash must get a timed row too");
    }
}
