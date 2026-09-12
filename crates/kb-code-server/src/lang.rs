//! Grammar plumbing + per-language cache salts for the six v1 languages — Rust, Python, Ruby
//! (Wave 1) and TypeScript/TSX, JavaScript, Bash, YAML (W2.2) — plus Go,
//! TOML, JSON (W2.6, the "next-tier languages" — operator ruling "plan for
//! it: json, go, toml") — plus the shared tree-sitter parse plumbing
//! `extract.rs`/`highlight.rs` both build on.
//!
//! **Detection itself no longer lives here (V72-H1).** Which file is which
//! language — the extension table, D7's filename-stem table (`Gemfile`,
//! `Rakefile`, `config.ru`, `*.rake`, `*.jbuilder`, `*.gemspec`) and the
//! `#!` interpreter sniff — is one declaration in the `syntax/1` registry
//! (`crate::syntax`), together with the extraction TIER and the Parity
//! Grid derived from it. [`detect`] is a thin façade over that table and
//! its contract is unchanged.
//!
//! Every derived row (`symbols`/`highlights`) is keyed by `(blob_hash,
//! salt)` (ADR-2). A salt bakes in BOTH the grammar crate's exact pinned
//! version (a grammar bump can change node shapes/field names, which would
//! silently corrupt cached rows extracted under the old grammar) AND a
//! bump-able revision suffix for the crate's OWN mapping logic. Bumping
//! either half invalidates every cached row of that FAMILY for that
//! language on next ingest — nothing else has to change. `typescript` and
//! `tsx` get DISTINCT salts even though W2.2's vendored query text is
//! byte-identical between them (see `queries/typescript-tags.scm`'s
//! header): they're different compiled `tree_sitter::Language` grammars
//! (different internal symbol/field ids), so a byte-identical `.ts`/`.tsx`
//! pair must never share a cache slot.
//!
//! ## Two salts, not one (V72-H2b, D7)
//!
//! Until V72-H2b ONE salt keyed every derived family, so a highlight-query
//! or role-table change re-extracted SYMBOLS too (and a `tags.scm` fix
//! re-painted every file). [`LangInfo`] now carries two:
//!
//! - [`LangInfo::symbol_salt`] (`{id}@{grammar}+qN`) — grammar version ×
//!   symbol queries × extractor version. Keys `symbols`, `occurrences`,
//!   `import_specs`, `call_sites`, `type_relations` and the `symbols`
//!   family's `derived_status` marker.
//! - [`LangInfo::highlight_salt`] (`{id}@{grammar}+hN+rolesM`) — grammar
//!   version × highlight queries × the ROLE TABLE version. Keys
//!   `highlights` and the `highlights` family's `derived_status` marker.
//!   `M` is [`crate::highlight::ROLE_TABLE_VERSION`] and is pinned to it by
//!   test: widening the role vocabulary bumps every language's highlight
//!   salt in one edit, and cannot be forgotten for one language.
//!
//! The two salt strings are globally unique across BOTH sets (the `+q` /
//! `+h` suffixes make them structurally unable to collide), which is what
//! keeps `store`'s "is this row's salt in the CURRENT set" filtering a
//! per-FAMILY question with no join back through `files.lang`. A family is
//! stale only when ITS OWN salt moved — see [`SaltFamily`], and
//! `store::Store::sweep_stale_salt_page`, which sweeps each table against
//! the salt set of the family that table belongs to.
//!
//! **A salt bump is a measured decision, not a free one.** Run the
//! re-extract bill (`kb-code reextract --bill`, `crate::reextract`) before
//! shipping one: it prices the bump in files, rows, bytes and a timed
//! sample per language.
//!
//! The salt strings below are hand-pinned against the exact resolved
//! versions in `Cargo.lock` at the time of writing (`tree-sitter-rust
//! 0.24.2`, `tree-sitter-python 0.25.0`, `tree-sitter-ruby 0.23.1`,
//! `tree-sitter-typescript 0.23.2`, `tree-sitter-javascript 0.25.0`,
//! `tree-sitter-bash 0.25.1`, `tree-sitter-yaml 0.7.2`, `tree-sitter-go
//! 0.25.0`, `tree-sitter-toml-ng 0.7.0`, `tree-sitter-json 0.24.8`,
//! `tree-sitter-css 0.25.0`, `tree-sitter-scss 1.0.0`, `tree-sitter-md
//! 0.5.3`) —
//! Cargo.toml only pins the minor version, so bump the salt by hand whenever
//! `cargo update` moves one of these crates forward.

