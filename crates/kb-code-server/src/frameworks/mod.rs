//! PRR-N3/N4 — the Rails lens: deterministic, LLM-free, convention-based
//! framework extraction (kb-code T1 track, design-nav.md §2 + the
//! design-addendum-2.md §G "full grammar" scope expansion).
//!
//! # Where this lives
//!
//! `frameworks/` is a module FAMILY, not a single file — this keeps the door
//! open for a future non-Rails framework without implying one is coming
//! (mirrors `crate::doclens`'s own "a family, not a feature" shape). Today
//! there is exactly one member:
//!
//! ```text
//! frameworks/
//!   mod.rs   — this file: FrameworkEdge, the rails-lens/1 grammar + closed
//!              kind enum, the shared dispatch entry point
//!   rails/
//!     mod.rs             — Rails-repo detection (`detect_is_rails`), cached
//!                          per-repo at reconcile time; the per-file dispatch
//!     support.rs          — PRR-N4: shared Ruby-AST + inflection helpers
//!                          used by every N4 extractor (see its own doc)
//!     routes.rs            — PRR-N3 + N4: routes.rb DSL → controller#action
//!                          edges, PLUS N4's `devise_for` → override edges
//!     views.rs              — PRR-N3: controller render/redirect analysis +
//!                          ERB render-partial/turbo_stream call-site scan
//!     view_component.rs      — PRR-N4: ViewComponent render call sites +
//!                          component↔template co-location
//!     stimulus.rs              — PRR-N4: data-controller/data-action → JS
//!                          controller files
//!     models.rs                 — PRR-N4: associations/scopes/callbacks/
//!                          validations/delegates/concern includes
//!     jobs_mailers.rs             — PRR-N4: ActiveJob/ActionMailer call sites
//!     specs.rs                     — PRR-N4: spec ↔ subject resolution
//!     i18n.rs                       — PRR-N4: `t(...)`/`I18n.t(...)` → the
//!                          locale YAML file that defines the key
//!     helpers.rs                    — PRR-N4: controller ↔ helper file
//!                          convention edge
//! ```
//!
//! PRR-N3 built `routes.rs` + `views.rs` (the MVC triangle: routes,
//! controllers, views/partials — that milestone's stated centerpiece).
//! PRR-N4 is the operator-ruled scope expansion ("this should be THE tool,
//! not a tool" — design-addendum-2.md §G) that builds every remaining
//! extractor named in design-nav.md §2's original table PLUS six more kinds
//! the addendum adds outright (`validation`, `delegate`, `concern_include`,
//! `i18n_key`, `helper_for`, `devise_override`).
//!
//! # Grammar: `rails-lens/1`, closed and versioned — STILL `/1`
//!
//! A fixed, whitelisted set of edge [`EdgeKind`]s — never an open/free-text
//! classification, exactly like `kb_core::coderefs`'s closed grammar
//! (invariant #2). PRR-N4 adds fourteen new kinds to the enum (eight already
//! named in design-nav.md §2's original table — `view_component_render`,
//! `stimulus_binding`, `association`, `scope`, `callback`, `job_enqueue`,
//! `mailer_deliver`, `spec_subject` — plus six the operator's scope
//! expansion adds outright — `validation`, `delegate`, `concern_include`,
//! `i18n_key`, `helper_for`, `devise_override`) but the grammar version
//! DOES NOT bump to `rails-lens/2`: every new kind is an ADDITION to the
//! closed set (exactly the `EdgeKind::RouteFile` precedent PRR-N3 itself set
//! — see below), never a repurposing of what an EXISTING kind string means.
//! `rails-lens/2` is reserved for the day a kind's MEANING changes, not for
//! "the operator asked for more kinds."
//!
//! [`EdgeKind::RouteFile`] was itself a PRR-N3 addition beyond
//! design-nav.md §2's original table — for the real `config/routes.rb`'s
//! `draw(:name)` convention (verified against the actual acme-shop repo
//! — see `rails::routes`'s module doc) — routes.rb→routes/<name>.rb is a
//! deterministic, unambiguous file-level linkage the same trust posture
//! applies to, so it rides the same closed grammar rather than inventing a
//! side channel. Every PRR-N4 kind below follows that same precedent.
//!
//! | `kind` | src | dst_kind | example |
//! |---|---|---|---|
//! | `route_action` | routes file line | `controller_action` | `resources :users` → `UsersController#index/show/…` |
//! | `route_file` | `config/routes.rb` line | `routes_file` | `draw(:trade)` → `config/routes/trade.rb` |
//! | `render_partial` | ERB/controller call site | `partial` | `<%= render 'users/row' %>` → `app/views/users/_row.html.erb` |
//! | `render_view` | controller action (implicit or explicit) | `view` | `UsersController#show` → `app/views/users/show.html.erb` |
//! | `turbo_stream_target` | ERB/controller | `dom_id`\|`partial` | `turbo_stream.replace "row_1", partial: …` |
//! | `view_component_render` | Ruby/ERB call site, OR the component file itself | `component_class`\|`component_template` | `render(RowComponent.new)`; co-location |
//! | `stimulus_binding` | ERB/HTML `data-controller`/`data-action` attribute | `js_controller` | `data-controller="row"` → `app/javascript/controllers/row_controller.js` |
//! | `association` | model class body | `model` | `has_many :orders, class_name: "Order"` |
//! | `scope` | model class body | (symbol only, no cross-file dst) | `scope :active, -> { … }` |
//! | `callback` | model class body | method symbol | `before_save :normalize!` |
//! | `validation` | model class body | method/attribute symbol | `validates :state, presence: true` |
//! | `delegate` | model class body | method symbol (candidate — target type unknown) | `delegate :a, to: :target` |
//! | `concern_include` | model/controller class body | `concern` | `include Trade::Discountable` |
//! | `job_enqueue` | Ruby call site | `job_class` | `FooJob.perform_later` → `app/jobs/foo_job.rb` |
//! | `mailer_deliver` | Ruby call site | `mailer_action` | `FooMailer.bar.deliver_later` → `app/mailers/foo_mailer.rb#bar` |
//! | `spec_subject` | `*_spec.rb` | `source_file` | path-convention or `described_class` |
//! | `i18n_key` | `t(...)`/`I18n.t(...)` call site | `locale_file` | `t("a.b.c")` → `config/locales/it.yml` |
//! | `helper_for` | controller file (file-level) | `helper` | `UsersController` ↔ `app/helpers/users_helper.rb` |
//! | `devise_override` | `devise_for` routes line | `controller_override` | `devise_for :users` → `app/controllers/users/sessions_controller.rb` (only if it exists) |
//!
//! # Trust posture (the oracle bar applies)
//!
//! Capped at [`Trust::Likely`], NEVER exact — these are convention matches,
//! not scope/type proofs (see [`Trust`]'s own doc). A wrong `exact` would be
//! a release blocker per the standing design law; this lane structurally
//! cannot produce one, since the storage layer's own `trust` column is
//! `CHECK (trust IN ('likely','candidate'))` (`migrations/
//! V0026__rails_edges.sql`) — there is no third value to accidentally emit.

