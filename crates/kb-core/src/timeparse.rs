//! Tiny hand-rolled UTC timestamp parsers for the sessions pipeline.
//!
//! Two call sites parse two different shapes — the kb-capture.sh
//! filename stamp (compact) and transcript JSONL timestamps (extended
//! ISO) — and both used to hand-roll Howard Hinnant's civil-date math
//! separately ("so we don't pull chrono"… which is already in the dep
//! tree). The shared module exists so the algorithm lives exactly once;
//! the parsers stay zero-alloc because they run inside per-request
//! render/scan loops.

/// Parse `YYYYMMDDTHHMMSSZ` (the kb-capture.sh session filename stamp)
/// to unix seconds. `None` on any format glitch — callers fall back to
/// e.g. the file's mtime.
pub fn parse_compact_utc(ts: &str) -> Option<i64> {
    if ts.len() != 16 || !ts.ends_with('Z') {
        return None;
    }
    let bytes = ts.as_bytes();
    if bytes[8] != b'T' {
        return None;
    }
    let year: i64 = ts.get(0..4)?.parse().ok()?;
    let month: i64 = ts.get(4..6)?.parse().ok()?;
    let day: i64 = ts.get(6..8)?.parse().ok()?;
    let hour: i64 = ts.get(9..11)?.parse().ok()?;
    let minute: i64 = ts.get(11..13)?.parse().ok()?;
    let second: i64 = ts.get(13..15)?.parse().ok()?;
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Parse `YYYY-MM-DDTHH:MM:SS[.fff]Z` (transcript JSONL timestamps) to
/// unix seconds, ignoring the fractional part. `None` on anything
/// unrecognised.
pub fn parse_iso_utc(s: &str) -> Option<i64> {
    if s.len() < 19 {
        return None;
    }
    // `.get()` (not unchecked `s[a..b]`) so a multi-byte prefix that is long
    // enough in bytes but mid-codepoint on a range end returns None instead of
    // panicking — same shape as [`parse_compact_utc`].
    let y: i64 = s.get(0..4)?.parse().ok()?;
    let mo: i64 = s.get(5..7)?.parse().ok()?;
    let d: i64 = s.get(8..10)?.parse().ok()?;
    let h: i64 = s.get(11..13)?.parse().ok()?;
    let mi: i64 = s.get(14..16)?.parse().ok()?;
    let se: i64 = s.get(17..19)?.parse().ok()?;
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3_600 + mi * 60 + se)
}

/// Howard Hinnant's `days_from_civil` — Gregorian y/m/d → days since
/// 1970-01-01.
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_round_trips_known_values() {
        assert_eq!(parse_compact_utc("19700101T000000Z"), Some(0));
        assert_eq!(parse_compact_utc("20260524T100000Z"), Some(1_779_616_800));
    }

    #[test]
    fn compact_rejects_malformed() {
        assert!(parse_compact_utc("not-a-date").is_none());
        assert!(parse_compact_utc("20260524100000Z").is_none()); // missing T
        assert!(parse_compact_utc("20260524T100000").is_none()); // missing Z
    }

    #[test]
    fn iso_matches_compact_for_the_same_instant() {
        assert_eq!(parse_iso_utc("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_iso_utc("2026-05-24T10:00:00.123Z"),
            parse_compact_utc("20260524T100000Z"),
        );
        assert!(parse_iso_utc("nope").is_none());
    }

    #[test]
    fn iso_rejects_multibyte_without_panic() {
        // ≥19 bytes of multi-byte UTF-8: unchecked `s[0..4]` would panic
        // mid-codepoint; `.get()` returns None.
        let s = "日本語クエリですよ"; // 9 chars × 3 bytes = 27
        assert!(s.len() >= 19);
        assert!(parse_iso_utc(s).is_none());
    }

    #[test]
    fn civil_handles_pre_epoch_and_leap_years() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(days_from_civil(2024, 2, 29), 19_782);
    }
}