/// Which derived FAMILY a salt keys (V72-H2b, D7). The two families are
/// independently invalidated: bumping one re-extracts only its own tables,
/// and the stale-salt sweep asks this question per table rather than
/// holding one "current salt" set for everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SaltFamily {
    /// `symbols`, `occurrences`, `import_specs`, `call_sites`,
    /// `type_relations` — everything derived from a `tags.scm`-style
    /// definition query or a CST outline walk.
    Symbol,
    /// `highlights` — spans from a `highlights.scm` query (or, for HAML,
    /// the first-party scanner) mapped through the role table.
    Highlight,
}

impl SaltFamily {
    /// The `derived_status.family` value — also what the wire and the CLI
    /// print. Deliberately the TABLE-ish plural, so a reader of a
    /// `derived_status` row does not have to translate.
    pub fn as_str(self) -> &'static str {
        match self {
            SaltFamily::Symbol => "symbols",
            SaltFamily::Highlight => "highlights",
        }
    }

    /// Both families, in wire order.
    pub const ALL: &'static [SaltFamily] = &[SaltFamily::Symbol, SaltFamily::Highlight];
}

/// One supported language: its id (used as the `files.lang` /
/// `symbols.salt`-prefix value) and its two cache-key salts — see the
/// module doc's "Two salts, not one".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LangInfo {
    pub id: &'static str,
    /// Keys every SYMBOL-family derived row. `{id}@{grammar}+qN`.
    pub symbol_salt: &'static str,
    /// Keys every HIGHLIGHT-family derived row.
    /// `{id}@{grammar}+hN+roles{ROLE_TABLE_VERSION}`.
    pub highlight_salt: &'static str,
}

impl LangInfo {
    /// The salt keying `family`'s rows for this language. The ONE place a
    /// family is turned into a salt, so no call site can pick the wrong
    /// one by habit.
    pub fn salt_for(&self, family: SaltFamily) -> &'static str {
        match family {
            SaltFamily::Symbol => self.symbol_salt,
            SaltFamily::Highlight => self.highlight_salt,
        }
    }
}

pub const RUST: LangInfo = LangInfo {
    id: "rust",
    // +q3: V3.1-H1 call_sites + type_relations derived tables.
    symbol_salt: "rust@0.24.2+q3",
    highlight_salt: "rust@0.24.2+h1+roles2",
};
pub const PYTHON: LangInfo = LangInfo {
    id: "python",
    // +q3: V3.1-H1 call_sites + type_relations derived tables.
    symbol_salt: "python@0.25.0+q3",
    highlight_salt: "python@0.25.0+h1+roles2",
};
pub const RUBY: LangInfo = LangInfo {
    id: "ruby",
    symbol_salt: "ruby@0.23.1+q1",
    highlight_salt: "ruby@0.23.1+h1+roles2",
};
pub const TYPESCRIPT: LangInfo = LangInfo {
    id: "typescript",
    // +q3: V3.1-H1 call_sites + type_relations derived tables.
    symbol_salt: "typescript@0.23.2+q3",
    highlight_salt: "typescript@0.23.2+h1+roles2",
};
pub const TSX: LangInfo = LangInfo {
    id: "tsx",
    // +q3: V3.1-H1 call_sites + type_relations derived tables.
    symbol_salt: "tsx@0.23.2+q3",
    highlight_salt: "tsx@0.23.2+h1+roles2",
};
pub const JAVASCRIPT: LangInfo = LangInfo {
    id: "javascript",
    symbol_salt: "javascript@0.25.0+q1",
    highlight_salt: "javascript@0.25.0+h1+roles2",
};
pub const BASH: LangInfo = LangInfo {
    id: "bash",
    symbol_salt: "bash@0.25.1+q1",
    highlight_salt: "bash@0.25.1+h1+roles2",
};
pub const YAML: LangInfo = LangInfo {
    id: "yaml",
    // +q2: V72-H2a (D7) surfaces anchors (`&a`), aliases (`*a`) and merge
    // keys (`<<:`) on the key row's `signature` — same rows, new field, so
    // every cached row extracted under +q1 is stale by definition.
    symbol_salt: "yaml@0.7.2+q2",
    highlight_salt: "yaml@0.7.2+h1+roles2",
};
pub const GO: LangInfo = LangInfo {
    id: "go",
    symbol_salt: "go@0.25.0+q1",
    highlight_salt: "go@0.25.0+h1+roles2",
};
pub const TOML: LangInfo = LangInfo {
    id: "toml",
    symbol_salt: "toml@0.7.0+q1",
    highlight_salt: "toml@0.7.0+h1+roles2",
};
pub const JSON: LangInfo = LangInfo {
    id: "json",
    symbol_salt: "json@0.24.8+q1",
    highlight_salt: "json@0.24.8+h1+roles2",
};
/// V72-H2a (D7) — CSS. Not a tags language (no `tags.scm` upstream, and no
/// function/class vocabulary to tag); `crate::css` walks the CST into a
/// STYLESHEET outline (rules, at-rules, custom properties), the same model
/// `yaml`/`keypath` use for data files.
pub const CSS: LangInfo = LangInfo {
    id: "css",
    symbol_salt: "css@0.25.0+q1",
    highlight_salt: "css@0.25.0+h1+roles2",
};
/// V72-H2a (D7) — SCSS. A DIFFERENT compiled grammar from [`CSS`] (its own
/// symbol/field ids), so it gets its own salt for the same reason
/// `typescript` and `tsx` do: a byte-identical `.css`/`.scss` pair must
/// never share a cache slot.
pub const SCSS: LangInfo = LangInfo {
    id: "scss",
    symbol_salt: "scss@1.0.0+q1",
    highlight_salt: "scss@1.0.0+h1+roles2",
};
/// V72-H2a (D7) — Markdown, via `tree-sitter-md`'s BLOCK grammar. The
/// crate ships two grammars (block structure + inline content); kb-code
/// registers the block one, whose `section`/`atx_heading` nodes are the
/// heading outline and whose `fenced_code_block`s are this language's
/// injection regions. Inline emphasis/link highlighting is NOT claimed —
/// see `highlights_query`'s own arm.
pub const MARKDOWN: LangInfo = LangInfo {
    id: "markdown",
    symbol_salt: "markdown@0.5.3+q1",
    highlight_salt: "markdown@0.5.3+h1+roles2",
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
    symbol_salt: "erb@0.25.0+q1",
    highlight_salt: "erb@0.25.0+h1+roles2",
};

