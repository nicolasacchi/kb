//! V71-G0 — the Zeitwerk reader: a PURE, filesystem-only derivation of the
//! target app's path→constant mapping. No Ruby is executed, ever (the
//! daemon spawns nothing but git — this crate's invariant 10); this module
//! reads three text files and one directory listing and reasons about them
//! as text.
//!
//! ## Why this exists at all
//!
//! The evidence report's risk 2 (`/tmp/kbc7/research/module-class-explorer.md`
//! §5): "Collapsed directories and custom acronyms mean path→constant is
//! *app-specific*. Reading `config.autoload_paths` / inflections is
//! mandatory; failing to read them should degrade the tree to 'by
//! directory' with a caption, not guess." A namespace in a Rails monolith
//! legitimately spans many autoload roots (`app/models/reseller/`,
//! `app/services/reseller/`, …), so an explorer that maps a path to a
//! constant without the app's own configuration mis-groups SILENTLY.
//!
//! ## What it can and cannot know
//!
//! Everything derived here is a CONVENTION, never a proof: nothing in this
//! module ever mints `exact` (see [`super::class_for`]). Two states:
//!
//! - [`STATE_READ`] — a Rails-shaped app (`config/application.rb` present)
//!   whose inflections this reader can account for. Derived constants are
//!   `likely`.
//! - [`STATE_DEGRADED`] — no `config/application.rb`, or a CUSTOM
//!   `Zeitwerk::Inflector` assigned in an initializer (whose Ruby this
//!   module deliberately does not attempt to interpret). Roots still fall
//!   back to the directory conventions, so the by-directory grouping the
//!   design asks for still happens — it is just captioned, and every
//!   derived constant drops to `candidate`.
//!
//! ## The default root set
//!
//! Rails registers `app` with the glob `{*,*/concerns}`, so every
//! immediate subdirectory of `app/` AND every `app/*/concerns` is an
//! autoload ROOT (which is why `app/models/concerns/priceable.rb` defines
//! `Priceable`, not `Concerns::Priceable`), minus the three Rails excludes
//! ([`EXCLUDED_APP_DIRS`]). `config/application.rb` can add more
//! (`autoload_paths`/`eager_load_paths` string literals, and Rails 7.1's
//! `autoload_lib(ignore:)`), and `collapse(...)` calls remove a directory
//! level from the constant path. Roots are matched LONGEST-FIRST, so
//! `app/models/concerns` wins over `app/models` for a path under both.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// A Rails-shaped app whose inflections this reader accounts for.
pub const STATE_READ: &str = "read";
/// No `config/application.rb`, or a custom inflector this reader cannot
/// interpret — roots still come from the directory conventions, but every
/// derived constant is capped at `candidate`.
pub const STATE_DEGRADED: &str = "degraded";

/// Rails' own excludes from the `app/*` autoload glob.
pub const EXCLUDED_APP_DIRS: [&str; 3] = ["assets", "javascript", "views"];

/// Cap on how many roots/collapse/acronym entries are kept from a config
/// read — a bounded parse of a caller-controlled file, same spirit as
/// every other cap in this crate.
const MAX_CONFIG_ENTRIES: usize = 256;

/// One app's resolved autoload configuration. Cheap to clone (a handful of
/// short strings); cloned out of the process-local cache rather than held
/// across a lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Zeitwerk {
    /// [`STATE_READ`] | [`STATE_DEGRADED`].
    pub state: &'static str,
    /// Why the state is degraded, for the caption. `None` when read.
    pub reason: Option<String>,
    /// Repo-relative, forward-slashed autoload roots, LONGEST FIRST.
    pub roots: Vec<String>,
    /// Repo-relative directory patterns whose own segment is removed from
    /// the constant path. A `*` matches exactly one path segment.
    pub collapse: Vec<String>,
    /// Acronym spellings from `inflect.acronym "…"` (e.g. `"HTML"`).
    pub acronyms: Vec<String>,
    /// Repo-relative prefixes that are autoloaded by nothing (Rails 7.1's
    /// `autoload_lib(ignore: %w[assets tasks])`).
    pub ignore: Vec<String>,
}