pub mod rails;

use std::path::Path;

/// This lane's closed-grammar version string — golden-pinned (see
/// `frameworks::tests::grammar_version_is_pinned` below). Bump only when a
/// `kind` string is added or its meaning changes; see the module doc.
pub const RAILS_LENS_GRAMMAR_VERSION: &str = "rails-lens/1";

/// A `rails_edges` row's trust tier. Deliberately has no `Exact` variant —
/// see the module doc's "Trust posture" section. `Likely`: a single
/// unambiguous literal resolves to exactly one target (a `resources :users`
/// block with no ambiguity, a `render partial: "x"` matching exactly one
/// `_x.*` file). `Candidate`: multiple plausible targets, or a heuristic
/// resolution that could plausibly be wrong (e.g. an `only:`/`except:`
/// filter whose argument wasn't a literal array, so the full default action
/// set is assumed). Anything less certain than `Candidate` is DROPPED
/// entirely (no row at all) rather than assigned a trust tier — never
/// fabricate a target from a non-literal argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    Likely,
    Candidate,
}

impl Trust {
    pub fn as_str(self) -> &'static str {
        match self {
            Trust::Likely => "likely",
            Trust::Candidate => "candidate",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "likely" => Some(Trust::Likely),
            "candidate" => Some(Trust::Candidate),
            _ => None,
        }
    }
}

