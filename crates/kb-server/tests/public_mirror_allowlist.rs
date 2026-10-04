//! Public-mirror recipe guard (v0.45 N2). The allowlist in
//! `docs/public-mirror/allowlist.txt` is the single source for the edge rules
//! in `docs/public-mirror.md`. These tests pin it to the real route table
//! (`docs/api-routes.md`, generated from `router.rs`), keep private route
//! families out, and keep the doc's Traefik and Caddy snippets derived from
//! the allowlist. No daemon boot.

use std::collections::BTreeSet;

const ALLOWLIST: &str = include_str!("../../../docs/public-mirror/allowlist.txt");
const ROUTES_MD: &str = include_str!("../../../docs/api-routes.md");
const DOC: &str = include_str!("../../../docs/public-mirror.md");
const ROUTER_SRC: &str = include_str!("../src/router.rs");

/// `(method, template)` for each rule line.
fn rules() -> Vec<(String, String)> {
    ALLOWLIST
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let mut it = l.splitn(2, ' ');
            let m = it.next().unwrap().to_string();
            let p = it.next().unwrap_or("").trim().to_string();
            (m, p)
        })
        .collect()
}

/// `(method, path)` rows of the generated table.
fn route_rows() -> Vec<(String, String)> {
    ROUTES_MD
        .lines()
        .filter_map(|l| {
            let cells: Vec<&str> = l.split('|').map(str::trim).collect();
            if cells.len() < 4 {
                return None;
            }
            let m = cells[1];
            if !matches!(m, "GET" | "POST" | "PUT" | "PATCH" | "DELETE") {
                return None;
            }
            Some((m.to_string(), cells[2].trim_matches('`').to_string()))
        })
        .collect()
}

/// Top-level non-/api mounts, read from router.rs (copy of the helper in
/// host_guard_router.rs, which this unit must not edit).
fn top_level_mounts() -> Vec<String> {
    let start = ROUTER_SRC
        .find("    Router::new()\n        .nest(\"/api\", api)")
        .expect("build_router's final expression moved - update this walk");
    let body: String = ROUTER_SRC[start..]
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let needle = ".route(\"";
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = body[from..].find(needle) {
        let at = from + i + needle.len();
        let end = body[at..].find('"').map(|e| at + e).unwrap_or(body.len());
        out.push(body[at..end].to_string());
        from = at;
    }
    out
}

fn rule_exists(rule: &(String, String), rows: &[(String, String)], mounts: &[String]) -> bool {
    rows.iter().any(|r| r == rule) || (rule.0 == "GET" && mounts.contains(&rule.1))
}

#[test]
fn allowlist_every_rule_names_an_existing_route() {
    let rows = route_rows();
    let mounts = top_level_mounts();
    let rs = rules();
    assert!(!rs.is_empty(), "allowlist parsed empty");
    for r in &rs {
        assert!(
            rule_exists(r, &rows, &mounts),
            "allowlist rule `{} {}` is not a route in docs/api-routes.md or a top-level mount",
            r.0,
            r.1
        );
    }
    // The checker itself must be able to fail.
    let bogus = ("GET".to_string(), "/api/nope".to_string());
    assert!(!rule_exists(&bogus, &rows, &mounts));
    assert!(mounts.iter().any(|m| m == "/healthz"), "mounts: {mounts:?}");
}

#[test]
fn allowlist_is_get_only() {
    for (m, p) in rules() {
        assert_eq!(m, "GET", "non-GET rule `{m} {p}`");
    }
}

const DENIED_FRAGMENTS: &[&str] = &[
    "/comments",
    "/review",
    "/notes",
    "/memor",
    "/sessions",
    "/slate",
    "/admin",
    "/config",
    "/events",
    "/identity",
    "/inbox",
    "/desk",
    "/context",
    "/metrics",
    "/capture",
    "/prompt",
    "/share",
    "/export",
    "/raw",
    "/sources",
    "/runs",
];

fn private_hit(path: &str) -> Option<&'static str> {
    DENIED_FRAGMENTS.iter().copied().find(|f| path.contains(f))
}

/// Why a rule must not be on a public edge (empty = acceptable).
fn rule_violations(m: &str, p: &str) -> Vec<String> {
    let mut v = Vec::new();
    if m != "GET" {
        v.push(format!("non-GET rule `{m} {p}`"));
    }
    if let Some(f) = private_hit(p) {
        v.push(format!("rule `{m} {p}` is a private family ({f})"));
    }
    // `{*name}` (a named tail) is the only legal star; a bare `*` is a wildcard rule.
    let mut stripped = p.to_string();
    while let Some(i) = stripped.find("{*") {
        let j = stripped[i..].find('}').unwrap() + i;
        stripped.replace_range(i..=j, "");
    }
    if stripped.contains('*') {
        v.push(format!("wildcard rule `{p}`"));
    }
    if !(p.len() > "/api/".len() || p == "/healthz") {
        v.push(format!("too-broad rule `{p}`"));
    }
    if !p.starts_with('/') {
        v.push(format!("bad template `{p}`"));
    }
    v
}

#[test]
fn allowlist_denies_private_families() {
    for (m, p) in rules() {
        assert_eq!(rule_violations(&m, &p), Vec::<String>::new());
    }
}