/// Hand-written rather than derived: a derived `Default` would leave
/// `state` as the empty string, which is neither of the two states this
/// module defines — and [`super::class_for`] reads that field. The default
/// is DEGRADED (nothing has been read yet), never `read`.
impl Default for Zeitwerk {
    fn default() -> Self {
        Zeitwerk {
            state: STATE_DEGRADED,
            reason: None,
            roots: Vec::new(),
            collapse: Vec::new(),
            acronyms: Vec::new(),
            ignore: Vec::new(),
        }
    }
}

impl Zeitwerk {
    /// An empty, degraded configuration — what every non-Rails repo gets
    /// (and what [`read`] returns when the root is unreadable). Derives no
    /// constant for any path at all: with no roots there is no by-directory
    /// grouping to caption either.
    pub fn none(reason: &str) -> Self {
        Zeitwerk {
            state: STATE_DEGRADED,
            reason: Some(reason.to_string()),
            ..Default::default()
        }
    }

    /// The constant `rel_path` MUST define under this configuration, or
    /// `None` when the path is under no autoload root (a spec, a
    /// `db/schema.rb`, a vendored gem), is not Ruby, or falls under an
    /// `ignore` prefix. Pure — no filesystem access.
    pub fn constant_for_path(&self, rel_path: &str) -> Option<String> {
        let stem = rel_path.strip_suffix(".rb")?;
        if self
            .ignore
            .iter()
            .any(|i| path_has_prefix(rel_path, i.as_str()))
        {
            return None;
        }
        // `roots` is sorted longest-first by `read`, so the first prefix
        // hit is the most specific root (`app/models/concerns` over
        // `app/models`).
        let root = self
            .roots
            .iter()
            .find(|r| path_has_prefix(stem, r.as_str()))?;
        let remainder = if root.is_empty() {
            stem
        } else {
            stem.get(root.len() + 1..)?
        };
        if remainder.is_empty() {
            return None;
        }
        let mut kept: Vec<&str> = Vec::new();
        let segments: Vec<&str> = remainder.split('/').collect();
        for (i, seg) in segments.iter().enumerate() {
            // The FILE segment (the last one) is never collapsed —
            // collapsing applies to directories.
            let is_dir = i + 1 < segments.len();
            if is_dir {
                let abs_dir = if root.is_empty() {
                    segments[..=i].join("/")
                } else {
                    format!("{root}/{}", segments[..=i].join("/"))
                };
                if self.collapse.iter().any(|p| glob_dir_matches(p, &abs_dir)) {
                    continue;
                }
            }
            kept.push(seg);
        }
        if kept.is_empty() {
            return None;
        }
        Some(
            kept.iter()
                .map(|s| camelize(s, &self.acronyms))
                .collect::<Vec<_>>()
                .join("::"),
        )
    }
}

/// `true` when `path` is `prefix` itself or lives under it, comparing whole
/// SEGMENTS — `app/models` is not a prefix of `app/models_legacy/x.rb`. An
/// empty prefix matches everything (the repo root as a root).
fn path_has_prefix(path: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    if !path.starts_with(prefix) {
        return false;
    }
    matches!(path.as_bytes().get(prefix.len()), None | Some(b'/'))
}

/// Segment-wise glob match for a `collapse` pattern: `*` matches exactly
/// one whole segment (`app/*/concerns` matches `app/services/concerns`),
/// every other segment is literal. Deliberately NOT a general glob — `**`
/// and partial-segment stars are not part of what this reader claims to
/// understand, and silently mis-matching one is worse than not matching it.
fn glob_dir_matches(pattern: &str, dir: &str) -> bool {
    let p: Vec<&str> = pattern.split('/').collect();
    let d: Vec<&str> = dir.split('/').collect();
    p.len() == d.len() && p.iter().zip(d.iter()).all(|(a, b)| *a == "*" || a == b)
}

