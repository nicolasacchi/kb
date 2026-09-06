//! PRR-N4 — `t("a.b.c")` / `I18n.t(...)` literal keys → the
//! `config/locales/*.yml` file that defines the dotted key path
//! (`EdgeKind::I18nKey`).
//!
//! # Resolution: reuse `yaml::outline`, scan `config/locales/**` directly
//!
//! design-addendum-2.md §G names two options — reuse kb-code's existing
//! YAML key-path index, or (if that's impractical at extract time) scan
//! `config/locales/*.yml` directly with the YAML grammar. The FIRST option
//! would mean querying the `symbols` table for `kind = "key"` rows scoped
//! to this repo's locale files — but every extractor in this lens is a
//! pure `(repo_root, path, bytes) -> Vec<FrameworkEdge>` function with NO
//! `Store`/DB handle (mirrors `views.rs`'s `find_view_files`, which reads
//! the LIVE sibling-file set off disk rather than querying `files`), so
//! threading a `Store` reference into every N4 extractor's signature just
//! for this ONE lane would be a real architectural wart. This module takes
//! the SECOND, cheaper-and-honest path instead: it directly calls
//! `crate::yaml::outline` (the SAME function that populates the `symbols`
//! table for `.yml` files — this is genuinely reusing the extraction
//! LOGIC, just not the stored rows) over every file under
//! `config/locales/**`, building an in-memory `(file, locale-stripped dotted
//! key)` index.
//!
//! # R2 fix (v70-a1): one build per reconcile, not one per dispatched file
//!
//! The index used to be rebuilt (full `fs::read` + `yaml::outline` parse of
//! every locale file) on EVERY call to `extract_from_ruby`/`extract_from_erb`
//! — i.e. once per dispatched file, not once per repo. Measured on the real
//! acme-shop repo (recon rails-lens.md R2): 13 locale files / 388 KB,
//! but **1,337** files dispatch an i18n extractor per full reconcile, so
//! that "locale trees are small" reasoning (true of the file COUNT) hid a
//! ~1,337× repetition factor — ~519 MB of YAML re-read and re-parsed per
//! reconcile. [`locale_index_for`] now fronts [`build_locale_index`] with a
//! process-local cache keyed by `repo_root`, invalidated by a cheap
//! FINGERPRINT of `config/locales/**` (each file's relative path + byte
//! length + mtime — a `read_dir`+`stat` walk, no file content read, no YAML
//! parse). This keeps the extractor functions' signatures pure (still no
//! `Store`/DB handle — the cache lives entirely inside this module, not
//! threaded through any call site) while making a full reconcile build the
//! index once and an edit to a locale file invalidate it on the very next
//! call. Edge OUTPUT is unchanged — this is purely a cost fix, pinned by
//! the existing `tests/rails_lens.rs` goldens plus this module's own
//! `locale_index_is_built_once_across_many_dispatched_files_and_invalidated_on_change`.
//!
//! # Absolute vs. relative keys
//!
//! An ABSOLUTE key (`t("users.show.title")`) is looked up directly (after
//! stripping each locale file's own top-level locale-code segment, e.g.
//! `en.users.show.title` → indexed as `users.show.title`). A RELATIVE key
//! (`t(".title")`, Rails' lazy-lookup convention) is resolved ONLY in ERB
//! view context, through the view's own path
//! (`app/views/users/show.html.erb` → scope `users.show`, a leading `_`
//! partial marker stripped per Rails' own `scope_key_by_partial`) — a
//! relative key encountered OUTSIDE view context (a controller/model/job/
//! mailer calling `t(".x")`, which Rails resolves via A DIFFERENT scope —
//! `controller_path`+action, not the view-path convention) is a
//! documented, honest drop rather than a wrong guess.
//!
//! Ambiguity (the SAME dotted key defined in more than one locale FILE —
//! common when content hasn't been translated to every locale yet, but
//! genuinely ambiguous from this extractor's point of view, which has no
//! notion of "the app's default locale") is `Trust::Candidate`; a
//! non-literal key argument is dropped, never guessed.

use crate::frameworks::rails::support::{
    call_args, call_method_name, literal_string_or_symbol, src_line, walk_calls,
    walk_erb_ruby_fragments,
};
use crate::frameworks::{EdgeKind, FrameworkEdge, Trust};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;
use tree_sitter::Node;

