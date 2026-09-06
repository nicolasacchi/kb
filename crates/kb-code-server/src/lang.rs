//! Language detection (by file extension, plus a narrow shebang sniff for
//! extensionless scripts) for the six v1 languages — Rust, Python, Ruby
//! (Wave 1) and TypeScript/TSX, JavaScript, Bash, YAML (W2.2) — plus Go,
//! TOML, JSON (W2.6, the "next-tier languages" — operator ruling "plan for
//! it: json, go, toml") — plus the shared tree-sitter parse plumbing
//! `extract.rs`/`highlight.rs` both build on.
//!
//! Every derived row (`symbols`/`highlights`) is keyed by `(blob_hash,
//! salt)` (ADR-2). `salt` bakes in BOTH the grammar crate's exact pinned
//! version (a grammar bump can change node shapes/field names, which would
//! silently corrupt cached rows extracted under the old grammar) AND a
//! bump-able query revision suffix (`+qN` — bump this when
//! `extract.rs`/`highlight.rs`'s OWN mapping logic changes in a way that
//! would change the output for unchanged bytes, e.g. a kind-mapping fix).
//! Bumping either half invalidates every cached row for that language on
//! next ingest — nothing else has to change. `typescript` and `tsx` get
//! DISTINCT salts even though W2.2's vendored query text is byte-identical
//! between them (see `queries/typescript-tags.scm`'s header): they're
//! different compiled `tree_sitter::Language` grammars (different internal
//! symbol/field ids), so a byte-identical `.ts`/`.tsx` pair must never share
//! a cache slot.
//!
//! The salt strings below are hand-pinned against the exact resolved
//! versions in `Cargo.lock` at the time of writing (`tree-sitter-rust
//! 0.24.2`, `tree-sitter-python 0.25.0`, `tree-sitter-ruby 0.23.1`,
//! `tree-sitter-typescript 0.23.2`, `tree-sitter-javascript 0.25.0`,
//! `tree-sitter-bash 0.25.1`, `tree-sitter-yaml 0.7.2`, `tree-sitter-go
//! 0.25.0`, `tree-sitter-toml-ng 0.7.0`, `tree-sitter-json 0.24.8`) —
//! Cargo.toml only pins the minor version, so bump the salt by hand whenever
//! `cargo update` moves one of these crates forward.

use std::path::Path;

/// One supported language: its id (used as the `files.lang` /
/// `symbols.salt`-prefix value) and its cache-key salt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LangInfo {
    pub id: &'static str,
    pub salt: &'static str,
}

pub const RUST: LangInfo = LangInfo {
    id: "rust",
    // +q3: V3.1-H1 call_sites + type_relations derived tables.
    salt: "rust@0.24.2+q3",
};
pub const PYTHON: LangInfo = LangInfo {
    id: "python",
    // +q3: V3.1-H1 call_sites + type_relations derived tables.
    salt: "python@0.25.0+q3",
};
pub const RUBY: LangInfo = LangInfo {
    id: "ruby",
    salt: "ruby@0.23.1+q1",
};
pub const TYPESCRIPT: LangInfo = LangInfo {
    id: "typescript",
    // +q3: V3.1-H1 call_sites + type_relations derived tables.
    salt: "typescript@0.23.2+q3",
};
pub const TSX: LangInfo = LangInfo {
    id: "tsx",
    // +q3: V3.1-H1 call_sites + type_relations derived tables.
    salt: "tsx@0.23.2+q3",
};
pub const JAVASCRIPT: LangInfo = LangInfo {
    id: "javascript",
    salt: "javascript@0.25.0+q1",
};
pub const BASH: LangInfo = LangInfo {
    id: "bash",
    salt: "bash@0.25.1+q1",
};
pub const YAML: LangInfo = LangInfo {
    id: "yaml",
    salt: "yaml@0.7.2+q1",
};
pub const GO: LangInfo = LangInfo {
    id: "go",
    salt: "go@0.25.0+q1",
};
pub const TOML: LangInfo = LangInfo {
    id: "toml",
    salt: "toml@0.7.0+q1",
};
pub const JSON: LangInfo = LangInfo {
    id: "json",
    salt: "json@0.24.8+q1",
};
/// PRR-N3 — ERB (Rails' embedded-template grammar; also covers EJS, unused
/// here). Registered as its own [`LangInfo`] so `files.lang`/cache-key salt
/// bookkeeping is uniform, but deliberately NOT added to
/// [`TOKEN_LEVEL_LANG_IDS`] below: ERB has no `tags.scm`/`highlights.scm` in
/// this crate (`extract::extract_symbols`/`highlight::extract_highlights`
/// both short-circuit to an empty result for `"erb"` — see their own module
/// docs) — no symbols, no occurrences, no highlight spans. `ts_language`
/// DOES register the grammar below: `frameworks::rails::views` parses `.erb`
/// bytes directly (via [`parse`]) to find tag boundaries, independent of the
/// symbols/highlights passes.
pub const ERB: LangInfo = LangInfo {
    id: "erb",
    salt: "erb@0.25.0+q1",
};

