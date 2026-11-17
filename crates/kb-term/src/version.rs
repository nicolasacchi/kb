//! The version `kb-term`, `kb-termd` and `kb-tap` report.
//!
//! The workspace `version` is pinned at `0.0.0` forever (versions live in git
//! tags), so `CARGO_PKG_VERSION` is never a release string. The two bins read
//! the build's stamp from plain compile-time env (`option_env!`), exactly as
//! the kb-code-server bin does, and hand it to [`set_own_version`]: this lib
//! must NOT link `kb-buildstamp` (a package dependency would attach to the lib
//! and drag every kb-term test target into the per-commit rebuild, PF-B1).
//! A tag build sets `KB_BUILD_VERSION` to the tag (`v0.52.1` reports
//! `0.52.1`); a main build sets it to the archive version (`main-<sha12>`);
//! an unstamped dev build reports [`UNSTAMPED_VERSION`], never a bare `0.0.0`
//! that reads like a release.

use std::sync::OnceLock;

/// What an unstamped (dev) build reports.
pub const UNSTAMPED_VERSION: &str = "0.0.0-dev";

/// Normalise a raw `KB_BUILD_VERSION`: surrounding whitespace is dropped, one
/// leading `v` is stripped when a digit follows (a `v0.52.1` tag), and an
/// empty value is not a version, so it becomes [`UNSTAMPED_VERSION`].
pub fn normalize_stamp_version(raw: &str) -> &str {
    let raw = raw.trim();
    let stripped = match raw.strip_prefix('v') {
        Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit()) => rest,
        _ => raw,
    };
    if stripped.is_empty() {
        UNSTAMPED_VERSION
    } else {
        stripped
    }
}

/// The version a bin reports for its compile-time `option_env!("KB_BUILD_VERSION")`.
pub fn stamped_version(raw: Option<&'static str>) -> &'static str {
    normalize_stamp_version(raw.unwrap_or(UNSTAMPED_VERSION))
}

static OWN_VERSION: OnceLock<&'static str> = OnceLock::new();

/// Record this process's own reported version (first caller wins). Called by
/// the bins before anything can ask for it.
pub fn set_own_version(version: &'static str) {
    let _ = OWN_VERSION.set(version);
}

/// This process's reported version; [`UNSTAMPED_VERSION`] until set.
pub fn own_version() -> &'static str {
    OWN_VERSION.get().copied().unwrap_or(UNSTAMPED_VERSION)
}

/// The version token of a `kb-tap --version` line (`kb-tap <version>`).
pub fn tap_line_version(line: &str) -> Option<&str> {
    let v = line.trim().strip_prefix("kb-tap")?.trim();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_loses_its_v_and_a_main_build_keeps_its_name() {
        assert_eq!(normalize_stamp_version("v0.52.1"), "0.52.1");
        assert_eq!(normalize_stamp_version("0.52.1"), "0.52.1");
        assert_eq!(
            normalize_stamp_version("main-0123456789ab"),
            "main-0123456789ab"
        );
        assert_eq!(normalize_stamp_version(" v1.2 "), "1.2");
        // `v` not followed by a digit is part of the name.
        assert_eq!(normalize_stamp_version("vx"), "vx");
    }

    #[test]
    fn an_empty_or_missing_stamp_is_the_dev_marker_never_a_bare_zero() {
        assert_eq!(stamped_version(None), UNSTAMPED_VERSION);
        assert_eq!(stamped_version(Some("")), UNSTAMPED_VERSION);
        assert_eq!(stamped_version(Some("  ")), UNSTAMPED_VERSION);
        assert_ne!(UNSTAMPED_VERSION, "0.0.0");
        assert_eq!(stamped_version(Some("v0.52.1")), "0.52.1");
    }

    #[test]
    fn the_tap_line_version_is_the_token_after_the_name() {
        assert_eq!(tap_line_version("kb-tap 0.52.1"), Some("0.52.1"));
        assert_eq!(tap_line_version("kb-tap main-abc\n"), Some("main-abc"));
        assert_eq!(tap_line_version("kb-tap"), None);
        assert_eq!(tap_line_version("other 1.0"), None);
    }
}