/// `path` is any Ruby file already dispatched here.
pub fn extract_from_ruby(repo_root: &Path, path: &str, bytes: &[u8]) -> Vec<FrameworkEdge> {
    let Ok((tree, _language)) = crate::lang::parse("ruby", bytes) else {
        return Vec::new();
    };
    let index = locale_index_for(repo_root);
    if index.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    walk_calls(
        tree.root_node(),
        bytes,
        0,
        &mut out,
        &mut |node, source, offset| resolve_t_call(node, source, offset, path, &index, None),
    );
    out
}

/// `path` is an ERB file already dispatched here (a view or a component's
/// co-located template).
pub fn extract_from_erb(repo_root: &Path, path: &str, bytes: &[u8]) -> Vec<FrameworkEdge> {
    let Ok((tree, _language)) = crate::lang::parse("erb", bytes) else {
        return Vec::new();
    };
    let index = locale_index_for(repo_root);
    if index.is_empty() {
        return Vec::new();
    }
    let view_scope = view_relative_scope(path);
    let mut out = Vec::new();
    walk_erb_ruby_fragments(
        tree.root_node(),
        bytes,
        &mut out,
        &mut |root, source, offset, out| {
            walk_calls(
                root,
                source,
                offset,
                out,
                &mut |node, source, line_offset| {
                    resolve_t_call(
                        node,
                        source,
                        line_offset,
                        path,
                        &index,
                        view_scope.as_deref(),
                    )
                },
            );
        },
    );
    out
}

/// V72-H3 — the `.haml` sibling of [`extract_from_erb`]. Same locale
/// index, same `resolve_t_call`, same lazy `t(".key")` scope derived from
/// the view path (`view_relative_scope` splits on the FIRST dot, so
/// `orders/show.html.haml` scopes to `orders.show` exactly as the `.erb`
/// form does); only the fragment walk differs.
pub fn extract_from_haml(repo_root: &Path, path: &str, bytes: &[u8]) -> Vec<FrameworkEdge> {
    let index = locale_index_for(repo_root);
    if index.is_empty() {
        return Vec::new();
    }
    let view_scope = view_relative_scope(path);
    let mut out = Vec::new();
    crate::frameworks::rails::support::walk_haml_ruby_fragments(
        bytes,
        &mut out,
        &mut |root, source, offset, out| {
            walk_calls(
                root,
                source,
                offset,
                out,
                &mut |node, source, line_offset| {
                    resolve_t_call(
                        node,
                        source,
                        line_offset,
                        path,
                        &index,
                        view_scope.as_deref(),
                    )
                },
            );
        },
    );
    out
}

