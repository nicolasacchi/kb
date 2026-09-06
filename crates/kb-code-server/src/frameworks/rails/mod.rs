//! Rails-repo detection + the per-file dispatch entry for the Rails lens's
//! extractors (PRR-N3's routes/views + PRR-N4's full-grammar expansion —
//! see `frameworks` module doc for the family shape and the grammar table).

pub mod helpers;
pub mod i18n;
pub mod jobs_mailers;
pub mod models;
pub mod routes;
pub mod specs;
pub mod stimulus;
pub mod support;
pub mod view_component;
pub mod views;

use crate::frameworks::FrameworkEdge;
use std::path::Path;

/// Detect whether `repo_root` is a Rails app, per design-nav.md §2's
/// "Detection gate": `config/routes.rb` present AND a `Gemfile` declaring
/// `gem "rails"` (double OR single quotes). Both conditions are cheap
/// filesystem/text checks — no Ruby parsing needed for the gate itself.
///
/// **Caller contract: call this ONCE per repo, at reconcile/boot time, and
/// cache the result** — never re-run it per file. Every real caller in this
/// crate (`lib.rs`'s boot walk, `sink::spawn`'s per-repo map) does exactly
/// that; see their own call sites for why (a `sink.rs` live-watcher handler
/// processes one file-change event at a time, so re-running this per event
/// would mean a `Gemfile` read + `routes.rb` stat on every single keystroke-
/// triggered save in an active Rails repo).
pub fn detect_is_rails(repo_root: &Path) -> bool {
    if !repo_root.join("config/routes.rb").is_file() {
        return false;
    }
    let Ok(gemfile) = std::fs::read_to_string(repo_root.join("Gemfile")) else {
        return false;
    };
    gemfile_declares_rails(&gemfile)
}

/// `true` if `gemfile` contains a `gem "rails"`/`gem 'rails'` declaration —
/// exact gem name match (a bundler group `do...end`/indentation doesn't
/// matter; a prefix-alike like `rails_sortable`/`rails-controller-testing`
/// must NOT match). Pure text scan, no Ruby parse: `Gemfile` is a small,
/// well-known DSL and this is the one line shape the gate cares about.
fn gemfile_declares_rails(gemfile: &str) -> bool {
    for line in gemfile.lines() {
        let Some(rest) = line.trim_start().strip_prefix("gem ") else {
            continue;
        };
        if let Some(name) = extract_quoted_gem_name(rest.trim_start()) {
            if name == "rails" {
                return true;
            }
        }
    }
    false
}

/// Extract the first quoted string at the start of `s` (either `'...'` or
/// `"..."`), e.g. `"rails", "8.1.3.1"` → `Some("rails")`. `None` if `s`
/// doesn't start with a quote, or the quote is never closed.
fn extract_quoted_gem_name(s: &str) -> Option<&str> {
    let mut chars = s.chars();
    let quote = chars.next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let start = quote.len_utf8();
    let end = s[start..].find(quote)? + start;
    Some(&s[start..end])
}

