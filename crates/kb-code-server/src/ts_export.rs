//! V76-R4a — ts-rs export gate for kb-code wire types.
//!
//! The shipping binary never enables `ts-export`. `just gen-ts-code` runs
//! `cargo test -p kb-code-server --features ts-export export_bindings`,
//! which is the same `#[ts(export)]` test ts-rs injects for kb-server.
//!
//! Two pins live here:
//!
//! 1. A source scan (always compiled) that every `skip_serializing_if`
//!    field on an exported struct also carries `ts(optional)`, so a field
//!    that is ABSENT on the wire cannot silently regenerate as required.
//! 2. Under `ts-export`, a golden over three representative structs that
//!    FAILS if those fields emit as non-optional in the TypeScript decl.

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    /// Files this unit actually exports. Brief-named modules that do not
    /// exist on this tree (`prose_refs.rs`, `refs_typeahead.rs`,
    /// `compare_file.rs`, `worktrees.rs`) are recorded as mismatches, not
    /// scanned.
    const COVERED: &[&str] = &[
        "src/highlight.rs",
        "src/frames.rs",
        "src/lanes/mod.rs",
        "src/lanes/routes.rs",
        "src/reviews.rs",
        "src/review_doc/mod.rs",
        "src/review_doc/lint.rs",
        "src/review_doc/cards.rs",
        "src/review_doc/routes.rs",
        "src/github.rs",
        "src/workspace.rs",
        "src/claims.rs",
    ];

    fn crate_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    /// A `skip_serializing_if` field on a `#[ts(export)]` struct without a
    /// matching `ts(optional)` would regenerate as a required TS field
    /// (ts-rs only auto-optionalises skip_serializing_if when serde
    /// `default` is also present). That is the optionality drift this
    /// unit exists to stop.
    #[test]
    fn skip_serializing_if_on_exported_structs_is_marked_ts_optional() {
        let mut failures: Vec<String> = Vec::new();
        for rel in COVERED {
            let path = crate_root().join(rel);
            let src = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            let mut struct_exported = false;
            // Set by the `ts(export)` derive line, consumed by the NEXT
            // struct/enum header — so an unexported struct that follows an
            // exported one is never scanned as exported.
            let mut pending_export = false;
            let mut field_has_skip = false;
            let mut field_has_optional = false;
            let mut pending_skip_line: Option<usize> = None;
            for (idx, line) in src.lines().enumerate() {
                let n = idx + 1;
                let trimmed = line.trim();
                if trimmed.starts_with("pub struct ")
                    || trimmed.starts_with("pub enum ")
                    || trimmed.starts_with("struct ")
                    || trimmed.starts_with("enum ")
                {
                    struct_exported = pending_export;
                    pending_export = false;
                    field_has_skip = false;
                    field_has_optional = false;
                    pending_skip_line = None;
                    continue;
                }
                if trimmed.starts_with("impl ") || trimmed.starts_with("pub fn ") {
                    struct_exported = false;
                    field_has_skip = false;
                    field_has_optional = false;
                    pending_skip_line = None;
                    continue;
                }
                if trimmed.contains("ts(export)") && trimmed.contains("derive") {
                    pending_export = true;
                }
                if line == "}" {
                    // End of a top-level item: nothing after it is exported
                    // until another `ts(export)` derive says so.
                    struct_exported = false;
                }
                if trimmed.contains("skip_serializing_if") {
                    // Only an EXPORTED struct's fields are held to the rule;
                    // a stray skip attr elsewhere is not a binding defect.
                    if struct_exported && field_has_skip && !field_has_optional {
                        if let Some(ln) = pending_skip_line {
                            failures.push(format!(
                                "{rel}:{ln}: skip_serializing_if without ts(optional)"
                            ));
                        }
                    }
                    field_has_skip = true;
                    field_has_optional = false;
                    pending_skip_line = Some(n);
                }
                if trimmed.contains("ts(optional)") {
                    field_has_optional = true;
                }
                let is_field = struct_exported
                    && (trimmed.starts_with("pub ") || trimmed.starts_with("pub("))
                    && trimmed.contains(':')
                    && !trimmed.starts_with("pub struct")
                    && !trimmed.starts_with("pub enum")
                    && !trimmed.starts_with("pub fn")
                    && !trimmed.starts_with("pub async")
                    && !trimmed.starts_with("pub const")
                    && !trimmed.starts_with("pub type");
                if is_field {
                    if field_has_skip && !field_has_optional {
                        let ln = pending_skip_line.unwrap_or(n);
                        failures.push(format!(
                            "{rel}:{ln}: skip_serializing_if field `{trimmed}` is missing ts(optional)"
                        ));
                    }
                    field_has_skip = false;
                    field_has_optional = false;
                    pending_skip_line = None;
                }
            }
        }
        assert!(
            failures.is_empty(),
            "exported skip_serializing_if fields must carry ts(optional) \
             so the generated TS is `field?: T` (absent on the wire), not \
             `field: T | null`:\n{}",
            failures.join("\n")
        );
    }

    #[cfg(feature = "ts-export")]
    #[test]
    fn export_bindings_skip_serializing_if_fields_emit_as_optional() {
        use ts_rs::TS;

        let cfg = ts_rs::Config::from_env();
        let lint = crate::review_doc::lint::LintRow::decl(&cfg);
        assert!(
            lint.contains("line?:"),
            "LintRow.line must be optional:\n{lint}"
        );
        assert!(
            lint.contains("ref?:"),
            "LintRow.ref must be optional:\n{lint}"
        );
        assert!(
            lint.contains("candidates?:"),
            "LintRow.candidates must be optional:\n{lint}"
        );
        assert!(
            !lint.contains("message?:"),
            "LintRow.message is required:\n{lint}"
        );

        let brief = crate::review_doc::routes::FindingBrief::decl(&cfg);
        assert!(
            brief.contains("fingerprint?:"),
            "FindingBrief.fingerprint must be optional:\n{brief}"
        );
        assert!(
            brief.contains("superseded_by?:"),
            "FindingBrief.superseded_by must be optional:\n{brief}"
        );
        assert!(
            brief.contains("disposition?:"),
            "FindingBrief.disposition must be optional:\n{brief}"
        );
        assert!(
            !brief.contains("slug?:"),
            "FindingBrief.slug is required:\n{brief}"
        );

        let card = crate::review_doc::cards::Card::decl(&cfg);
        assert!(
            card.contains("trust?:"),
            "Card.trust must be optional:\n{card}"
        );
        assert!(
            card.contains("path?:"),
            "Card.path must be optional:\n{card}"
        );
        assert!(
            card.contains("highlights?:"),
            "Card.highlights must be optional:\n{card}"
        );
        assert!(
            !card.contains("caption?:"),
            "Card.caption is required:\n{card}"
        );

        let code = crate::github::PrMetaUnavailableCode::decl(&cfg);
        assert!(
            code.contains("\"no-credentials\""),
            "PrMetaUnavailableCode must be the kebab-case union:\n{code}"
        );
        assert!(
            code.contains("\"rate-limited\""),
            "PrMetaUnavailableCode must include rate-limited:\n{code}"
        );
    }
}