fn resolve_t_call(
    node: Node,
    source: &[u8],
    line_offset: u32,
    path: &str,
    index: &[(String, String)],
    view_scope: Option<&str>,
) -> Vec<FrameworkEdge> {
    let Some(method) = call_method_name(node, source) else {
        return Vec::new();
    };
    let receiver_text = node
        .child_by_field_name("receiver")
        .and_then(|r| r.utf8_text(source).ok());
    let is_t_call = matches!(
        (receiver_text, method.as_str()),
        (None, "t") | (None, "translate") | (Some("I18n"), "t") | (Some("I18n"), "translate")
    );
    if !is_t_call {
        return Vec::new();
    }
    let args = call_args(node);
    let Some(first) = args.first() else {
        return Vec::new();
    };
    let Some(raw_key) = literal_string_or_symbol(*first, source) else {
        return Vec::new(); // non-literal — never guess.
    };
    let key = if let Some(rel) = raw_key.strip_prefix('.') {
        let Some(scope) = view_scope else {
            return Vec::new(); // relative key outside view context — documented gap.
        };
        format!("{scope}.{rel}")
    } else {
        raw_key
    };

    let mut files: BTreeSet<&str> = BTreeSet::new();
    for (file, k) in index {
        if k == &key {
            files.insert(file.as_str());
        }
    }
    let files: Vec<String> = files.into_iter().map(|s| s.to_string()).collect();
    let line = src_line(node, line_offset);
    match files.len() {
        0 => Vec::new(),
        1 => vec![FrameworkEdge {
            kind: EdgeKind::I18nKey,
            src_path: path.to_string(),
            src_line: Some(line),
            src_symbol: None,
            dst_kind: Some("locale_file".to_string()),
            dst_path: Some(files.into_iter().next().unwrap()),
            dst_symbol: Some(key),
            trust: Trust::Likely,
            extra_json: None,
        }],
        _ => {
            let cj = files
                .iter()
                .map(|f| format!("\"{f}\""))
                .collect::<Vec<_>>()
                .join(",");
            vec![FrameworkEdge {
                kind: EdgeKind::I18nKey,
                src_path: path.to_string(),
                src_line: Some(line),
                src_symbol: None,
                dst_kind: Some("locale_file".to_string()),
                dst_path: Some(files[0].clone()),
                dst_symbol: Some(key),
                trust: Trust::Candidate,
                extra_json: Some(format!(r#"{{"candidates":[{cj}]}}"#)),
            }]
        }
    }
}

/// `app/views/users/show.html.erb` → `Some("users.show")` (a leading `_`
/// partial marker stripped per Rails' `scope_key_by_partial`, matching
/// `_row.html.erb` → `row`, not `_row`). `None` for a non-`app/views/*`
/// path (a component template — relative-key resolution is deliberately
/// scoped to VIEWS only, see the module doc).
fn view_relative_scope(path: &str) -> Option<String> {
    let rest = path.strip_prefix("app/views/")?;
    let (dir, filename) = match rest.rfind('/') {
        Some(idx) => (&rest[..idx], &rest[idx + 1..]),
        None => ("", rest),
    };
    let stem = filename.split('.').next()?;
    let stem = stem.strip_prefix('_').unwrap_or(stem);
    if stem.is_empty() {
        return None;
    }
    let mut segments: Vec<&str> = if dir.is_empty() {
        Vec::new()
    } else {
        dir.split('/').collect()
    };
    segments.push(stem);
    Some(segments.join("."))
}

/// `(source-relative locale file path, locale-stripped dotted key)` pairs
/// for every key under `config/locales/**/*.{yml,yaml}` — see the module
/// doc for why this is a fresh scan rather than a stored-index query. NOT
/// cached itself — [`locale_index_for`] is the cached front door every real
/// caller should use; this fn is the (expensive) rebuild it fronts.
fn build_locale_index(repo_root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    walk_locale_dir(repo_root, &repo_root.join("config/locales"), &mut out);
    out
}

// ── R2: process-local cache over `build_locale_index` ──────────────────
//
// Keyed by `repo_root` (a repo-scoped cache, mirrors `rails::detect_is_rails`'s
// own per-repo caching precedent — see that fn's doc). A cache HIT still
// costs one cheap filesystem walk (readdir + stat, no file content read, no
// YAML parse) to compute the current fingerprint and compare it; only a
// MISS pays for `build_locale_index`'s full read+parse. Boot walks are a
// single-threaded recursive `walk_dir` (`ingest.rs`) and the live sink
// drains one message at a time (`sink.rs`'s own doc: "awaits each
// `spawn_blocking` before draining the next"), so contention on this
// `Mutex` is never a bottleneck — it exists for correctness (shared mutable
// state), not to gate concurrency.

/// One repo's cached locale index, plus the fingerprint it was built from.
struct CachedLocaleIndex {
    fingerprint: LocaleFingerprint,
    index: Vec<(String, String)>,
    /// V70-A3X — test-only, per-`repo_root` count of how many times
    /// [`build_locale_index`] has ACTUALLY run for this entry (monotonic
    /// across the entry's whole lifetime, including invalidation-triggered
    /// rebuilds — never reset). Replaces a process-global `AtomicUsize`
    /// this module used to keep (`cfg(test)` counter incremented on every
    /// real build, read by `locale_index_is_built_once_across_many_
    /// dispatched_files_and_invalidated_on_change`): under parallel test
    /// execution, ANY other test that dispatches Rails i18n extraction
    /// (this module's own other tests, `rails_lens.rs`, `ingest.rs`)
    /// bumped the SAME global counter concurrently, so "built exactly once
    /// over N calls" could observe foreign builds and flake. Scoping the
    /// count inside the per-root cache entry (already keyed and mutex-
    /// guarded) makes each test's assertion immune to what any OTHER
    /// repo_root's concurrent activity does, with no suite-wide
    /// serialization.

    #[cfg(test)]
    builds: usize,
}

/// `(source-relative path, byte length, mtime)` for every
/// `config/locales/**/*.{yml,yaml}` file, SORTED — a cheap stand-in for "has
/// anything under `config/locales/**` changed since the last build" that
/// never reads file contents. A file's mtime is `None` when the platform
/// can't report one (`Metadata::modified` is fallible); that's still a
/// valid, comparable fingerprint value (just less precise), never a panic
/// or a skip.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LocaleFingerprint(Vec<(String, u64, Option<SystemTime>)>);

fn compute_locale_fingerprint(repo_root: &Path) -> LocaleFingerprint {
    let mut out = Vec::new();
    fingerprint_locale_dir(repo_root, &repo_root.join("config/locales"), &mut out);
    out.sort();
    LocaleFingerprint(out)
}

fn fingerprint_locale_dir(
    repo_root: &Path,
    dir: &Path,
    out: &mut Vec<(String, u64, Option<SystemTime>)>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            fingerprint_locale_dir(repo_root, &p, out);
            continue;
        }
        let Some(ext) = p.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if ext != "yml" && ext != "yaml" {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let Ok(rel) = p.strip_prefix(repo_root) else {
            continue;
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        out.push((rel_str, meta.len(), meta.modified().ok()));
    }
}

fn locale_cache() -> &'static Mutex<HashMap<PathBuf, CachedLocaleIndex>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedLocaleIndex>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Test-only: [`CachedLocaleIndex::builds`] for `repo_root` (0 if this root
/// has never been built) — see that field's doc for why this replaced a
/// process-global counter.
#[cfg(test)]
fn build_count_for(repo_root: &Path) -> usize {
    locale_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(repo_root)
        .map(|e| e.builds)
        .unwrap_or(0)
}

/// The cached front door every extractor calls instead of
/// [`build_locale_index`] directly — see the module doc's R2 section and
/// this section's own doc. Builds fresh on the first call for a given
/// `repo_root` and on any call whose [`LocaleFingerprint`] no longer
/// matches the cached one; otherwise returns a clone of the cached index
/// (a `Vec<(String, String)>` clone is far cheaper than re-reading and
/// re-parsing every locale file).
pub(crate) fn locale_index_for(repo_root: &Path) -> Vec<(String, String)> {
    let fingerprint = compute_locale_fingerprint(repo_root);
    let mut cache = locale_cache().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(entry) = cache.get(repo_root) {
        if entry.fingerprint == fingerprint {
            return entry.index.clone();
        }
    }
    let index = build_locale_index(repo_root);
    #[cfg(test)]
    let builds = cache.get(repo_root).map(|e| e.builds).unwrap_or(0) + 1;
    cache.insert(
        repo_root.to_path_buf(),
        CachedLocaleIndex {
            fingerprint,
            index: index.clone(),
            #[cfg(test)]
            builds,
        },
    );
    index
}

fn walk_locale_dir(repo_root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let p = entry.path();
        if p.is_dir() {
            walk_locale_dir(repo_root, &p, out);
            continue;
        }
        let Some(ext) = p.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if ext != "yml" && ext != "yaml" {
            continue;
        }
        let Ok(bytes) = std::fs::read(&p) else {
            continue;
        };
        let Ok(outline) = crate::yaml::outline(&bytes) else {
            continue;
        };
        let Ok(rel) = p.strip_prefix(repo_root) else {
            continue;
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        for sym in outline.symbols {
            if let Some(idx) = sym.name.find('.') {
                out.push((rel_str.clone(), sym.name[idx + 1..].to_string()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    #[test]
    fn absolute_key_resolves_to_unique_locale_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "config/locales/it.yml",
            "it:\n  users:\n    show:\n      title: \"Ciao\"\n",
        );
        let src = b"class X\n  def y\n    t('users.show.title')\n  end\nend\n";
        let edges = extract_from_ruby(root, "app/models/x.rb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].kind, EdgeKind::I18nKey);
        assert_eq!(edges[0].dst_path, Some("config/locales/it.yml".to_string()));
        assert_eq!(edges[0].dst_symbol.as_deref(), Some("users.show.title"));
        assert_eq!(edges[0].trust, Trust::Likely);
    }

    #[test]
    fn key_defined_in_two_locale_files_is_a_candidate() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "config/locales/en.yml", "en:\n  hello: \"Hi\"\n");
        write(root, "config/locales/it.yml", "it:\n  hello: \"Ciao\"\n");
        let src = b"t('hello')\n";
        let edges = extract_from_ruby(root, "app/models/x.rb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].trust, Trust::Candidate);
        let extra = edges[0].extra_json.as_deref().unwrap();
        assert!(extra.contains("en.yml"));
        assert!(extra.contains("it.yml"));
    }

    #[test]
    fn relative_key_in_view_resolves_through_view_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "config/locales/en.yml",
            "en:\n  users:\n    show:\n      title: \"Hi\"\n",
        );
        let src = b"<%= t('.title') %>\n";
        let edges = extract_from_erb(root, "app/views/users/show.html.erb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].dst_symbol.as_deref(), Some("users.show.title"));
    }

    #[test]
    fn relative_key_strips_partial_underscore_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "config/locales/en.yml",
            "en:\n  users:\n    row:\n      label: \"Hi\"\n",
        );
        let src = b"<%= t('.label') %>\n";
        let edges = extract_from_erb(root, "app/views/users/_row.html.erb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].dst_symbol.as_deref(), Some("users.row.label"));
    }

    #[test]
    fn relative_key_outside_view_context_is_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "config/locales/en.yml", "en:\n  hello: \"Hi\"\n");
        let src = b"t('.hello')\n";
        let edges = extract_from_ruby(root, "app/controllers/x_controller.rb", src);
        assert!(edges.is_empty());
    }

    #[test]
    fn non_literal_key_is_dropped_not_fabricated() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "config/locales/en.yml", "en:\n  hello: \"Hi\"\n");
        let src = b"t(dynamic_key_var)\n";
        let edges = extract_from_ruby(root, "app/models/x.rb", src);
        assert!(edges.is_empty());
    }

    #[test]
    fn key_absent_from_any_locale_file_is_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "config/locales/en.yml", "en:\n  hello: \"Hi\"\n");
        let src = b"t('nope.nowhere')\n";
        let edges = extract_from_ruby(root, "app/models/x.rb", src);
        assert!(edges.is_empty());
    }

    #[test]
    fn i18n_dot_t_with_explicit_receiver_also_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "config/locales/en.yml", "en:\n  hello: \"Hi\"\n");
        let src = b"I18n.t('hello')\n";
        let edges = extract_from_ruby(root, "app/jobs/foo_job.rb", src);
        assert_eq!(edges.len(), 1);
    }

    // ── R2: locale index is cached once per reconcile ────────────────

    #[test]
    fn locale_index_is_built_once_across_many_dispatched_files_and_invalidated_on_change() {
        // V70-A3X: counts are read via `build_count_for(root)` — scoped to
        // THIS test's own tempdir `repo_root` inside the shared process-
        // global cache, so concurrently-running tests dispatching Rails
        // i18n extraction against THEIR OWN roots (this module's other
        // tests, `rails_lens.rs`, `ingest.rs`) can never bump what this
        // assertion observes. A bare global counter (the pre-fix shape)
        // couldn't tell those apart and flaked under parallel execution.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "config/locales/en.yml", "en:\n  hello: \"Hi\"\n");
        let src = b"t('hello')\n";

        let before = build_count_for(root);
        assert_eq!(before, 0, "a fresh repo_root must start unbuilt");
        for _ in 0..20 {
            let edges = extract_from_ruby(root, "app/models/x.rb", src);
            assert_eq!(edges.len(), 1);
        }
        let after_many = build_count_for(root);
        assert_eq!(
            after_many - before,
            1,
            "20 dispatched files sharing one repo_root must build the locale index exactly once"
        );

        // Editing the locale file (different byte length, so the
        // fingerprint changes regardless of filesystem mtime resolution)
        // must invalidate the cache on the very next call.
        write(
            root,
            "config/locales/en.yml",
            "en:\n  hello: \"Hi\"\n  bye: \"Bye\"\n",
        );
        let edges = extract_from_ruby(root, "app/models/x.rb", src);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].dst_symbol.as_deref(), Some("hello"));
        let after_change = build_count_for(root);
        assert_eq!(
            after_change - after_many,
            1,
            "editing the locale file must invalidate the cache exactly once"
        );

        // Calling again with NO change must not rebuild.
        let _ = extract_from_ruby(root, "app/models/x.rb", src);
        let after_repeat = build_count_for(root);
        assert_eq!(
            after_repeat, after_change,
            "an unchanged fingerprint must not trigger a rebuild"
        );
    }

    #[test]
    fn locale_index_cache_is_scoped_per_repo_root() {
        let tmp_a = tempfile::tempdir().unwrap();
        let tmp_b = tempfile::tempdir().unwrap();
        write(
            tmp_a.path(),
            "config/locales/en.yml",
            "en:\n  a_only: \"A\"\n",
        );
        write(
            tmp_b.path(),
            "config/locales/en.yml",
            "en:\n  b_only: \"B\"\n",
        );

        let edges_a = extract_from_ruby(tmp_a.path(), "app/models/x.rb", b"t('a_only')\n");
        let edges_b = extract_from_ruby(tmp_b.path(), "app/models/x.rb", b"t('b_only')\n");
        assert_eq!(edges_a.len(), 1, "{edges_a:#?}");
        assert_eq!(edges_b.len(), 1, "{edges_b:#?}");

        // Repo A's index must never leak repo B's key, and vice versa.
        assert!(extract_from_ruby(tmp_a.path(), "app/models/x.rb", b"t('b_only')\n").is_empty());
        assert!(extract_from_ruby(tmp_b.path(), "app/models/x.rb", b"t('a_only')\n").is_empty());
    }
}