/// Detect a language from the file's extension, or — ONLY when there is no
/// extension at all AND `content` is given and starts with `#!` — a narrow
/// bash-family shebang sniff (see [`shebang_is_bash_family`]). `None` means
/// "no grammar for this file" (still gets a `files` row via
/// `ingest::index_file`, tagged `lang = "unknown"`, but no
/// symbols/highlights).
///
/// `content` is `None` at call sites that never had the bytes handy in the
/// original W1.5 shape; both live call sites gained the bytes in W2.2
/// (`ingest::index_file` always has them, `routes::file`/`routes::symbols`
/// read them via `read_repo_file` first) — see those call sites' own
/// comments for why passing `Some(bytes)` there is free (the bytes are
/// already resident, not a second read).
pub fn detect(path: &str, content: Option<&[u8]>) -> Option<LangInfo> {
    match Path::new(path).extension().and_then(|e| e.to_str()) {
        Some("rs") => return Some(RUST),
        Some("py") => return Some(PYTHON),
        Some("rb") => return Some(RUBY),
        Some("ts") | Some("mts") | Some("cts") => return Some(TYPESCRIPT),
        Some("tsx") => return Some(TSX),
        Some("js") | Some("jsx") | Some("mjs") | Some("cjs") => return Some(JAVASCRIPT),
        Some("sh") | Some("bash") => return Some(BASH),
        Some("yml") | Some("yaml") => return Some(YAML),
        Some("go") => return Some(GO),
        Some("toml") => return Some(TOML),
        Some("json") => return Some(JSON),
        // PRR-N3 — `Path::extension()` only ever returns the LAST dotted
        // component, so `.erb`/`.html.erb`/`.turbo_stream.erb`/`.text.erb`
        // all collapse to the same `"erb"` match here; the format the ERB
        // renders (html/turbo_stream/text) is a `frameworks::rails::views`
        // concern (it inspects the FULL filename suffix, e.g.
        // `.turbo_stream.erb`, for `turbo_stream_target` detection), not a
        // `lang::detect` one.
        Some("erb") => return Some(ERB),
        Some(_) => return None,
        None => {}
    }
    let bytes = content?;
    if bytes.starts_with(b"#!") && shebang_is_bash_family(bytes) {
        Some(BASH)
    } else {
        None
    }
}

/// `true` if `bytes`' first line is a `#!` shebang naming a bash-family
/// interpreter (`bash`/`sh`/`dash`/`ksh`), directly (`#!/bin/bash`) or via
/// `env` indirection (`#!/usr/bin/env bash`). Any OTHER interpreter
/// (python/perl/node/ruby/...) returns `false` — extensionless-with-shebang
/// is deliberately BASH-SPECIFIC sniffing (W2.2 scope: Bash is the only
/// language `detect` ever infers without an extension), not a general
/// interpreter-detection facility that would risk misclassifying an
/// extensionless Python/Perl/Node script as Bash.
fn shebang_is_bash_family(bytes: &[u8]) -> bool {
    let line_end = bytes
        .iter()
        .position(|&b| b == b'\n')
        .unwrap_or(bytes.len());
    let Ok(line) = std::str::from_utf8(&bytes[2..line_end]) else {
        return false;
    };
    let mut parts = line.split_whitespace();
    let Some(first) = parts.next() else {
        return false;
    };
    // `#!/usr/bin/env bash` indirection: the real interpreter is the NEXT
    // word, not `env` itself.
    let interpreter = if first == "env" || first.ends_with("/env") {
        parts.next().unwrap_or("")
    } else {
        first
    };
    let name = interpreter.rsplit('/').next().unwrap_or(interpreter);
    matches!(name, "bash" | "sh" | "dash" | "ksh")
}