/// V72-H3 — HAML. The ONE language whose rows are derived by a FIRST-PARTY
/// scanner (`crate::haml`) rather than a tree-sitter grammar: no viable
/// HAML grammar exists, and D7's ruling is to own the scanner rather than
/// rest a "no hard stops" instrument on a 13-star dependency. Its
/// `syntax/1` row therefore carries `Engine::Scanner`, and
/// [`ts_language`] deliberately has NO arm for it — `lang::parse("haml",
/// …)` is `Unsupported`, which is the honest answer and which every
/// caller already degrades on.
///
/// The salt's version component is the SCANNER's version
/// (`crate::haml::SCANNER_VERSION`), not a grammar's, and bumping one
/// means bumping the other — the same contract a grammar bump has, applied
/// to code this crate owns.
pub const HAML: LangInfo = LangInfo {
    id: "haml",
    symbol_salt: "haml@haml/1+q1",
    highlight_salt: "haml@haml/1+h1+roles2",
};

/// Detect a language from `path` (and, for an extensionless file with a
/// `#!` line, `content`) — a thin façade over the `syntax/1` registry
/// (`crate::syntax`), which owns the extension table, D7's filename-stem
/// table and the interpreter table as ONE declaration.
///
/// The contract is load-bearing: `Some` means "something in this build
/// PARSES this file, and `salt` keys its derived rows". V72-H1 could say
/// "a tree-sitter grammar" there; V72-H3 widens it by exactly one engine
/// (`syntax::Engine::Scanner` — HAML, parsed by `crate::haml`), so a
/// caller that reads `Some` and then reaches for `lang::parse` must handle
/// its `Unsupported` — every one already does, since that is also what an
/// unregistered id returns. A registry row nothing parses (`sql`,
/// `dockerfile` — named so the gap is visible on `GET /api/syntax` and the
/// Parity Grid) is invisible here, exactly as it was before V72-H1; reach
/// for `syntax::row_for_path` when you want the row rather than the
/// parser. `None` still means "nothing parses this file" (a `files` row
/// via `ingest::index_file`, tagged `lang = "unknown"`, but no symbols or
/// highlights).
///
/// `content` is `None` at call sites that never had the bytes handy in the
/// original W1.5 shape; both live call sites gained the bytes in W2.2
/// (`ingest::index_file` always has them, `routes::file`/`routes::symbols`
/// read them via `read_repo_file` first) — see those call sites' own
/// comments for why passing `Some(bytes)` there is free (the bytes are
/// already resident, not a second read).
pub fn detect(path: &str, content: Option<&[u8]>) -> Option<LangInfo> {
    crate::syntax::row_for_path(path, content).and_then(|row| row.info)
}