/// Serializes as its string form (`"likely"`/`"candidate"`) — used by the
/// golden fixture test (`tests/rails_lens.rs`) to dump `FrameworkEdge` rows
/// as comparable JSON; not used by `store.rs` (which writes `as_str()`
/// straight into a `TEXT` column, no serde involved).
impl serde::Serialize for Trust {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// The rails-lens/1 closed kind enum — see the module doc's table. PRR-N3
/// built the first five variants; PRR-N4 adds the remaining fourteen (see
/// the module doc's "Grammar" section for why that's still `/1`, not `/2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    /// `routes.rb`/`routes/*.rb` DSL → `controller#action`.
    RouteAction,
    /// PRR-N3 addition (see the module doc): `config/routes.rb`'s
    /// `draw(:name)` → `config/routes/<name>.rb`.
    RouteFile,
    /// A `render partial:`/bare-string-in-view-context/`render` call (from
    /// either a controller or an ERB call site) → a `_partial.*.erb` file.
    RenderPartial,
    /// A controller action's implicit (no explicit render/redirect in the
    /// method body) or explicit (`render :other_action`/`render template:`)
    /// resolution → a non-partial view template file.
    RenderView,
    /// A `turbo_stream.<verb> "dom_id"` call → the literal target id
    /// (`dst_kind = "dom_id"`), with an accompanying `render_partial` edge
    /// when the SAME call also carries a literal `partial:` argument.
    TurboStreamTarget,
    /// PRR-N4: a `render(FooComponent.new(...))` call site → the component
    /// class file, OR (file-level, `src_line: None`) a component class
    /// file's co-located template.
    ViewComponentRender,
    /// PRR-N4: a `data-controller="a b"`/`data-action="evt->ctrl#method"`
    /// attribute literal → a Stimulus JS controller file.
    StimulusBinding,
    /// PRR-N4: `has_many`/`has_one`/`belongs_to`/`has_and_belongs_to_many`
    /// → the associated model file.
    Association,
    /// PRR-N4: `scope :name, -> { … }` — symbol only, no cross-file dst.
    Scope,
    /// PRR-N4: `before_*`/`after_*`/`around_*` callback → the method symbol
    /// it invokes.
    Callback,
    /// PRR-N4: `validates`/`validate` → the attribute/method symbol(s).
    Validation,
    /// PRR-N4: `delegate :a, :b, to: :target` → the delegated method
    /// symbol(s); always `Candidate` (the target's type is never resolved).
    Delegate,
    /// PRR-N4: `include`/`extend SomeConcern` → the concern file.
    ConcernInclude,
    /// PRR-N4: `FooJob.perform_later`/`.perform_now`/`.set(...).perform_later`
    /// → the job class file.
    JobEnqueue,
    /// PRR-N4: `FooMailer.bar.deliver_later`/`.deliver_now` → the mailer
    /// class file + the action symbol.
    MailerDeliver,
    /// PRR-N4: a `*_spec.rb` file → its subject source file (path
    /// convention or `described_class`/`RSpec.describe` resolution).
    SpecSubject,
    /// PRR-N4: `t("a.b.c")`/`I18n.t(...)` literal key → the locale YAML
    /// file that defines it.
    I18nKey,
    /// PRR-N4: file-level controller ↔ helper convention edge (no
    /// per-call-site resolution — see the module doc).
    HelperFor,
    /// PRR-N4: `devise_for :users` → an override controller file, ONLY
    /// when it actually exists on disk.
    DeviseOverride,
}