/// Rails' `String#camelize` over ONE path segment, honouring the acronym
/// table: each `_`-separated word becomes its acronym spelling when one
/// matches case-insensitively, else its first ASCII character uppercased
/// with the remainder left as-is.
///
/// (Recorded divergence) Rails' own `camelize` `capitalize`s each word,
/// which also LOWERCASES the remainder — the two differ only for a
/// mixed-case filename segment (`fooBar.rb`), which no Zeitwerk-managed
/// file conventionally has. Leaving the remainder alone is the more
/// conservative reading; either way the resulting name is only ever a
/// `likely`/`candidate` claim, never `exact`.
pub fn camelize(segment: &str, acronyms: &[String]) -> String {
    segment
        .split('_')
        .filter(|w| !w.is_empty())
        .map(
            |w| match acronyms.iter().find(|a| a.eq_ignore_ascii_case(w)) {
                Some(a) => a.clone(),
                None => {
                    let mut chars = w.chars();
                    match chars.next() {
                        Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
                        None => String::new(),
                    }
                }
            },
        )
        .collect::<Vec<_>>()
        .concat()
}

/// Read `repo_root`'s autoload configuration from disk. Filesystem reads
/// only — three text files and one directory listing.
pub fn read(repo_root: &Path) -> Zeitwerk {
    let app_dir = repo_root.join("app");
    let application_rb = repo_root.join("config").join("application.rb");
    let has_app = app_dir.is_dir();
    let has_application_rb = application_rb.is_file();
    if !has_app && !has_application_rb {
        return Zeitwerk::none("no app/ or config/application.rb — not a Rails-shaped tree");
    }

    let mut roots: Vec<String> = Vec::new();
    if has_app {
        let mut app_children: Vec<String> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&app_dir) {
            for e in entries.flatten() {
                if !e.path().is_dir() {
                    continue;
                }
                let Some(name) = e.file_name().to_str().map(|s| s.to_string()) else {
                    continue;
                };
                app_children.push(name);
            }
        }
        app_children.sort();
        for name in app_children {
            // `app/*/concerns` is a root even when `app/*` itself is
            // excluded (`app/views/concerns` does not exist in practice,
            // but the glob does not know that).
            let concerns = app_dir.join(&name).join("concerns");
            if concerns.is_dir() {
                roots.push(format!("app/{name}/concerns"));
            }
            if EXCLUDED_APP_DIRS.contains(&name.as_str()) {
                continue;
            }
            roots.push(format!("app/{name}"));
        }
    }

    let mut collapse: Vec<String> = Vec::new();
    let mut ignore: Vec<String> = Vec::new();
    let mut state = if has_application_rb {
        STATE_READ
    } else {
        STATE_DEGRADED
    };
    let mut reason = if has_application_rb {
        None
    } else {
        Some("no config/application.rb — autoload roots assumed from app/".to_string())
    };

    if let Some(text) = read_capped(&application_rb) {
        parse_autoload_lines(&text, repo_root, &mut roots, &mut ignore);
        collect_collapse(&text, &mut collapse);
    }
    let initializers = repo_root.join("config").join("initializers");
    let mut acronyms: Vec<String> = Vec::new();
    if let Some(text) = read_capped(&initializers.join("inflections.rb")) {
        collect_acronyms(&text, &mut acronyms);
        if mentions_custom_inflector(&text) {
            state = STATE_DEGRADED;
            reason = Some(
                "a custom Zeitwerk inflector is configured — this reader does not interpret Ruby"
                    .to_string(),
            );
        }
    }
    if let Some(text) = read_capped(&initializers.join("zeitwerk.rb")) {
        collect_collapse(&text, &mut collapse);
        collect_acronyms(&text, &mut acronyms);
        if mentions_custom_inflector(&text) {
            state = STATE_DEGRADED;
            reason = Some(
                "a custom Zeitwerk inflector is configured — this reader does not interpret Ruby"
                    .to_string(),
            );
        }
    }

    roots.sort();
    roots.dedup();
    // Longest first: `constant_for_path`'s first-hit walk depends on it.
    roots.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    roots.truncate(MAX_CONFIG_ENTRIES);
    collapse.sort();
    collapse.dedup();
    collapse.truncate(MAX_CONFIG_ENTRIES);
    acronyms.sort();
    acronyms.dedup();
    acronyms.truncate(MAX_CONFIG_ENTRIES);
    ignore.sort();
    ignore.dedup();
    ignore.truncate(MAX_CONFIG_ENTRIES);

    Zeitwerk {
        state,
        reason,
        roots,
        collapse,
        acronyms,
        ignore,
    }
}

