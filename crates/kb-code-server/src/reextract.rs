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
    let sample_started = std::time::Instant::now();
    let mut sample_complete = true;
    let mut languages: Vec<LangBill> = Vec::with_capacity(per_lang.len());
    for (lang, files, bytes) in per_lang {
        let info = crate::lang::for_id(&lang);
        // The tier's plan is the SAME short-circuit `ingest::index_file`
        // consults — never a second opinion about what runs.
        let plan = crate::syntax::REGISTRY
            .iter()
            .find(|r| r.lang == lang)
            .map(|r| r.plan())
            .unwrap_or(crate::syntax::Plan {
                highlight: false,
                symbols: false,
            });
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
            if sample_started.elapsed() >= SAMPLE_BUDGET {
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
}