impl EdgeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EdgeKind::RouteAction => "route_action",
            EdgeKind::RouteFile => "route_file",
            EdgeKind::RenderPartial => "render_partial",
            EdgeKind::RenderView => "render_view",
            EdgeKind::TurboStreamTarget => "turbo_stream_target",
            EdgeKind::ViewComponentRender => "view_component_render",
            EdgeKind::StimulusBinding => "stimulus_binding",
            EdgeKind::Association => "association",
            EdgeKind::Scope => "scope",
            EdgeKind::Callback => "callback",
            EdgeKind::Validation => "validation",
            EdgeKind::Delegate => "delegate",
            EdgeKind::ConcernInclude => "concern_include",
            EdgeKind::JobEnqueue => "job_enqueue",
            EdgeKind::MailerDeliver => "mailer_deliver",
            EdgeKind::SpecSubject => "spec_subject",
            EdgeKind::I18nKey => "i18n_key",
            EdgeKind::HelperFor => "helper_for",
            EdgeKind::DeviseOverride => "devise_override",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "route_action" => Some(EdgeKind::RouteAction),
            "route_file" => Some(EdgeKind::RouteFile),
            "render_partial" => Some(EdgeKind::RenderPartial),
            "render_view" => Some(EdgeKind::RenderView),
            "turbo_stream_target" => Some(EdgeKind::TurboStreamTarget),
            "view_component_render" => Some(EdgeKind::ViewComponentRender),
            "stimulus_binding" => Some(EdgeKind::StimulusBinding),
            "association" => Some(EdgeKind::Association),
            "scope" => Some(EdgeKind::Scope),
            "callback" => Some(EdgeKind::Callback),
            "validation" => Some(EdgeKind::Validation),
            "delegate" => Some(EdgeKind::Delegate),
            "concern_include" => Some(EdgeKind::ConcernInclude),
            "job_enqueue" => Some(EdgeKind::JobEnqueue),
            "mailer_deliver" => Some(EdgeKind::MailerDeliver),
            "spec_subject" => Some(EdgeKind::SpecSubject),
            "i18n_key" => Some(EdgeKind::I18nKey),
            "helper_for" => Some(EdgeKind::HelperFor),
            "devise_override" => Some(EdgeKind::DeviseOverride),
            _ => None,
        }
    }
}

/// Serializes as its string form — see `Trust`'s `Serialize` impl doc.
impl serde::Serialize for EdgeKind {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// One `rails_edges` row, pre-storage. `ordinal` is NOT carried on this
/// type — `Store::replace_rails_edges` assigns it as the row's position in
/// the slice it's given (matches every content-addressed table's own
/// producer/store split; nothing downstream needs to reference an edge's
/// ordinal the way `occurrences.local_def_ordinal` back-references a
/// `symbols` ordinal, so there's no reason to plumb it through the
/// extractors themselves — deterministic tree-walk order IS the ordinal).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FrameworkEdge {
    pub kind: EdgeKind,
    pub src_path: String,
    pub src_line: Option<u32>,
    pub src_symbol: Option<String>,
    /// `Option<String>` rather than `&'static str` — every extractor
    /// constructs this from a fixed set of string literals, but the type
    /// has to round-trip through `Store::rails_edges_by_*`'s reads too
    /// (owned `String` columns), so it can't borrow `'static`.
    pub dst_kind: Option<String>,
    pub dst_path: Option<String>,
    pub dst_symbol: Option<String>,
    pub trust: Trust,
    /// Small structured metadata (http verb, turbo_stream verb, …), stored
    /// as a JSON string. `None` when there's nothing worth carrying.
    pub extra_json: Option<String>,
}

/// The shared dispatch entry point `ingest.rs` calls — gated by the caller
/// on `is_rails` (see `rails::detect_is_rails`'s doc for why that check is
/// cached per-repo rather than re-run here per file). Only one framework
/// family exists today, so this is a single forwarding call; a future
/// second framework would add a sibling arm here rather than requiring
/// `ingest.rs` itself to grow framework-specific branches.
///
/// `repo_root` is needed because `rails::views`'s partial/view resolution
/// depends on the LIVE sibling-file set (which `_row.html.erb` variants
/// actually exist on disk), not just this blob's own bytes — mirrors
/// `import_graph::resolve_import_edges`'s same repo-root-aware design.
/// Returns an empty `Vec` for any path this lens doesn't recognize (not an
/// error — most files in a Rails repo aren't routes/controllers/views).
pub fn extract_edges(repo_root: &Path, path: &str, bytes: &[u8]) -> Vec<FrameworkEdge> {
    rails::extract(repo_root, path, bytes)
}