#[test]
fn rule_checker_rejects_seeded_bad_rules() {
    // A seeded allowlist line must be rejected by the same checker the real
    // allowlist goes through, not merely matched by the fragment table.
    for (m, p) in [
        ("GET", "/api/events"),
        ("GET", "/api/kb/{kb}/artifacts/{id}/prompt"),
        ("GET", "/api/*"),
        ("GET", "/api/kb/{kb}/*"),
        ("GET", "/api/"),
        ("POST", "/api/search"),
        ("DELETE", "/api/kb/{kb}/docs/{id}"),
        ("GET", "api/search"),
    ] {
        assert!(
            !rule_violations(m, p).is_empty(),
            "seeded bad rule `{m} {p}` was accepted"
        );
    }
    assert!(rule_violations("GET", "/api/kb/{kb}/docs/by-path/{*path}").is_empty());
}

#[test]
fn doc_states_the_edge_preconditions() {
    // Doc truth pinned to the code it describes: the scrub needs a
    // non-loopback-looking request (scrub.rs::looks_non_loopback), /api/kbs
    // and the docs routes return absolute paths, and Traefik's noop service
    // answers 418, so the recipe must not rely on it for a 403.
    assert!(DOC.contains("X-Forwarded-For"), "doc must require XFF");
    assert!(DOC.contains("does nothing for a direct loopback fetch"));
    assert!(
        DOC.contains("/srv/public-docs"),
        "neutral mount path advice"
    );
    assert!(DOC.contains("filesystem\n  layout") || DOC.contains("filesystem layout"));
    assert!(
        DOC.contains("418"),
        "doc must state the noop@internal status"
    );
    let tr = fenced("traefik");
    assert!(
        tr.contains("mirror-forbid"),
        "deny router needs the 403 middleware"
    );
    assert!(tr.contains("ipAllowList"));
}

#[test]
fn allowlist_has_no_duplicates_and_is_sorted() {
    let lines: Vec<String> = rules()
        .into_iter()
        .map(|(m, p)| format!("{m} {p}"))
        .collect();
    let set: BTreeSet<&String> = lines.iter().collect();
    assert_eq!(set.len(), lines.len(), "duplicate rule");
    let mut sorted = lines.clone();
    sorted.sort();
    assert_eq!(lines, sorted, "allowlist.txt must stay byte-sorted");
}

#[test]
fn deny_list_still_covers_every_mutating_route() {
    let rows = route_rows();
    assert!(rows.len() > 100, "route table parse shrank: {}", rows.len());
    let allowed: BTreeSet<(String, String)> = rules().into_iter().collect();
    let mutating = rows.iter().filter(|(m, _)| m != "GET").count();
    assert!(mutating > 20, "mutating rows parsed: {mutating}");
    for r in rows.iter().filter(|(m, _)| m != "GET") {
        assert!(!allowed.contains(r), "mutating route allowlisted: {r:?}");
    }
}

fn regex_of(t: &str) -> String {
    let mut out = String::new();
    let mut rest = t;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let j = rest[i..].find('}').expect("unbalanced template") + i;
        out.push_str(if rest[i + 1..].starts_with('*') {
            ".+"
        } else {
            "[^/]+"
        });
        rest = &rest[j + 1..];
    }
    out.push_str(rest);
    out
}

fn fenced(lang: &str) -> String {
    let open = format!("```{lang}\n");
    let a = DOC
        .find(&open)
        .unwrap_or_else(|| panic!("no ```{lang} block in public-mirror.md"))
        + open.len();
    let b = DOC[a..].find("```").expect("unterminated fence") + a;
    DOC[a..b].to_string()
}

/// Every `name(`...`)` argument in a block.
fn calls(block: &str, name: &str) -> BTreeSet<String> {
    let open = format!("{name}(`");
    let mut out = BTreeSet::new();
    let mut rest = block;
    while let Some(i) = rest.find(&open) {
        let s = i + open.len();
        let e = rest[s..].find("`)").expect("unterminated call") + s;
        out.insert(rest[s..e].to_string());
        rest = &rest[e..];
    }
    out
}

#[test]
fn reference_snippets_in_doc_match_allowlist() {
    let paths: Vec<String> = rules().into_iter().map(|(_, p)| p).collect();

    // Traefik: exact Path() for plain templates, PathRegexp() otherwise.
    let tr = fenced("traefik");
    let mut want_exact = BTreeSet::new();
    let mut want_re = BTreeSet::new();
    for p in &paths {
        if p.contains('{') {
            want_re.insert(format!("^{}$", regex_of(p)));
        } else {
            want_exact.insert(p.clone());
        }
    }
    assert_eq!(calls(&tr, "Path"), want_exact, "Traefik Path() drifted");
    assert_eq!(
        calls(&tr, "PathRegexp"),
        want_re,
        "Traefik PathRegexp() drifted"
    );

    // Caddy: one alternation regex over every rule.
    let cd = fenced("caddy");
    let alt: Vec<String> = paths.iter().map(|p| regex_of(p)).collect();
    let want = format!("path_regexp mirror_read ^(?:{})$", alt.join("|"));
    assert!(
        cd.lines().any(|l| l.trim() == want),
        "Caddy path_regexp drifted; expected line:\n{want}"
    );
}