/// Every registered [`LangInfo`], PRODUCTION-reachable (unlike the
/// `#[cfg(test)]`-only [`ALL_LANG_IDS`] below) — the eleven token/CST
/// languages plus [`ERB`] (parse-only; never actually gets `symbols`/
/// `highlights`/`occurrences` rows, but including its salt in a "every
/// CURRENTLY valid salt" set is harmless, since it can never match a row
/// that exists). V70-A3X: the single source of truth `store::Store`'s
/// stale-salt filtering/sweep builds its "every currently valid salt" set
/// from — salts are globally unique per language (the `"{id}@{version}
/// +qN"` format), so filtering a derived-table row by "is its salt IN this
/// set" is equivalent to "is its salt the CURRENT one for whichever
/// language it names," with no need to also join back through `files.lang`.
pub(crate) const ALL_LANGS: &[LangInfo] = &[
    RUST, PYTHON, RUBY, TYPESCRIPT, TSX, JAVASCRIPT, BASH, YAML, GO, TOML, JSON, ERB,
];

/// Resolve a language id (as stored in `symbols.salt`'s language or
/// `files.lang`) back to its `LangInfo`. `detect` and `for_id` must agree —
/// pinned by the `detect_and_for_id_agree` test below.
pub fn for_id(id: &str) -> Option<LangInfo> {
    match id {
        "rust" => Some(RUST),
        "python" => Some(PYTHON),
        "ruby" => Some(RUBY),
        "typescript" => Some(TYPESCRIPT),
        "tsx" => Some(TSX),
        "javascript" => Some(JAVASCRIPT),
        "bash" => Some(BASH),
        "yaml" => Some(YAML),
        "go" => Some(GO),
        "toml" => Some(TOML),
        "json" => Some(JSON),
        "erb" => Some(ERB),
        _ => None,
    }
}

/// Languages covered by kb-code v2's TOKEN-LEVEL pass (occurrences, symbol
/// signatures, and doc-comment capture): B2 shipped four (Rust, TypeScript,
/// TSX, JavaScript); B5b widens this to all eight tier-1 languages, adding
/// Python, Ruby, Go, and Bash (per-language node-kind tables in
/// `occurrences.rs`'s `is_identifier_like`/`is_def`/`is_within_import` and
/// `extract.rs`'s `body_field_name`/`is_doc_comment`). YAML/TOML/JSON stay
/// OUT (their `symbols.signature`/`.doc` columns and `occurrences` rows are
/// simply never populated) — those three are CST-walk key-path outlines
/// (ADR-7), not "code" in the sense this pass models (no functions/
/// identifiers to occur/resolve), so widening to them would be meaningless,
/// not merely unmeasured. Single source of truth: gates `extract::
/// extract_symbols`'s signature/doc computation, `occurrences::
/// extract_occurrences`'s own language dispatch, AND `ingest::index_file`'s
/// call site — a language only needs adding here once.
pub const TOKEN_LEVEL_LANG_IDS: &[&str] = &[
    "rust",
    "typescript",
    "tsx",
    "javascript",
    "python",
    "ruby",
    "go",
    "bash",
];

pub fn supports_token_level(id: &str) -> bool {
    TOKEN_LEVEL_LANG_IDS.contains(&id)
}