/// `true` if `path` is one this lens has ANY chance of producing edges for
/// — the cheap pre-check `ingest.rs` uses (alongside `is_rails`) so every
/// path this lens has NOTHING to say about (assets, `db/schema.rb`, gems,
/// …) never even calls [`extract_edges`]. A single source of truth shared
/// with `rails::extract`'s own internal dispatch — see the `rails::is_*`
/// predicates. PRR-N4 widens this beyond N3's three patterns to cover every
/// new extractor's SOURCE paths (models/jobs/mailers/specs/components/
/// concerns) — `app/javascript/controllers/**` is deliberately NOT added
/// here: those `.js` files are only ever a stimulus-binding DESTINATION,
/// resolved by a direct `repo_root`-relative filesystem walk (mirroring
/// `views::find_view_files`), never a rails-lens extraction SOURCE, so
/// gating them in here would just be dead cost on every `.js` file in a
/// Rails repo. Likewise `config/locales/**` is a pure DESTINATION for
/// `i18n_key` (resolved the same repo-root-walk way, see `rails::i18n`'s
/// doc) — never gated here.
///
/// **This is a SOURCE-path predicate — do not reuse it to gate a
/// DESTINATION (`dst_path`) read** (R3 fix, v70-a1, recon rails-lens.md
/// §6). `usages.rs`'s reverse (`rails_edges_by_dst_path`) lookup used to
/// gate on this fn, which meant a destination-only file (a locale `.yml`,
/// a Stimulus `.js` controller, a `.haml` partial…) never surfaced its
/// inbound edges there even though `rails_edges` held rows pointing at it
/// — `GET /api/framework/edges` has no such gate and did show them, so the
/// SPA rail and "find usages" disagreed about the same file. It remains
/// correct to gate a SOURCE-path query on this predicate (`resolve.rs`'s
/// `rails_edges_by_src_path` tier: `rails_edges.src_path` can structurally
/// never hold a path this predicate rejects, since `ingest.rs` only ever
/// dispatches `extract_edges` on a path that passes it).
pub fn rails_lens_relevant_path(path: &str) -> bool {
    rails::is_routes_file(path)
        || rails::is_controller_file(path)
        || rails::is_erb_view_file(path)
        || rails::is_component_ruby_file(path)
        || rails::is_component_template_file(path)
        || rails::is_model_file(path)
        || rails::is_job_file(path)
        || rails::is_mailer_file(path)
        || rails::is_spec_file(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar_version_is_pinned() {
        assert_eq!(RAILS_LENS_GRAMMAR_VERSION, "rails-lens/1");
    }

    #[test]
    fn trust_round_trips_through_its_string_form() {
        for t in [Trust::Likely, Trust::Candidate] {
            assert_eq!(Trust::from_str_opt(t.as_str()), Some(t));
        }
        assert_eq!(Trust::from_str_opt("exact"), None, "no Exact tier exists");
        assert_eq!(Trust::from_str_opt("bogus"), None);
    }

    #[test]
    fn edge_kind_round_trips_through_its_string_form() {
        for k in [
            EdgeKind::RouteAction,
            EdgeKind::RouteFile,
            EdgeKind::RenderPartial,
            EdgeKind::RenderView,
            EdgeKind::TurboStreamTarget,
            EdgeKind::ViewComponentRender,
            EdgeKind::StimulusBinding,
            EdgeKind::Association,
            EdgeKind::Scope,
            EdgeKind::Callback,
            EdgeKind::Validation,
            EdgeKind::Delegate,
            EdgeKind::ConcernInclude,
            EdgeKind::JobEnqueue,
            EdgeKind::MailerDeliver,
            EdgeKind::SpecSubject,
            EdgeKind::I18nKey,
            EdgeKind::HelperFor,
            EdgeKind::DeviseOverride,
        ] {
            assert_eq!(EdgeKind::from_str_opt(k.as_str()), Some(k));
        }
        assert_eq!(EdgeKind::from_str_opt("bogus_kind"), None);
    }
}
