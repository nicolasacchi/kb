//! Stale-salt sweep and derived-row census plumbing.
//!
//! Moved out of the store monolith (second split pass). `Store`'s private
//! connection and the helpers it shares stay in the parent module; this
//! child can call them. Public paths stay `crate::store`.
use super::*;

/// V70-A3X — a `WITH cur(salt) AS (VALUES (?),(?),...)` CTE fragment
/// listing every entry of [`crate::lang::ALL_LANGS`] (the CURRENT salt for
/// every registered language), for a repo-wide derived-table read
/// (`symbols_for_repo`, the occurrence `*_in_repo` fns) to restrict itself
/// to current-salt rows. Returns `(cte_sql, salts)` — bind `salts` FIRST
/// (the `VALUES` placeholders come before the rest of the query's own
/// params in source order).
///
/// The restriction has a deliberate FALLBACK: a caller applies it as
/// `s.salt IN (SELECT salt FROM cur) OR NOT EXISTS (SELECT 1 FROM <table>
/// t2 WHERE t2.blob_hash = <table>.blob_hash AND t2.salt IN (SELECT salt
/// FROM cur))` — i.e. "prefer the current-salt row, but if NO row for this
/// blob_hash has a current salt at all, show everything for it." A strict
/// `s.salt IN (SELECT salt FROM cur)`-only filter would silently empty out
/// every fixture across this crate's test suite that seeds symbols/
/// occurrences under a short ad hoc salt like `"rust@1"` (a deliberate,
/// widespread test convention, decoupled from `lang.rs`'s exact pinned
/// version strings) — this fallback keeps every one of those
/// byte-identical while still closing the real production bug: once a
/// GENUINE salt bump leaves an old-salt derivation coexisting with a new
/// one for the SAME blob, a current-salt sibling now exists, so the
/// fallback doesn't fire and the stale generation is correctly hidden.
///
/// V72-H2b: the set is per-FAMILY. `symbols`/`occurrences` are read
/// against the SYMBOL salts, `highlights` against the HIGHLIGHT ones —
/// passing the wrong family declares every current row of the other
/// family stale, which for the sweep would mean deleting it.
pub(super) fn current_salt_cte(family: crate::lang::SaltFamily) -> (String, Vec<&'static str>) {
    let salts = crate::lang::current_salts(family);
    let values = salts.iter().map(|_| "(?)").collect::<Vec<_>>().join(",");
    (format!("WITH cur(salt) AS (VALUES {values})"), salts)
}

/// Per-table row counts pruned by [`Store::sweep_stale_salt_derived`]
/// (V70-A3X).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StaleSaltSweepCounts {
    pub symbols: u64,
    pub highlights: u64,
    pub occurrences: u64,
    /// V72-H2b — the per-family derivation markers, swept against their
    /// OWN family's salt set (see [`SWEEP_TABLES`]).
    pub derived_status: u64,
}

impl StaleSaltSweepCounts {
    /// `true` if every table's count is zero — the common case, so the
    /// caller can skip logging a no-op sweep (mirrors `prune_stale_pins`'s
    /// `Ok(0) => {}` boot-log convention).
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    pub fn total(&self) -> u64 {
        self.symbols + self.highlights + self.occurrences + self.derived_status
    }
}

/// How many distinct `files.blob_hash` values one
/// [`Store::sweep_stale_salt_page`] covers (V72-B0). Small on purpose: this
/// is the unit of write-mutex hold time, and on a cold production store
/// every blob costs a handful of random index seeks (~50/s on spinning
/// disks), so a page is seconds, not hours.
pub const STALE_SALT_SWEEP_PAGE: usize = 128;