/// The per-file dispatch entry — see `frameworks::extract_edges`'s doc for
/// the caller contract (already gated on `is_rails`).
///
/// # PRR-N4: multiple extractors per file, each its own independent parse
///
/// N3 had a strict one-path-one-extractor dispatch; N4 widens the grammar
/// enough that a SINGLE file often has several DIFFERENT things to say
/// (a controller has render/redirect targets AND may enqueue jobs AND may
/// `include` a concern AND may call `t(...)` AND has a `helper_for`
/// file-level convention edge) — so each branch below UNIONS the relevant
/// extractors' output. Every extractor still does its OWN independent
/// `crate::lang::parse` call (N3's `routes.rs`/`views.rs` precedent — see
/// `views.rs`'s module doc's "deliberately NOT shared" note) — a controller
/// file is therefore parsed several times over by several extractors. This
/// is a deliberate, accepted cost: ingest-time only (never per-request),
/// gated on `is_rails` so it's zero-cost for every non-Rails repo, and
/// re-parsing a single small `.rb`/`.erb` file 3-5× is immaterial next to
/// the grammar's own `resources`/DSL-walk cost. Sharing one parsed tree
/// across extractors would need a bigger refactor (a shared "parsed Ruby
/// file" context threaded through every extractor's signature) that isn't
/// justified by this milestone's actual cost profile.
pub fn extract(repo_root: &Path, path: &str, bytes: &[u8]) -> Vec<FrameworkEdge> {
    if is_routes_file(path) {
        let mut out = routes::extract(path, bytes);
        out.extend(routes::extract_devise_overrides(repo_root, path, bytes));
        return out;
    }
    if is_controller_file(path) {
        let mut out = views::extract_controller(repo_root, path, bytes);
        out.extend(view_component::extract_from_ruby(repo_root, path, bytes));
        out.extend(jobs_mailers::extract_from_ruby(repo_root, path, bytes));
        out.extend(models::extract_concern_includes(repo_root, path, bytes));
        out.extend(i18n::extract_from_ruby(repo_root, path, bytes));
        out.extend(helpers::extract_helper_for(repo_root, path));
        return out;
    }
    if is_erb_view_file(path) {
        let mut out = views::extract_erb(repo_root, path, bytes);
        out.extend(view_component::extract_from_erb(repo_root, path, bytes));
        out.extend(stimulus::extract(repo_root, path, bytes));
        out.extend(jobs_mailers::extract_from_erb(repo_root, path, bytes));
        out.extend(i18n::extract_from_erb(repo_root, path, bytes));
        return out;
    }
    if is_component_ruby_file(path) {
        let mut out = view_component::extract_component_class(repo_root, path, bytes);
        out.extend(view_component::extract_from_ruby(repo_root, path, bytes));
        out.extend(jobs_mailers::extract_from_ruby(repo_root, path, bytes));
        out.extend(i18n::extract_from_ruby(repo_root, path, bytes));
        return out;
    }
    if is_component_template_file(path) {
        let mut out = view_component::extract_from_erb(repo_root, path, bytes);
        out.extend(stimulus::extract(repo_root, path, bytes));
        out.extend(i18n::extract_from_erb(repo_root, path, bytes));
        return out;
    }
    if is_model_file(path) {
        let mut out = models::extract(repo_root, path, bytes);
        out.extend(models::extract_concern_includes(repo_root, path, bytes));
        out.extend(jobs_mailers::extract_from_ruby(repo_root, path, bytes));
        out.extend(i18n::extract_from_ruby(repo_root, path, bytes));
        return out;
    }
    if is_job_file(path) {
        let mut out = jobs_mailers::extract_from_ruby(repo_root, path, bytes);
        out.extend(i18n::extract_from_ruby(repo_root, path, bytes));
        return out;
    }
    if is_mailer_file(path) {
        let mut out = jobs_mailers::extract_from_ruby(repo_root, path, bytes);
        out.extend(i18n::extract_from_ruby(repo_root, path, bytes));
        return out;
    }
    if is_spec_file(path) {
        return specs::extract(repo_root, path, bytes);
    }
    Vec::new()
}

/// `config/routes.rb` itself, or any `config/routes/*.rb` split file (the
/// real `draw(:name)` convention — see `routes`'s module doc).
pub fn is_routes_file(path: &str) -> bool {
    path == "config/routes.rb" || (path.starts_with("config/routes/") && path.ends_with(".rb"))
}

pub fn is_controller_file(path: &str) -> bool {
    path.starts_with("app/controllers/") && path.ends_with(".rb")
}

pub fn is_erb_view_file(path: &str) -> bool {
    path.starts_with("app/views/") && path.ends_with(".erb")
}

/// PRR-N4: a ViewComponent Ruby class file — `app/components/**/*.rb`.
pub fn is_component_ruby_file(path: &str) -> bool {
    path.starts_with("app/components/") && path.ends_with(".rb")
}

/// PRR-N4: a ViewComponent's co-located template — `app/components/**/*.erb`
/// (co-located beside its `.rb`, NOT under `app/views/`, so this is a
/// separate predicate from [`is_erb_view_file`]).
pub fn is_component_template_file(path: &str) -> bool {
    path.starts_with("app/components/") && path.ends_with(".erb")
}