/// Every registered [`LangInfo`], PRODUCTION-reachable (unlike the
/// `#[cfg(test)]`-only [`ALL_LANG_IDS`] below) — the eleven token/CST
/// languages plus [`ERB`] (parse-only; never actually gets `symbols`/
/// `highlights`/`occurrences` rows, but including its salt in a "every
/// CURRENTLY valid salt" set is harmless, since it can never match a row
/// that exists). V70-A3X: the single source of truth `store::Store`'s
/// stale-salt filtering/sweep builds its "every currently valid salt" set
/// from — salts are globally unique per language AND per family (the
/// `"{id}@{version}+qN"` / `"{id}@{version}+hN+rolesM"` formats), so
/// filtering a derived-table row by "is its salt IN this family's set" is
/// equivalent to "is its salt the CURRENT one for whichever language it
/// names," with no need to also join back through `files.lang`.
/// V72-H2b: reach for [`current_salts`] rather than mapping this list
/// yourself — a family's set is the only correct input to a
/// stale-salt filter, and mapping `symbol_salt` over a HIGHLIGHT read
/// would declare every current highlight row stale.
pub(crate) const ALL_LANGS: &[LangInfo] = &[
    RUST, PYTHON, RUBY, TYPESCRIPT, TSX, JAVASCRIPT, BASH, YAML, GO, TOML, JSON, CSS, SCSS,
    MARKDOWN, ERB, HAML,
];

/// Every CURRENTLY valid salt for `family`, one per registered language —
/// the single input to `store`'s stale-salt filtering and to the V72-B0
/// sweep. Sorted output is NOT promised (callers that persist a
/// fingerprint sort it themselves, `lib::salt_set_fingerprint`).
pub(crate) fn current_salts(family: SaltFamily) -> Vec<&'static str> {
    ALL_LANGS.iter().map(|l| l.salt_for(family)).collect()
}

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
        "css" => Some(CSS),
        "scss" => Some(SCSS),
        "markdown" => Some(MARKDOWN),
        "erb" => Some(ERB),
        "haml" => Some(HAML),
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
        "css" => Some(tree_sitter_css::LANGUAGE.into()),
        // `tree-sitter-scss` 1.0.0 is the ONE grammar here with an
        // OLD-STYLE binding: it returns a `tree_sitter::Language` by value
        // instead of the ABI-stable `LanguageFn` every other crate exposes.
        // That is safe only while the workspace resolves ONE `tree-sitter`
        // (its own requirement is a wide `>=0.21.0`) — if a `cargo update`
        // ever split it, this line stops compiling, which is the failure
        // mode we want. See the root Cargo.toml comment.
        "scss" => Some(tree_sitter_scss::language()),
        "markdown" => Some(tree_sitter_md::LANGUAGE.into()),
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
/// (`@local.scope` / `@local.definition*` / `@local.reference`, plus
/// V71-E1's `@local.scope.isolated` barrier capture). The four V3.G1 proof
/// languages (rust/typescript/tsx/python) ship here; js/go/bash follow in a
/// later Phase G lane. See each file's header and `crate::locals`.
const RUST_LOCALS_QUERY: &str = include_str!("../queries/rust-locals.scm");
const TYPESCRIPT_LOCALS_QUERY: &str = include_str!("../queries/typescript-locals.scm");
const TSX_LOCALS_QUERY: &str = include_str!("../queries/tsx-locals.scm");
const PYTHON_LOCALS_QUERY: &str = include_str!("../queries/python-locals.scm");
/// V71-E1 — Ruby's locals query is READ-TIME ONLY: it is deliberately NOT
/// in `locals::LOCALS_LANG_IDS` (the persisted `local_def_ordinal` stamping
/// set), so the occurrences ingest pass is byte-identical and no salt bump
/// / re-extract is owed. `usages/2` binds Ruby locals per request instead
/// (`intel::ruby_strict`). See the file's own header.
const RUBY_LOCALS_QUERY: &str = include_str!("../queries/ruby-locals.scm");