/// One table's worth of ONE [`Store::sweep_stale_salt_page`] — see that
/// fn's doc for the exact delete condition and why paging the driver cannot
/// change the result set. Free fn (not a `Store` method): takes an open
/// `Transaction` so all three tables' deletes share one tx.
///
/// `blobs` is this page's driver set, bound as parameters — the V72-B0 fix
/// for the un-paged `blob_hash IN (SELECT blob_hash FROM files)`, whose
/// per-statement cost was O(every live blob) regardless of how few rows
/// actually needed deleting.
pub(super) fn sweep_stale_salt_table(
    tx: &Transaction<'_>,
    cte: &str,
    salts: &[&'static str],
    blobs: &[String],
    table: &str,
    family_value: Option<&str>,
) -> Result<u64> {
    let blob_slots = blobs.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    // V72-H2b — `derived_status` holds BOTH families in one table, so its
    // two passes each add `family = ?`. The predicate rides the
    // correlated EXISTS too: a blob's SYMBOL marker must never count as
    // the current-salt sibling that authorises deleting its HIGHLIGHT one.
    let (family_pred, sibling_pred) = match family_value {
        Some(_) => (
            format!("AND {table}.family = ?"),
            format!("AND t2.family = {table}.family"),
        ),
        None => (String::new(), String::new()),
    };
    let sql = format!(
        "{cte}
         DELETE FROM {table}
         WHERE blob_hash IN ({blob_slots})
           {family_pred}
           AND salt NOT IN (SELECT salt FROM cur)
           AND EXISTS (
                 SELECT 1 FROM {table} t2
                 WHERE t2.blob_hash = {table}.blob_hash AND t2.salt IN (SELECT salt FROM cur)
                 {sibling_pred}
               )"
    );
    let mut stmt = tx.prepare(&sql)?;
    // Bind order matches the SQL: the `cur` CTE's salts are written first
    // (`{cte}` opens the statement), then this page's blob hashes, then
    // the optional family.
    let mut bind: Vec<Box<dyn rusqlite::ToSql>> = salts.iter().map(|s| Box::new(*s) as _).collect();
    for b in blobs {
        bind.push(Box::new(b.clone()));
    }
    if let Some(f) = family_value {
        bind.push(Box::new(f.to_string()));
    }
    let n = stmt.execute(rusqlite::params_from_iter(bind.iter()))?;
    Ok(n as u64)
}

/// Every table [`Store::sweep_stale_salt_page`] sweeps, with the salt
/// FAMILY that keys it and (for the one table holding both families) the
/// `family` value to restrict to. V72-H2b: the family column is what makes
/// "a family is stale only when ITS salt moved" a property of the SQL
/// rather than of the caller's memory. Adding a derived table means adding
/// a row here — an omission is a table that accumulates stale rows
/// forever, which is the defect V70-A3X shipped this sweep for.
/// Every blob-keyed derived table the re-extract bill counts (V72-H2b).
/// Deliberately WIDER than [`SWEEP_TABLES`]: the bill prices what a salt
/// bump re-derives, and `import_specs`/`call_sites`/`type_relations` ride
/// the SYMBOL salt even though the V70-A3X sweep never learned to prune
/// them (a known, named gap — see `crates/kb-code-server/CLAUDE.md`
/// invariant 11).
/// One page of [`Store::derived_row_census_page`] — a named struct rather
/// than a three-tuple so the bill's loop reads as what it is.
#[derive(Debug, Clone)]
pub struct CensusPage {
    /// Per-table row counts for THIS page's blobs, in [`BILL_TABLES`] order.
    pub counts: Vec<(&'static str, u64)>,
    /// Distinct blob hashes this page actually covered.
    pub blobs: usize,
    /// Cursor to resume from; `None` once the last page has been counted.
    pub next: Option<String>,
}

pub(crate) const BILL_TABLES: &[&str] = &[
    "symbols",
    "highlights",
    "occurrences",
    "import_specs",
    "call_sites",
    "type_relations",
    "derived_status",
];

pub(super) const SWEEP_TABLES: &[(&str, crate::lang::SaltFamily, Option<&str>)] = &[
    ("symbols", crate::lang::SaltFamily::Symbol, None),
    ("occurrences", crate::lang::SaltFamily::Symbol, None),
    ("highlights", crate::lang::SaltFamily::Highlight, None),
    (
        "derived_status",
        crate::lang::SaltFamily::Symbol,
        Some("symbols"),
    ),
    (
        "derived_status",
        crate::lang::SaltFamily::Highlight,
        Some("highlights"),
    ),
];

/// V70-A3X — a `LIKE` pattern matching every salt of `salt`'s OWN language
/// (e.g. `salt = "rust@0.24.2+q3"` → `"rust@%"`), used to purge a blob's
/// STALE-salt `symbols`/`highlights`/`occurrences` rows before writing a
/// fresh derivation under a NEW salt (a grammar/query version bump — see
/// `lang.rs`'s module doc). Deliberately narrower than a bare `blob_hash`
/// match: a degenerate/empty file can share one `blob_hash` across
/// DIFFERENT languages (different extensions detecting to different
/// `LangInfo`s over identical bytes), and this must never purge a sibling
/// language's CURRENT rows for that same blob. Salt ids are a closed,
/// alphanumeric set (`lang::ALL_LANGS`), so no `LIKE`-wildcard-escaping
/// concern for the prefix itself.
pub(super) fn lang_prefix_pattern(salt: &str) -> String {
    format!("{}@%", salt.split('@').next().unwrap_or(salt))
}

/// Write the `(blob_hash, family, salt)` derivation marker inside an
/// already-open transaction, purging every OTHER salt of this blob's
/// language FOR THIS FAMILY first (invariant 11's purge-on-write rule,
/// applied to the marker table). Scoped by `family` as well as by language
/// prefix: a symbol-salt bump must not erase the highlight marker, which is
/// the entire point of V72-H2b's split.
pub(super) fn mark_derived_in(
    tx: &Transaction<'_>,
    blob_hash: &str,
    family: crate::lang::SaltFamily,
    salt: &str,
    rows: usize,
) -> Result<()> {
    tx.execute(
        "DELETE FROM derived_status \
         WHERE blob_hash = ?1 AND family = ?2 AND salt LIKE ?3 AND salt != ?4",
        params![blob_hash, family.as_str(), lang_prefix_pattern(salt), salt],
    )?;
    tx.execute(
        "INSERT INTO derived_status (blob_hash, family, salt, rows) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(blob_hash, family, salt) DO UPDATE SET rows = excluded.rows",
        params![blob_hash, family.as_str(), salt, rows as i64],
    )?;
    Ok(())
}