/// 512 KiB is far above any real `application.rb`; a config file bigger
/// than that is not one this reader will pretend to understand.
const MAX_CONFIG_BYTES: u64 = 512 * 1024;

fn read_capped(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_CONFIG_BYTES {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

/// `true` when a config assigns its own inflector (`autoloader.inflector =
/// MyInflector.new`) — the one Zeitwerk knob whose effect is arbitrary
/// Ruby. Detected so the state can degrade honestly rather than deriving
/// constants under an inflection rule this module cannot see.
fn mentions_custom_inflector(text: &str) -> bool {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .any(|l| l.contains("inflector ="))
}

/// `config.autoload_paths`/`eager_load_paths` additions and Rails 7.1's
/// `config.autoload_lib(ignore: %w[…])`, read as TEXT: every quoted
/// literal on an `autoload_paths`/`eager_load_paths` line becomes a
/// candidate root, kept only when it actually exists as a directory (so a
/// `Rails.root.join` fragment or an interpolation this reader mis-slices
/// can never invent a root out of nothing).
fn parse_autoload_lines(
    text: &str,
    repo_root: &Path,
    roots: &mut Vec<String>,
    ignore: &mut Vec<String>,
) {
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('#') {
            continue;
        }
        if l.contains("autoload_lib") {
            if repo_root.join("lib").is_dir() {
                roots.push("lib".to_string());
            }
            for w in string_literals(l) {
                let rel = format!("lib/{w}");
                if repo_root.join(&rel).exists() {
                    ignore.push(rel);
                }
            }
            continue;
        }
        if !l.contains("autoload_paths") && !l.contains("eager_load_paths") {
            continue;
        }
        for w in string_literals(l) {
            let cleaned = normalize_config_path(&w);
            if cleaned.is_empty() {
                continue;
            }
            if repo_root.join(&cleaned).is_dir() {
                roots.push(cleaned);
            }
        }
    }
}

fn collect_collapse(text: &str, out: &mut Vec<String>) {
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('#') || !l.contains("collapse") {
            continue;
        }
        for w in string_literals(l) {
            let cleaned = normalize_config_path(&w);
            if !cleaned.is_empty() {
                out.push(cleaned);
            }
        }
    }
}

fn collect_acronyms(text: &str, out: &mut Vec<String>) {
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('#') || !l.contains("acronym") {
            continue;
        }
        for w in string_literals(l) {
            if !w.is_empty() {
                out.push(w);
            }
        }
    }
}

/// Strip the `#{Rails.root}/` / `Rails.root.join(` decoration and any
/// leading/trailing slashes from a configured path literal.
fn normalize_config_path(raw: &str) -> String {
    let mut s = raw.trim();
    for prefix in ["#{Rails.root}/", "#{Rails.root}", "./"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest;
        }
    }
    s.trim_matches('/').to_string()
}

