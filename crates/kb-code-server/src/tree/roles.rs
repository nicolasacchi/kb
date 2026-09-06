//! V71-F1 — the Rails ROLE vocabulary, derived from a path by CONVENTION.
//!
//! The evidence report's §2.1 asks for a `role` projection whose buckets are
//! Model · Controller · Views · Jobs · Specs · Factories · Serializers ·
//! Locales · Migrations. Every one of those is read off the PATH, which
//! makes each verdict a convention and never a proof — the same structural
//! cap this crate's invariant 12 puts on `rails-lens/1`'s convention edges
//! and invariant 13 puts on a Zeitwerk-derived FQN. Nothing here can mint
//! `exact`; [`ROLE_TRUST`] is the ceiling and it is `likely`.
//!
//! The vocabulary is CLOSED (the E1 precedent for `usages/2`'s kinds): a
//! path that matches none of the rules is [`ROLE_OTHER`], which the `role`
//! projection renders as its `Unplaced` bucket rather than inventing a
//! bucket name. A file must never silently disappear because a projection
//! failed to place it.

/// Every role name this module can mint, in render order. CLOSED — a
/// caller may compare against this list exhaustively.
pub const ROLES: &[&str] = &[
    "model",
    "controller",
    "view",
    "helper",
    "job",
    "mailer",
    "serializer",
    "service",
    "concern",
    "channel",
    "component",
    "spec",
    "factory",
    "migration",
    "locale",
    "config",
    "asset",
    "lib",
    "task",
    "doc",
    ROLE_OTHER,
];

/// The bucket for a path no rule places — surfaced, never hidden.
pub const ROLE_OTHER: &str = "other";

/// The trust ceiling for every role verdict. A path convention is evidence,
/// not proof: `app/models/order.rb` is *very likely* a model and there is no
/// tree walk in this module that could raise that to `exact`.
pub const ROLE_TRUST: &str = "likely";

/// `true` if `name` is one of [`ROLES`].
pub fn is_role(name: &str) -> bool {
    ROLES.contains(&name)
}

/// The role a repo-relative, forward-slash path convention-ally carries.
///
/// Rules are tried in the order written and the FIRST hit wins; the test
/// suite pins the orderings that actually matter (a spec under
/// `spec/models/` is a `spec`, not a `model`; a factory under
/// `spec/factories/` is a `factory`, not a `spec`).
pub fn role_for_path(path: &str) -> &'static str {
    let p = path;
    let base = p.rsplit('/').next().unwrap_or(p);

    // Test trees first — `spec/models/order_spec.rb` is a spec, and the
    // factory tree under it is narrower still.
    if seg(p, "spec/factories") || seg(p, "test/factories") || base.ends_with("_factory.rb") {
        return "factory";
    }
    if p.starts_with("spec/")
        || p.starts_with("test/")
        || base.ends_with("_spec.rb")
        || base.ends_with("_test.rb")
        || base.ends_with(".test.ts")
        || base.ends_with(".spec.ts")
        || base.ends_with(".test.tsx")
        || base.ends_with(".spec.tsx")
    {
        return "spec";
    }
    if seg(p, "db/migrate") {
        return "migration";
    }
    if p.starts_with("config/locales/") || seg(p, "config/locales") {
        return "locale";
    }
    if p.starts_with("config/") || p.starts_with("db/") || base == "Gemfile" || base == "Rakefile" {
        return "config";
    }
    if seg(p, "lib/tasks") || base.ends_with(".rake") {
        return "task";
    }
    // `app/**/concerns/**` is a concern regardless of which app subtree it
    // sits in — Rails' own `app/*/concerns` autoload glob (entities::
    // zeitwerk's `EXCLUDED_APP_DIRS` note) is why this is checked before
    // the per-subtree rules below.
    if seg(p, "concerns") && p.starts_with("app/") {
        return "concern";
    }
    if p.starts_with("app/models/") {
        return "model";
    }
    if p.starts_with("app/controllers/") {
        return "controller";
    }
    if p.starts_with("app/views/") || base.ends_with(".erb") || base.ends_with(".haml") {
        return "view";
    }
    if p.starts_with("app/helpers/") {
        return "helper";
    }
    if p.starts_with("app/jobs/") || p.starts_with("app/workers/") {
        return "job";
    }
    if p.starts_with("app/mailers/") {
        return "mailer";
    }
    if p.starts_with("app/serializers/") || p.starts_with("app/presenters/") {
        return "serializer";
    }
    if p.starts_with("app/services/")
        || p.starts_with("app/interactors/")
        || p.starts_with("app/queries/")
        || p.starts_with("app/forms/")
    {
        return "service";
    }
    if p.starts_with("app/channels/") {
        return "channel";
    }
    if p.starts_with("app/components/") || p.starts_with("app/javascript/") {
        return "component";
    }
    if p.starts_with("app/assets/") || p.starts_with("public/") {
        return "asset";
    }
    if p.starts_with("lib/") {
        return "lib";
    }
    if p.starts_with("docs/") || base.ends_with(".md") {
        return "doc";
    }
    ROLE_OTHER
}