fn ts_language(id: &str) -> Option<tree_sitter::Language> {
    match id {
        "rust" => Some(tree_sitter_rust::LANGUAGE.into()),
        "python" => Some(tree_sitter_python::LANGUAGE.into()),
        "ruby" => Some(tree_sitter_ruby::LANGUAGE.into()),
        "typescript" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        "javascript" => Some(tree_sitter_javascript::LANGUAGE.into()),
        "bash" => Some(tree_sitter_bash::LANGUAGE.into()),
        "yaml" => Some(tree_sitter_yaml::LANGUAGE.into()),
        "go" => Some(tree_sitter_go::LANGUAGE.into()),
        "toml" => Some(tree_sitter_toml_ng::LANGUAGE.into()),
        "json" => Some(tree_sitter_json::LANGUAGE.into()),
        "erb" => Some(tree_sitter_embedded_template::LANGUAGE.into()),
        _ => None,
    }
}

/// Vendored TypeScript tags query — see `queries/typescript-tags.scm`'s
/// header for why this is hand-written rather than the crate's own bundled
/// (signatures-only) `TAGS_QUERY`.
const TYPESCRIPT_TAGS_QUERY: &str = include_str!("../queries/typescript-tags.scm");
/// Vendored TSX tags query — byte-identical pattern set to
/// `TYPESCRIPT_TAGS_QUERY`, kept as its own file/const (see
/// `queries/tsx-tags.scm`'s header).
const TSX_TAGS_QUERY: &str = include_str!("../queries/tsx-tags.scm");
/// Vendored Bash tags query — tree-sitter-bash ships no `tags.scm` at all
/// (see `queries/bash-tags.scm`'s header).
const BASH_TAGS_QUERY: &str = include_str!("../queries/bash-tags.scm");
/// Vendored Go tags query (W2.6) — the official bundled one exists but
/// doesn't wrap `const`/`var` declarations in a `@definition.*` capture; see
/// `queries/go-tags.scm`'s header for exactly what's carried over verbatim
/// vs. added.
const GO_TAGS_QUERY: &str = include_str!("../queries/go-tags.scm");

/// Vendored locals queries (V3.G1) — tree-sitter locals convention
/// (`@local.scope` / `@local.definition*` / `@local.reference`). Only the
/// four proof languages (rust/typescript/tsx/python) ship here; js/ruby/go/
/// bash follow in a later Phase G lane. See each file's header and
/// `crate::locals`.
const RUST_LOCALS_QUERY: &str = include_str!("../queries/rust-locals.scm");
const TYPESCRIPT_LOCALS_QUERY: &str = include_str!("../queries/typescript-locals.scm");
const TSX_LOCALS_QUERY: &str = include_str!("../queries/tsx-locals.scm");
const PYTHON_LOCALS_QUERY: &str = include_str!("../queries/python-locals.scm");

/// The locals/scope query for `id` — vendored `*-locals.scm` for the four
/// V3.G1 proof languages, `None` for every other language (including the
/// remaining token-level languages whose locals.scm comes later). Used by
/// `crate::locals::bind_locals` and registered the same way as
/// [`tags_query`]'s vendored arm.
pub fn locals_query(id: &str) -> Option<&'static str> {
    match id {
        "rust" => Some(RUST_LOCALS_QUERY),
        "typescript" => Some(TYPESCRIPT_LOCALS_QUERY),
        "tsx" => Some(TSX_LOCALS_QUERY),
        "python" => Some(PYTHON_LOCALS_QUERY),
        _ => None,
    }
}

/// The tags/definition query for `id` — the OFFICIAL bundled `tags.scm`
/// (loaded via the grammar crate's own `TAGS_QUERY` const, never a
/// filesystem path) for Rust/Python/Ruby/JavaScript, or one of this crate's
/// OWN vendored queries above for TypeScript/TSX/Bash/Go. `None` for
/// `"yaml"`/`"toml"`/`"json"`: none of the three is a tags language at all
/// (ADR-7 for YAML, same reasoning extended to TOML/JSON in W2.6) —
/// `extract::extract_symbols` dispatches each to the `yaml`/`keypath`
/// modules' CST-walk outlines instead of this query path; see those
/// modules' docs.
pub fn tags_query(id: &str) -> Option<&'static str> {
    match id {
        "rust" => Some(tree_sitter_rust::TAGS_QUERY),
        "python" => Some(tree_sitter_python::TAGS_QUERY),
        "ruby" => Some(tree_sitter_ruby::TAGS_QUERY),
        "javascript" => Some(tree_sitter_javascript::TAGS_QUERY),
        "typescript" => Some(TYPESCRIPT_TAGS_QUERY),
        "tsx" => Some(TSX_TAGS_QUERY),
        "bash" => Some(BASH_TAGS_QUERY),
        "go" => Some(GO_TAGS_QUERY),
        _ => None,
    }
}