/// Every `"…"`/`'…'` literal on one line, plus every whitespace-separated
/// word inside a `%w[…]`/`%w(…)` list. Deliberately naive: this is a
/// TEXT read of Ruby, and anything it cannot see simply does not become a
/// root (the `is_dir` check above is the safety net).
fn string_literals(line: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let bytes: Vec<char> = line.chars().collect();
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if c == '%' && i + 1 < bytes.len() && (bytes[i + 1] == 'w' || bytes[i + 1] == 'W') {
            let open = bytes.get(i + 2).copied();
            let close = match open {
                Some('[') => Some(']'),
                Some('(') => Some(')'),
                Some('{') => Some('}'),
                _ => None,
            };
            if let Some(close) = close {
                let mut j = i + 3;
                let mut buf = String::new();
                while j < bytes.len() && bytes[j] != close {
                    buf.push(bytes[j]);
                    j += 1;
                }
                out.extend(buf.split_whitespace().map(|s| s.to_string()));
                i = j + 1;
                continue;
            }
        }
        if c == '"' || c == '\'' {
            let mut j = i + 1;
            let mut buf = String::new();
            while j < bytes.len() && bytes[j] != c {
                buf.push(bytes[j]);
                j += 1;
            }
            out.push(buf);
            i = j + 1;
            continue;
        }
        i += 1;
    }
    out
}

// --- the cached front door --------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint {
    entries: Vec<(bool, u64, Option<std::time::SystemTime>)>,
}

#[derive(Debug)]
struct Cached {
    fingerprint: Fingerprint,
    value: Zeitwerk,
    #[cfg(test)]
    builds: usize,
}

fn cache() -> &'static Mutex<HashMap<PathBuf, Cached>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Cached>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Four cheap `stat`s — the three config files this reader parses plus the
/// `app/` directory itself (whose mtime changes when a subdirectory is
/// added or removed, which is the only way the DEFAULT root set can
/// change). Never reads content; never lists a directory.
fn fingerprint(repo_root: &Path) -> Fingerprint {
    let paths = [
        repo_root.join("app"),
        repo_root.join("config").join("application.rb"),
        repo_root
            .join("config")
            .join("initializers")
            .join("inflections.rb"),
        repo_root
            .join("config")
            .join("initializers")
            .join("zeitwerk.rb"),
    ];
    Fingerprint {
        entries: paths
            .iter()
            .map(|p| match std::fs::metadata(p) {
                Ok(m) => (true, m.len(), m.modified().ok()),
                Err(_) => (false, 0, None),
            })
            .collect(),
    }
}

/// The cached front door every caller uses instead of [`read`] — the same
/// shape (and the same reason) as `frameworks::rails::i18n::
/// locale_index_for`: a full reconcile dispatches this once per FILE, and
/// re-reading three config files per file on a 3.6K-file monolith is the
/// measured mistake that fix already recorded. Rebuilds when the cheap
/// four-`stat` fingerprint changes.
pub fn zeitwerk_for(repo_root: &Path) -> Zeitwerk {
    let fp = fingerprint(repo_root);
    let mut cache = cache().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(entry) = cache.get(repo_root) {
        if entry.fingerprint == fp {
            return entry.value.clone();
        }
    }
    let value = read(repo_root);
    #[cfg(test)]
    let builds = cache.get(repo_root).map(|e| e.builds).unwrap_or(0) + 1;
    cache.insert(
        repo_root.to_path_buf(),
        Cached {
            fingerprint: fp,
            value: value.clone(),
            #[cfg(test)]
            builds,
        },
    );
    value
}