/// `true` when `path` contains `needle` as a whole `/`-delimited run of
/// segments (`seg("app/models/concerns/x.rb", "concerns")` is true;
/// `seg("app/concernsish/x.rb", "concerns")` is not).
fn seg(path: &str, needle: &str) -> bool {
    if path == needle {
        return true;
    }
    if let Some(rest) = path.strip_prefix(needle) {
        if rest.starts_with('/') {
            return true;
        }
    }
    path.contains(&format!("/{needle}/")) || path.ends_with(&format!("/{needle}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_minted_role_is_in_the_closed_vocabulary() {
        // A representative path per rule, so a rule that returns a name
        // missing from ROLES fails here rather than on the wire.
        let probes = [
            "app/models/order.rb",
            "app/controllers/orders_controller.rb",
            "app/views/orders/show.html.erb",
            "app/helpers/orders_helper.rb",
            "app/jobs/reviews_job.rb",
            "app/mailers/order_mailer.rb",
            "app/serializers/order_serializer.rb",
            "app/services/apply_coupon_service.rb",
            "app/models/concerns/priceable.rb",
            "app/channels/chat_channel.rb",
            "app/components/button_component.rb",
            "spec/models/order_spec.rb",
            "spec/factories/orders.rb",
            "db/migrate/20240101_add_x.rb",
            "config/locales/en.yml",
            "config/application.rb",
            "app/assets/stylesheets/app.scss",
            "lib/reseller/client.rb",
            "lib/tasks/import.rake",
            "docs/readme.md",
            "unplaceable-thing",
        ];
        for p in probes {
            let r = role_for_path(p);
            assert!(
                is_role(r),
                "{p}: minted {r:?}, not in the closed vocabulary"
            );
        }
    }

    #[test]
    fn narrower_trees_win_over_broader_ones() {
        assert_eq!(role_for_path("spec/models/order_spec.rb"), "spec");
        assert_eq!(role_for_path("spec/factories/orders.rb"), "factory");
        assert_eq!(role_for_path("app/models/concerns/priceable.rb"), "concern");
        assert_eq!(role_for_path("db/migrate/1_x.rb"), "migration");
        assert_eq!(role_for_path("config/locales/it.yml"), "locale");
        assert_eq!(role_for_path("lib/tasks/x.rake"), "task");
    }

    #[test]
    fn an_unplaceable_path_is_other_never_a_guess() {
        assert_eq!(role_for_path("weird"), ROLE_OTHER);
        assert_eq!(role_for_path("vendor/bundle/gem.rb"), ROLE_OTHER);
    }

    #[test]
    fn seg_matches_whole_segments_only() {
        assert!(seg("app/models/concerns/x.rb", "concerns"));
        assert!(!seg("app/concernsish/x.rb", "concerns"));
        assert!(seg("spec/factories", "spec/factories"));
        assert!(seg("spec/factories/x.rb", "spec/factories"));
    }
}