/// The locals/scope query for `id` — vendored `*-locals.scm` for the four
/// V3.G1 proof languages plus V71-E1's read-time-only Ruby query, `None`
/// for every other language (including the remaining token-level languages
/// whose locals.scm comes later). Used by `crate::locals::bind_locals` and
/// registered the same way as [`tags_query`]'s vendored arm.
///
/// **A query here is NOT the same set as `locals::supports`** — that one
/// gates the INGEST-time `local_def_ordinal` stamping and stays at the four
/// proof languages; Ruby is bound per request. Adding a language here alone
/// is free; adding it to `locals::supports` costs a salt bump.
pub fn locals_query(id: &str) -> Option<&'static str> {
    match id {
        "rust" => Some(RUST_LOCALS_QUERY),
        "typescript" => Some(TYPESCRIPT_LOCALS_QUERY),
        "tsx" => Some(TSX_LOCALS_QUERY),
        "python" => Some(PYTHON_LOCALS_QUERY),
        "ruby" => Some(RUBY_LOCALS_QUERY),
        _ => None,
    }
}

/// The tags/definition query for `id` — the OFFICIAL bundled `tags.scm`
/// (loaded via the grammar crate's own `TAGS_QUERY` const, never a
/// filesystem path) for Rust/Python/Ruby/JavaScript, or one of this crate's
/// OWN vendored queries above for TypeScript/TSX/Bash/Go. `None` for
/// the six CST-WALK OUTLINE languages (`extract::CST_OUTLINES` —
/// `"yaml"`/`"toml"`/`"json"`, plus V72-H2a's `"css"`/`"scss"`/
/// `"markdown"`): none of them is a tags language at all (ADR-7 for YAML,
/// the same reasoning extended to TOML/JSON in W2.6 and to stylesheets/
/// prose in V72-H2a) — `extract::extract_symbols` dispatches each to the
/// `yaml`/`keypath`/`css`/`markdown` modules' CST-walk outlines instead of
/// this query path; see those modules' docs.
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
        "css" => Some(tree_sitter_css::HIGHLIGHTS_QUERY.to_string()),
        "scss" => Some(tree_sitter_scss::HIGHLIGHTS_QUERY.to_string()),
        // The BLOCK grammar's query only. `tree-sitter-md` also ships
        // `HIGHLIGHT_QUERY_INLINE`, which compiles against the SEPARATE
        // inline grammar — running it here would be a query/tree mismatch,
        // and registering `markdown_inline` as its own `syntax/1` row would
        // declare a language no path can address. Emphasis, link text and
        // inline code spans are therefore NOT painted; the fenced-code
        // INJECTIONS (`crate::injection`) are, which is the part that
        // carries real code.
        "markdown" => Some(tree_sitter_md::HIGHLIGHT_QUERY_BLOCK.to_string()),
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
    "css",
    "scss",
    "markdown",
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

    // V72-H1 — the detection TABLE (extensions, D7's filename stems, the
    // `#!` interpreter sniff) moved to the `syntax/1` registry and is
    // pinned there: `syntax::tests::
    // detection_is_byte_identical_for_every_pre_v72_h1_extension` carries
    // the pre-V72-H1 pairs this module used to assert, plus the stem and
    // shebang tables the registry adds. What stays here is the part that
    // is about `LangInfo` itself.

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
    fn all_langs_covers_every_for_id_entry_including_erb_and_haml() {
        // V70-A3X: `ALL_LANGS` backs the store's stale-salt filtering — it
        // must never silently drop a language `for_id` still resolves (that
        // language's CURRENT rows would then look "stale" and get swept),
        // and every salt in it must be unique (two languages sharing a salt
        // string would break "salt IN (current set) implies current for
        // whichever language it names").
        // `erb` (parse-only grammar) and `haml` (first-party scanner) are
        // not in `ALL_LANG_IDS` — neither has a highlights query, which is
        // what that list asserts — but both have live salts the sweep must
        // treat as current.
        for id in ALL_LANG_IDS.iter().chain(["erb", "haml"].iter()) {
            assert!(
                ALL_LANGS.iter().any(|l| l.id == *id),
                "ALL_LANGS is missing {id:?}"
            );
        }
        // V72-H2b: uniqueness must hold across BOTH families at once —
        // the sweep filters `salt IN (this family's set)`, so a string
        // appearing in both sets (or twice in one) would make "current for
        // family F" ambiguous.
        let mut salts: Vec<&str> = ALL_LANGS
            .iter()
            .flat_map(|l| [l.symbol_salt, l.highlight_salt])
            .collect();
        salts.sort_unstable();
        salts.dedup();
        assert_eq!(
            salts.len(),
            ALL_LANGS.len() * 2,
            "every salt must be unique across both families"
        );
    }

    /// V72-H2b — the split's own contract, stated as three assertions
    /// rather than left to the reader of two string tables.
    #[test]
    fn the_two_salt_families_are_disjoint_and_role_versioned() {
        let roles = format!("+roles{}", crate::highlight::ROLE_TABLE_VERSION);
        for l in ALL_LANGS {
            assert_ne!(
                l.symbol_salt, l.highlight_salt,
                "{}: the two families must never share a cache slot",
                l.id
            );
            assert!(
                l.symbol_salt.contains("+q"),
                "{}: a symbol salt carries the `+qN` query revision, got {:?}",
                l.id,
                l.symbol_salt
            );
            // The whole point of encoding the ROLE TABLE version in every
            // highlight salt: widening `highlight::HighlightClass` is one
            // edit that invalidates every language's painted rows, and
            // cannot be forgotten for one of them.
            assert!(
                l.highlight_salt.ends_with(&roles),
                "{}: highlight salt {:?} does not end with the current role-table version                  {roles:?} — bump every highlight salt in the same edit that widens                  `highlight::HighlightClass`",
                l.id,
                l.highlight_salt
            );
            assert_eq!(l.salt_for(SaltFamily::Symbol), l.symbol_salt);
            assert_eq!(l.salt_for(SaltFamily::Highlight), l.highlight_salt);
        }
        for family in SaltFamily::ALL {
            assert_eq!(current_salts(*family).len(), ALL_LANGS.len());
        }
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
        for id in ["yaml", "toml", "json", "css", "scss", "markdown"] {
            assert!(
                tags_query(id).is_none(),
                "{id} is a CST-walk outline, not a tags language — see extract::extract_symbols"
            );
        }
    }

    #[test]
    fn locals_query_is_the_four_proof_languages_plus_read_time_ruby() {
        for id in ["rust", "typescript", "tsx", "python", "ruby"] {
            assert!(locals_query(id).is_some(), "{id} locals.scm");
        }
        for id in [
            "javascript",
            "go",
            "bash",
            "yaml",
            "toml",
            "json",
            "css",
            "scss",
            "markdown",
        ] {
            assert!(
                locals_query(id).is_none(),
                "{id} must not have a locals query yet (V3.G1 proof set + V71-E1 ruby)"
            );
        }
        // V71-E1: having a query is NOT the same as being stamped at
        // ingest — Ruby is read-time only, and adding it to
        // `locals::supports` would owe a ruby salt bump + re-extract.
        assert!(
            !crate::locals::supports("ruby"),
            "ruby locals are read-time only — see queries/ruby-locals.scm's header"
        );
    }

    #[test]
    fn locals_queries_compile_against_their_grammars() {
        for id in ["rust", "typescript", "tsx", "python", "ruby"] {
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
        for id in ["yaml", "toml", "json", "css", "scss", "markdown"] {
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
        assert_ne!(TYPESCRIPT.symbol_salt, TSX.symbol_salt);
        assert_ne!(TYPESCRIPT.highlight_salt, TSX.highlight_salt);
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
        (CSS, "tree-sitter-css"),
        (SCSS, "tree-sitter-scss"),
        // The crate is `tree-sitter-md`; the LANGUAGE id is `markdown`
        // (what a reader types, and what `files.lang` carries), so the
        // salt's prefix is `markdown@` while the version it must embed is
        // `tree-sitter-md`'s.
        (MARKDOWN, "tree-sitter-md"),
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
        // toml 1.x: `Value::from_str` parses a single VALUE; a document is a
        // `Table` (V76-R4e).
        let lock: toml::Table = lock_text
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
            // V72-H2b: BOTH salts embed the grammar version, and both must
            // be bumped by a `cargo update` — checking only one would let
            // half the derived rows go on being served under a grammar
            // that no longer exists in this build.
            for (family, salt) in [
                ("symbol", info.symbol_salt),
                ("highlight", info.highlight_salt),
            ] {
                assert!(
                    salt.starts_with(&expected_prefix),
                    "{}: {family} salt {salt:?} does not embed Cargo.lock's resolved \
                     {crate_name} version {resolved:?} (expected it to start with \
                     {expected_prefix:?}) — bump the salt's version segment after this \
                     cargo update, see the module doc",
                    info.id,
                );
            }
        }
    }
}