/// Test-only build counter for `repo_root` — per-root, never a
/// process-global counter (the `locale_index_for` lesson: a global one
/// false-fails under parallel test execution).
#[cfg(test)]
fn build_count_for(repo_root: &Path) -> usize {
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(repo_root)
        .map(|e| e.builds)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(dirs: &[&str]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        for d in dirs {
            std::fs::create_dir_all(tmp.path().join(d)).unwrap();
        }
        tmp
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn default_roots_are_every_app_subdir_plus_concerns_minus_the_rails_excludes() {
        let tmp = app(&[
            "app/models",
            "app/models/concerns",
            "app/controllers",
            "app/views",
            "app/assets",
            "app/javascript",
        ]);
        write(
            tmp.path(),
            "config/application.rb",
            "class Application; end\n",
        );
        let zw = read(tmp.path());
        assert_eq!(zw.state, STATE_READ, "{:?}", zw.reason);
        assert!(zw.roots.contains(&"app/models".to_string()));
        assert!(zw.roots.contains(&"app/controllers".to_string()));
        assert!(zw.roots.contains(&"app/models/concerns".to_string()));
        assert!(!zw.roots.contains(&"app/views".to_string()));
        assert!(!zw.roots.contains(&"app/assets".to_string()));
        assert!(!zw.roots.contains(&"app/javascript".to_string()));
        // Longest-first ordering is what makes `app/models/concerns` win
        // over `app/models` in `constant_for_path`.
        let models = zw.roots.iter().position(|r| r == "app/models").unwrap();
        let concerns = zw
            .roots
            .iter()
            .position(|r| r == "app/models/concerns")
            .unwrap();
        assert!(concerns < models);
    }

    #[test]
    fn a_nested_path_maps_to_a_namespaced_constant_and_concerns_are_not_a_namespace() {
        let tmp = app(&["app/models", "app/models/concerns"]);
        write(tmp.path(), "config/application.rb", "\n");
        let zw = read(tmp.path());
        assert_eq!(
            zw.constant_for_path("app/models/reseller/order.rb")
                .as_deref(),
            Some("Reseller::Order")
        );
        assert_eq!(
            zw.constant_for_path("app/models/concerns/priceable.rb")
                .as_deref(),
            Some("Priceable")
        );
    }

    #[test]
    fn a_path_under_no_autoload_root_derives_no_constant() {
        let tmp = app(&["app/models", "spec/models"]);
        write(tmp.path(), "config/application.rb", "\n");
        let zw = read(tmp.path());
        assert_eq!(zw.constant_for_path("spec/models/order_spec.rb"), None);
        assert_eq!(zw.constant_for_path("db/schema.rb"), None);
        assert_eq!(zw.constant_for_path("app/models/order.yml"), None);
        // Segment-wise prefixing: `app/models_legacy` is not under
        // `app/models`.
        assert_eq!(zw.constant_for_path("app/models_legacy/order.rb"), None);
    }

    #[test]
    fn acronyms_from_the_inflections_initializer_change_the_derived_constant() {
        let tmp = app(&["app/services"]);
        write(tmp.path(), "config/application.rb", "\n");
        write(
            tmp.path(),
            "config/initializers/inflections.rb",
            "ActiveSupport::Inflector.inflections(:en) do |inflect|\n  inflect.acronym \"HTML\"\n  inflect.acronym 'API'\nend\n",
        );
        let zw = read(tmp.path());
        assert_eq!(zw.acronyms, vec!["API".to_string(), "HTML".to_string()]);
        assert_eq!(
            zw.constant_for_path("app/services/html_parser.rb")
                .as_deref(),
            Some("HTMLParser")
        );
        assert_eq!(
            zw.constant_for_path("app/services/api/client.rb")
                .as_deref(),
            Some("API::Client")
        );
    }

    #[test]
    fn a_collapsed_directory_drops_out_of_the_constant_path() {
        let tmp = app(&["app/services"]);
        write(tmp.path(), "config/application.rb", "\n");
        write(
            tmp.path(),
            "config/initializers/zeitwerk.rb",
            "Rails.autoloaders.main.collapse(\"app/services/*/shared\")\n",
        );
        let zw = read(tmp.path());
        assert_eq!(zw.collapse, vec!["app/services/*/shared".to_string()]);
        assert_eq!(
            zw.constant_for_path("app/services/billing/shared/rounding.rb")
                .as_deref(),
            Some("Billing::Rounding")
        );
        // The star matches exactly ONE segment — not two.
        assert_eq!(
            zw.constant_for_path("app/services/a/b/shared/rounding.rb")
                .as_deref(),
            Some("A::B::Shared::Rounding")
        );
    }

    #[test]
    fn autoload_lib_adds_lib_as_a_root_and_records_its_ignores() {
        let tmp = app(&["app/models", "lib/tasks", "lib/parsers"]);
        write(
            tmp.path(),
            "config/application.rb",
            "    config.autoload_lib(ignore: %w[assets tasks])\n",
        );
        let zw = read(tmp.path());
        assert!(zw.roots.contains(&"lib".to_string()));
        assert_eq!(zw.ignore, vec!["lib/tasks".to_string()]);
        assert_eq!(
            zw.constant_for_path("lib/parsers/csv.rb").as_deref(),
            Some("Parsers::Csv")
        );
        assert_eq!(zw.constant_for_path("lib/tasks/import.rb"), None);
    }

    #[test]
    fn an_autoload_paths_literal_that_is_not_a_real_directory_never_becomes_a_root() {
        let tmp = app(&["app/models", "extras"]);
        write(
            tmp.path(),
            "config/application.rb",
            "config.autoload_paths << \"#{Rails.root}/extras\"\nconfig.autoload_paths << \"#{Rails.root}/nope\"\n",
        );
        let zw = read(tmp.path());
        assert!(zw.roots.contains(&"extras".to_string()));
        assert!(!zw.roots.contains(&"nope".to_string()));
    }

    #[test]
    fn a_custom_inflector_degrades_the_state_with_a_reason_but_keeps_the_roots() {
        let tmp = app(&["app/models"]);
        write(tmp.path(), "config/application.rb", "\n");
        write(
            tmp.path(),
            "config/initializers/zeitwerk.rb",
            "Rails.autoloaders.each { |a| a.inflector = MyInflector.new }\n",
        );
        let zw = read(tmp.path());
        assert_eq!(zw.state, STATE_DEGRADED);
        assert!(zw.reason.is_some());
        assert!(zw.roots.contains(&"app/models".to_string()));
        assert_eq!(
            zw.constant_for_path("app/models/order.rb").as_deref(),
            Some("Order"),
            "a degraded read still groups by directory — it is captioned, not silent"
        );
    }

    #[test]
    fn a_non_rails_tree_reads_as_degraded_with_no_roots_at_all() {
        let tmp = app(&["src"]);
        let zw = read(tmp.path());
        assert_eq!(zw.state, STATE_DEGRADED);
        assert!(zw.roots.is_empty());
        assert_eq!(zw.constant_for_path("src/lib.rb"), None);
    }

    #[test]
    fn a_commented_out_config_line_is_not_read() {
        let tmp = app(&["app/models", "extras"]);
        write(
            tmp.path(),
            "config/application.rb",
            "# config.autoload_paths << \"extras\"\n",
        );
        let zw = read(tmp.path());
        assert!(!zw.roots.contains(&"extras".to_string()));
    }

    #[test]
    fn the_cached_front_door_rebuilds_only_when_the_fingerprint_changes() {
        let tmp = app(&["app/models"]);
        write(tmp.path(), "config/application.rb", "\n");
        let root = tmp.path();
        let a = zeitwerk_for(root);
        let b = zeitwerk_for(root);
        assert_eq!(a, b);
        assert_eq!(build_count_for(root), 1, "a second call must hit the cache");
        // A config edit (new length) invalidates it.
        write(
            root,
            "config/initializers/inflections.rb",
            "inflect.acronym \"API\"\n",
        );
        let c = zeitwerk_for(root);
        assert_eq!(c.acronyms, vec!["API".to_string()]);
        assert_eq!(build_count_for(root), 2);
    }

    #[test]
    fn camelize_uppercases_each_word_and_prefers_an_acronym() {
        let acr = vec!["API".to_string()];
        assert_eq!(camelize("order", &acr), "Order");
        assert_eq!(camelize("order_line", &acr), "OrderLine");
        assert_eq!(camelize("api", &acr), "API");
        assert_eq!(camelize("api_client", &acr), "APIClient");
        assert_eq!(camelize("", &acr), "");
    }
}