/// PRR-N4: `app/models/**/*.rb`, including `app/models/concerns/**` (a
/// concern file itself gets no special treatment here — `models::extract`
/// simply finds no `class`/recognized macros in a bare `module` and returns
/// empty; what matters is that an app model is only ever under this tree).
pub fn is_model_file(path: &str) -> bool {
    path.starts_with("app/models/") && path.ends_with(".rb")
}

pub fn is_job_file(path: &str) -> bool {
    path.starts_with("app/jobs/") && path.ends_with(".rb")
}

pub fn is_mailer_file(path: &str) -> bool {
    path.starts_with("app/mailers/") && path.ends_with(".rb")
}

/// `spec/**/*_spec.rb` — RSpec's own file-naming convention.
pub fn is_spec_file(path: &str) -> bool {
    path.starts_with("spec/") && path.ends_with("_spec.rb")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemfile_matches_exact_rails_gem_only() {
        assert!(gemfile_declares_rails("gem \"rails\", \"8.1.3.1\"\n"));
        assert!(gemfile_declares_rails("gem 'rails'\n"));
        assert!(gemfile_declares_rails("  gem 'rails', require: false\n"));
        assert!(!gemfile_declares_rails("gem \"rails_sortable\"\n"));
        assert!(!gemfile_declares_rails("gem 'rails-controller-testing'\n"));
        assert!(!gemfile_declares_rails("gem \"sinatra\"\n"));
        assert!(!gemfile_declares_rails(""));
    }

    #[test]
    fn detect_is_rails_requires_both_conditions() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Neither present.
        assert!(!detect_is_rails(root));

        // routes.rb only.
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::write(
            root.join("config/routes.rb"),
            "Rails.application.routes.draw do\nend\n",
        )
        .unwrap();
        assert!(!detect_is_rails(root));

        // + Gemfile without rails.
        std::fs::write(root.join("Gemfile"), "gem \"sinatra\"\n").unwrap();
        assert!(!detect_is_rails(root));

        // + Gemfile with rails.
        std::fs::write(root.join("Gemfile"), "gem \"rails\", \"8.1.3.1\"\n").unwrap();
        assert!(detect_is_rails(root));
    }

    #[test]
    fn path_pattern_gates() {
        assert!(is_routes_file("config/routes.rb"));
        assert!(is_routes_file("config/routes/trade.rb"));
        assert!(!is_routes_file("config/routes/trade.rb.bak"));
        assert!(!is_routes_file("config/initializers/routes.rb"));

        assert!(is_controller_file(
            "app/controllers/trade/rounds_controller.rb"
        ));
        assert!(!is_controller_file("app/models/round.rb"));

        assert!(is_erb_view_file("app/views/trade/rounds/show.html.erb"));
        assert!(is_erb_view_file(
            "app/views/trade/rounds/merge_complete.turbo_stream.erb"
        ));
        assert!(!is_erb_view_file("app/views/trade/rounds/show.html.haml"));
    }

    #[test]
    fn prr_n4_path_pattern_gates() {
        assert!(is_component_ruby_file("app/components/row_component.rb"));
        assert!(is_component_ruby_file(
            "app/components/trade/row_component.rb"
        ));
        assert!(!is_component_ruby_file("app/models/row_component.rb"));

        assert!(is_component_template_file(
            "app/components/row_component.html.erb"
        ));
        assert!(!is_component_template_file("app/views/row/_row.html.erb"));

        assert!(is_model_file("app/models/round.rb"));
        assert!(is_model_file("app/models/concerns/discountable.rb"));
        assert!(!is_model_file("app/controllers/round.rb"));

        assert!(is_job_file("app/jobs/foo_job.rb"));
        assert!(!is_job_file("app/models/foo_job.rb"));

        assert!(is_mailer_file("app/mailers/foo_mailer.rb"));
        assert!(!is_mailer_file("app/jobs/foo_mailer.rb"));

        assert!(is_spec_file("spec/models/round_spec.rb"));
        assert!(is_spec_file("spec/helpers/foo_helper_spec.rb"));
        assert!(!is_spec_file("spec/factories/rounds.rb"));
    }
}