/// The highlight query for `id`. For six of the eight languages this is the
/// OFFICIAL bundled `highlights.scm` verbatim (same sourcing rule as
/// `tags_query`). TypeScript/TSX are the exception: the
/// `tree-sitter-typescript` crate's own `HIGHLIGHTS_QUERY` is a TS-only
/// DELTA (type annotations + TS-only keywords — no strings/comments/
/// functions/numbers of its own; it's designed to run ALONGSIDE
/// `tree-sitter-javascript`'s query, the same layering convention editors
/// like nvim-treesitter use for this exact grammar pair), so this
/// concatenates JavaScript's base query first, then TypeScript's delta
/// second — `highlight.rs`'s overlap-resolution sweep keeps the LAST
/// capture for an exact same-span duplicate, so the more specific TS-delta
/// pattern (e.g. a capitalized identifier reclassified from `variable` to
/// `type`) wins over JS's generic one. Returns an owned `String` (rather
/// than `&'static str`, unlike `tags_query`) purely because of this one
/// concatenation case; the other six languages allocate one `.to_string()`
/// copy of an already-`'static` slice, which is cheap and happens once per
/// file ingest, not in a hot loop.
pub fn highlights_query(id: &str) -> Option<String> {
    match id {
        "rust" => Some(tree_sitter_rust::HIGHLIGHTS_QUERY.to_string()),
        "python" => Some(tree_sitter_python::HIGHLIGHTS_QUERY.to_string()),
        "ruby" => Some(tree_sitter_ruby::HIGHLIGHTS_QUERY.to_string()),
        "javascript" => Some(tree_sitter_javascript::HIGHLIGHT_QUERY.to_string()),
        "typescript" | "tsx" => Some(format!(
            "{}\n{}",
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_typescript::HIGHLIGHTS_QUERY
        )),
        "bash" => Some(tree_sitter_bash::HIGHLIGHT_QUERY.to_string()),
        "yaml" => Some(tree_sitter_yaml::HIGHLIGHTS_QUERY.to_string()),
        "go" => Some(tree_sitter_go::HIGHLIGHTS_QUERY.to_string()),
        "toml" => Some(tree_sitter_toml_ng::HIGHLIGHTS_QUERY.to_string()),
        "json" => Some(tree_sitter_json::HIGHLIGHTS_QUERY.to_string()),
        _ => None,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LangError {
    #[error("unsupported language id: {0:?}")]
    Unsupported(String),
    #[error("failed to set tree-sitter language: {0}")]
    Language(String),
    #[error("tree-sitter returned no parse tree")]
    ParseFailed,
    #[error("invalid tree-sitter query for {lang}: {message}")]
    Query { lang: String, message: String },
}

pub type Result<T> = std::result::Result<T, LangError>;

/// Parse `source` with the grammar for `id`. Shared by `extract.rs`
/// (`tags.scm`), `highlight.rs` (`highlights.scm`), and `yaml.rs` (the
/// CST-walk outline) — all three need the same language handle + tree,
/// just different things built on top.
pub fn parse(id: &str, source: &[u8]) -> Result<(tree_sitter::Tree, tree_sitter::Language)> {
    let language = ts_language(id).ok_or_else(|| LangError::Unsupported(id.to_string()))?;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&language)
        .map_err(|e| LangError::Language(e.to_string()))?;
    let tree = parser.parse(source, None).ok_or(LangError::ParseFailed)?;
    Ok((tree, language))
}

/// Compile `query_src` against `language`, tagging any error with `id` so
/// callers don't have to.
pub fn compile_query(
    id: &str,
    language: &tree_sitter::Language,
    query_src: &str,
) -> Result<tree_sitter::Query> {
    tree_sitter::Query::new(language, query_src).map_err(|e| LangError::Query {
        lang: id.to_string(),
        message: e.to_string(),
    })
}

/// Every supported language id — the six v1 languages (TS/TSX counted
/// separately since they're distinct grammars/salts) plus Go/TOML/JSON
/// (W2.6) — eleven total language ids. Shared by this module's own tests
/// and by `extract.rs`/`highlight.rs`'s cross-language fixture loops.
#[cfg(test)]
pub(crate) const ALL_LANG_IDS: &[&str] = &[
    "rust",
    "python",
    "ruby",
    "typescript",
    "tsx",
    "javascript",
    "bash",
    "yaml",
    "go",
    "toml",
    "json",
];

/// Language ids that go through the `tags.scm`-style definition-query path
/// (`extract::extract_symbols`'s generic branch) — every language EXCEPT
/// `"yaml"`/`"toml"`/`"json"`, which are CST-walk outlines instead (see
/// `tags_query`'s doc).
#[cfg(test)]
pub(crate) const TAGS_LANG_IDS: &[&str] = &[
    "rust",
    "python",
    "ruby",
    "typescript",
    "tsx",
    "javascript",
    "bash",
    "go",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_every_v1_extension() {
        assert_eq!(detect("src/lib.rs", None).map(|l| l.id), Some("rust"));
        assert_eq!(detect("pkg/module.py", None).map(|l| l.id), Some("python"));
        assert_eq!(detect("app/model.rb", None).map(|l| l.id), Some("ruby"));
        assert_eq!(detect("src/app.ts", None).map(|l| l.id), Some("typescript"));
        assert_eq!(
            detect("src/app.mts", None).map(|l| l.id),
            Some("typescript")
        );
        assert_eq!(
            detect("src/app.cts", None).map(|l| l.id),
            Some("typescript")
        );
        assert_eq!(detect("src/Widget.tsx", None).map(|l| l.id), Some("tsx"));
        assert_eq!(detect("src/app.js", None).map(|l| l.id), Some("javascript"));
        assert_eq!(
            detect("src/app.jsx", None).map(|l| l.id),
            Some("javascript")
        );
        assert_eq!(
            detect("src/app.mjs", None).map(|l| l.id),
            Some("javascript")
        );
        assert_eq!(
            detect("src/app.cjs", None).map(|l| l.id),
            Some("javascript")
        );
        assert_eq!(detect("bin/run.sh", None).map(|l| l.id), Some("bash"));
        assert_eq!(detect("bin/run.bash", None).map(|l| l.id), Some("bash"));
        assert_eq!(detect("k8s/deploy.yml", None).map(|l| l.id), Some("yaml"));
        assert_eq!(detect("k8s/deploy.yaml", None).map(|l| l.id), Some("yaml"));
        assert_eq!(detect("cmd/main.go", None).map(|l| l.id), Some("go"));
        assert_eq!(detect("Cargo.toml", None).map(|l| l.id), Some("toml"));
        assert_eq!(detect("package.json", None).map(|l| l.id), Some("json"));
        assert_eq!(detect("README.md", None), None);
        assert_eq!(detect("noextension", None), None);
    }

    #[test]
    fn shebang_sniff_only_fires_for_extensionless_bash_family_scripts() {
        assert_eq!(
            detect("bin/tool", Some(b"#!/bin/bash\necho hi\n")).map(|l| l.id),
            Some("bash")
        );
        assert_eq!(
            detect("bin/tool", Some(b"#!/bin/sh\necho hi\n")).map(|l| l.id),
            Some("bash")
        );
        assert_eq!(
            detect("bin/tool", Some(b"#!/usr/bin/env bash\necho hi\n")).map(|l| l.id),
            Some("bash")
        );
        assert_eq!(
            detect("bin/tool", Some(b"#!/usr/bin/env dash\n")).map(|l| l.id),
            Some("bash")
        );
        // A python/perl shebang must NOT be misclassified as bash.
        assert_eq!(detect("bin/tool", Some(b"#!/usr/bin/env python3\n")), None);
        assert_eq!(detect("bin/tool", Some(b"#!/usr/bin/perl\n")), None);
        // No `#!` at all, or no content given: no sniff, no match.
        assert_eq!(detect("bin/tool", Some(b"just some text\n")), None);
        assert_eq!(detect("bin/tool", None), None);
        // An extension always wins over content — no sniff attempted.
        assert_eq!(
            detect("bin/tool.py", Some(b"#!/bin/bash\n")).map(|l| l.id),
            Some("python")
        );
    }

    #[test]
    fn detect_and_for_id_agree() {
        for path in [
            "a.rs", "a.py", "a.rb", "a.ts", "a.tsx", "a.js", "a.sh", "a.yml", "a.go", "a.toml",
            "a.json",
        ] {
            let info = detect(path, None).unwrap();
            assert_eq!(for_id(info.id), Some(info));
        }
    }

    #[test]
    fn all_langs_covers_every_for_id_entry_including_erb() {
        // V70-A3X: `ALL_LANGS` backs the store's stale-salt filtering — it
        // must never silently drop a language `for_id` still resolves (that
        // language's CURRENT rows would then look "stale" and get swept),
        // and every salt in it must be unique (two languages sharing a salt
        // string would break "salt IN (current set) implies current for
        // whichever language it names").
        for id in ALL_LANG_IDS.iter().chain(["erb"].iter()) {
            assert!(
                ALL_LANGS.iter().any(|l| l.id == *id),
                "ALL_LANGS is missing {id:?}"
            );
        }
        let mut salts: Vec<&str> = ALL_LANGS.iter().map(|l| l.salt).collect();
        salts.sort_unstable();
        salts.dedup();
        assert_eq!(salts.len(), ALL_LANGS.len(), "every salt must be unique");
    }

    #[test]
    fn every_supported_language_has_a_grammar_and_a_highlights_query() {
        for id in ALL_LANG_IDS {
            assert!(ts_language(id).is_some(), "{id} language");
            assert!(highlights_query(id).is_some(), "{id} highlights.scm");
        }
        // `tree_sitter::Language` implements neither `Debug` nor
        // `PartialEq`, so `assert_eq!(..., None)` won't compile here —
        // `is_none()` is the only option.
        assert!(ts_language("cobol").is_none());
    }

    #[test]
    fn every_tags_language_has_a_tags_query_yaml_toml_json_have_none() {
        for id in TAGS_LANG_IDS {
            assert!(tags_query(id).is_some(), "{id} tags.scm");
        }
        for id in ["yaml", "toml", "json"] {
            assert!(
                tags_query(id).is_none(),
                "{id} is a CST-walk outline, not a tags language — see extract::extract_symbols"
            );
        }
    }

    #[test]
    fn locals_query_is_exactly_the_four_proof_languages() {
        for id in ["rust", "typescript", "tsx", "python"] {
            assert!(locals_query(id).is_some(), "{id} locals.scm");
        }
        for id in ["javascript", "ruby", "go", "bash", "yaml", "toml", "json"] {
            assert!(
                locals_query(id).is_none(),
                "{id} must not have a locals query yet (V3.G1 proof set only)"
            );
        }
    }

    #[test]
    fn locals_queries_compile_against_their_grammars() {
        for id in ["rust", "typescript", "tsx", "python"] {
            let (_tree, language) = parse(id, b"").unwrap();
            let q = locals_query(id).unwrap();
            compile_query(id, &language, q).unwrap();
        }
    }

    #[test]
    fn supports_token_level_is_exactly_the_eight_tier1_languages() {
        for id in [
            "rust",
            "typescript",
            "tsx",
            "javascript",
            "python",
            "ruby",
            "go",
            "bash",
        ] {
            assert!(supports_token_level(id), "{id} should be token-level");
        }
        for id in ["yaml", "toml", "json"] {
            assert!(
                !supports_token_level(id),
                "{id} should NOT be token-level (CST-walk outlines, not code)"
            );
        }
    }

    #[test]
    fn parse_and_compile_query_succeed_for_every_language() {
        for id in ALL_LANG_IDS {
            let (tree, language) = parse(id, b"").unwrap();
            assert!(!tree.root_node().kind().is_empty());
            let hl = highlights_query(id).unwrap();
            compile_query(id, &language, &hl).unwrap();
            if let Some(tags) = tags_query(id) {
                compile_query(id, &language, tags).unwrap();
            }
        }
    }

    #[test]
    fn typescript_and_tsx_have_distinct_salts() {
        assert_ne!(TYPESCRIPT.salt, TSX.salt);
    }

    /// (this module's `LangInfo`, its grammar crate's package name in
    /// `Cargo.lock`) — one row per language. TypeScript and TSX share ONE
    /// grammar crate (`tree-sitter-typescript`, see the module doc's
    /// "DISTINCT salts" paragraph) so both rows point at the same package
    /// name but assert against their own, different, salt.
    const GRAMMAR_SALT_TABLE: &[(LangInfo, &str)] = &[
        (RUST, "tree-sitter-rust"),
        (PYTHON, "tree-sitter-python"),
        (RUBY, "tree-sitter-ruby"),
        (TYPESCRIPT, "tree-sitter-typescript"),
        (TSX, "tree-sitter-typescript"),
        (JAVASCRIPT, "tree-sitter-javascript"),
        (BASH, "tree-sitter-bash"),
        (YAML, "tree-sitter-yaml"),
        (GO, "tree-sitter-go"),
        (TOML, "tree-sitter-toml-ng"),
        (JSON, "tree-sitter-json"),
    ];

    /// Walk up from `CARGO_MANIFEST_DIR` (this crate's own directory,
    /// `crates/kb-code-server`) looking for the workspace root `Cargo.lock`
    /// — robust to this crate's depth in the tree changing, rather than
    /// hand-pinning `"../../Cargo.lock"`.
    fn workspace_cargo_lock() -> std::path::PathBuf {
        let start = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        start
            .ancestors()
            .map(|dir| dir.join("Cargo.lock"))
            .find(|candidate| candidate.is_file())
            .unwrap_or_else(|| panic!("no Cargo.lock found above {}", start.display()))
    }

    /// Every hand-pinned salt above (see the module doc's "hand-pinned
    /// against the exact resolved versions" paragraph) must embed the
    /// EXACT version `Cargo.lock` currently resolves for that grammar
    /// crate. Catches the silent-staleness footgun the module doc warns
    /// about: `cargo update` moves a tree-sitter grammar forward, node
    /// shapes/field names shift, and nobody remembers to bump the salt —
    /// which would keep serving `symbols`/`highlights` rows cached under
    /// the OLD grammar for every file whose `blob_hash` didn't change.
    #[test]
    fn salts_match_the_resolved_cargo_lock_grammar_versions() {
        let lock_path = workspace_cargo_lock();
        let lock_text = std::fs::read_to_string(&lock_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", lock_path.display()));
        let lock: toml::Value = lock_text
            .parse()
            .unwrap_or_else(|e| panic!("parse {}: {e}", lock_path.display()));
        let packages = lock
            .get("package")
            .and_then(|p| p.as_array())
            .unwrap_or_else(|| panic!("{} has no [[package]] array", lock_path.display()));

        for (info, crate_name) in GRAMMAR_SALT_TABLE {
            let matches: Vec<&str> = packages
                .iter()
                .filter(|p| p.get("name").and_then(|n| n.as_str()) == Some(*crate_name))
                .map(|p| {
                    p.get("version")
                        .and_then(|v| v.as_str())
                        .unwrap_or_else(|| panic!("{crate_name}: [[package]] entry has no version"))
                })
                .collect();
            let resolved = match matches.as_slice() {
                [] => panic!(
                    "{crate_name}: no [[package]] entry in {}",
                    lock_path.display()
                ),
                [v] => *v,
                multiple => panic!(
                    "{crate_name}: {} ambiguous [[package]] entries in {} ({multiple:?}) — \
                     workspace-unify or pick the one this crate actually links",
                    multiple.len(),
                    lock_path.display()
                ),
            };
            let expected_prefix = format!("{}@{resolved}", info.id);
            assert!(
                info.salt.starts_with(&expected_prefix),
                "{}: salt {:?} does not embed Cargo.lock's resolved {crate_name} version \
                 {resolved:?} (expected it to start with {expected_prefix:?}) — bump the \
                 salt's version segment after this cargo update, see the module doc",
                info.id,
                info.salt,
            );
        }
    }
}
