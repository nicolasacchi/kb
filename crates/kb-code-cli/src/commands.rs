//! V70-A5 — `kb-code commands …`: the CLI half of kbc-cmd/1.
//!
//! The registry lives in the SERVER crate (`crates/kb-code-server/commands/
//! registry.json`) and is embedded HERE by `include_str!` on the same path
//! the daemon uses, so the two binaries can never disagree about what a key
//! means. That single fact is what makes `doctor` a real gate rather than a
//! lint over a copy: the bytes it checks are the bytes the daemon serves at
//! `GET /api/commands` and the bytes the SPA's generated mirror is pinned to.
//!
//! Five verbs:
//!
//! * `manifest` — the whole registry, human or `--json`/`--md`.
//! * `cheatsheet` — one scope's keys, printable. The `?` sheet's twin: both
//!   render the same rows in the same registry order, so the printed card and
//!   the on-screen one cannot drift.
//! * `conflicts` — the generated collision report (§P2: "v7.0 exits with zero
//!   unratified rows").
//! * `explain` — the PURE RESOLVER, run over any scope + context. Same
//!   algorithm as the SPA dispatcher, and the answer to "why did that key do
//!   that".
//! * `doctor` — the CI gate, wired to a `#[test]` in `main.rs` so
//!   `cargo test -p kb-code-cli` is the check.

use anyhow::{bail, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

/// The registry, verbatim. Same file the daemon embeds — see the module doc.
pub const REGISTRY_JSON: &str = include_str!("../../kb-code-server/commands/registry.json");

#[derive(Debug, Deserialize)]
pub struct Registry {
    pub schema: String,
    pub leader: String,
    pub presets: Vec<Preset>,
    pub reserved_chords: ReservedChords,
    pub context_keys: Vec<ContextKey>,
    pub scopes: Vec<Scope>,
    pub commands: Vec<Command>,
}

#[derive(Debug, Deserialize)]
pub struct Preset {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Deserialize)]
pub struct ReservedChords {
    pub hard: Vec<String>,
    pub passthrough: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ContextKey {
    pub key: String,
    #[serde(default)]
    pub values: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Scope {
    pub id: String,
    pub title: String,
    pub depth: i64,
    pub covers: String,
    pub coactive_with: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Keys {
    pub vim: Vec<String>,
    pub plain: Vec<String>,
    pub helix: Vec<String>,
}

impl Keys {
    pub fn column(&self, preset: &str) -> &[String] {
        match preset {
            "plain" => &self.plain,
            "helix" => &self.helix,
            _ => &self.vim,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Command {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub aka: Vec<String>,
    pub group: String,
    pub keys: Keys,
    pub scope: String,
    #[serde(default)]
    pub when: Option<String>,
    #[serde(default)]
    pub targets: Vec<String>,
    pub cli: String,
    pub mutation: String,
    pub side_effect: String,
    #[serde(default)]
    pub affordance_anchor: Option<String>,
    #[serde(default)]
    pub dismiss_order: Option<i64>,
    pub lifecycle: String,
    pub dispatch: String,
    #[serde(default)]
    pub vim_kind: Option<String>,
    #[serde(default)]
    pub browser_reserved: Option<String>,
    #[serde(default)]
    pub browser_passthrough: bool,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub ratified_conflicts: Vec<String>,
}

pub fn load() -> Result<Registry> {
    Ok(serde_json::from_str(REGISTRY_JSON)?)
}

// ── the `when` grammar (the SPA's `dispatch.ts` mirror) ───────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum Op {
    Truthy,
    Falsy,
    Eq(String),
    Ne(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Atom {
    key: String,
    op: Op,
}

/// Atoms joined by `&&`: `key`, `!key`, `key == value`, `key != value`.
/// Deliberately no `||` and no parentheses — a predicate that needs
/// disjunction should be two rows, and keeping the grammar this small is what
/// lets [`when_disjoint`] reason mechanically instead of asking a human.
fn parse_when(when: Option<&str>) -> Vec<Atom> {
    let Some(w) = when else { return Vec::new() };
    let mut out = Vec::new();
    for raw in w.split("&&") {
        let part = raw.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((k, v)) = part.split_once("==") {
            out.push(Atom {
                key: k.trim().to_string(),
                op: Op::Eq(v.trim().to_string()),
            });
        } else if let Some((k, v)) = part.split_once("!=") {
            out.push(Atom {
                key: k.trim().to_string(),
                op: Op::Ne(v.trim().to_string()),
            });
        } else if let Some(k) = part.strip_prefix('!') {
            out.push(Atom {
                key: k.trim().to_string(),
                op: Op::Falsy,
            });
        } else {
            out.push(Atom {
                key: part.to_string(),
                op: Op::Truthy,
            });
        }
    }
    out
}

fn truthy(v: Option<&String>) -> bool {
    matches!(v, Some(s) if !s.is_empty() && s != "false")
}

/// True when every atom holds in `ctx`. An UNKNOWN key reads as absent, so an
/// empty bag makes only unconditional rows available — the honest degrade.
pub fn eval_when(when: Option<&str>, ctx: &BTreeMap<String, String>) -> bool {
    parse_when(when).into_iter().all(|a| {
        let v = ctx.get(&a.key);
        match a.op {
            Op::Truthy => truthy(v),
            Op::Falsy => !truthy(v),
            Op::Eq(x) => v.map(String::as_str).unwrap_or("") == x,
            Op::Ne(x) => v.map(String::as_str).unwrap_or("") != x,
        }
    })
}

/// Two predicates are PROVABLY disjoint when one asserts an atom the other
/// negates. Conservative: "might overlap" reads as "collides", so an
/// unratified pair fails the gate rather than being waved through.
pub fn when_disjoint(a: Option<&str>, b: Option<&str>) -> bool {
    let (aa, bb) = (parse_when(a), parse_when(b));
    for x in &aa {
        for y in &bb {
            if x.key != y.key {
                continue;
            }
            let hit = match (&x.op, &y.op) {
                (Op::Truthy, Op::Falsy) | (Op::Falsy, Op::Truthy) => true,
                (Op::Eq(p), Op::Eq(q)) => p != q,
                (Op::Eq(p), Op::Ne(q)) | (Op::Ne(q), Op::Eq(p)) => p == q,
                _ => false,
            };
            if hit {
                return true;
            }
        }
    }
    false
}

// ── key tokens ────────────────────────────────────────────────────────────

pub fn tokens_of(sequence: &str) -> Vec<&str> {
    sequence.split(' ').filter(|t| !t.is_empty()).collect()
}

fn wildcard_matches(token: &str, key: &str) -> bool {
    let one = key.chars().count() == 1;
    let c = key.chars().next().unwrap_or('\0');
    match token {
        "{a-z}" => one && c.is_ascii_lowercase(),
        "{1-9}" => one && ('1'..='9').contains(&c),
        "{0-9}" => one && c.is_ascii_digit(),
        _ => false,
    }
}

fn token_matches(token: &str, key: &str) -> bool {
    token == key || wildcard_matches(token, key)
}

// ── the resolver (the SPA's `resolve()` mirror) ───────────────────────────

fn in_play(c: &Command, scope: &str) -> bool {
    c.scope == scope || c.scope == "global"
}

/// THE pure resolver: the one command `sequence` names in `scope` under
/// `ctx`, in `preset`. Precedence: scope-specific over global; for `Escape`
/// the LOWEST `dismiss_order` whose `when` holds (innermost first); then
/// registry order.
pub fn resolve<'a>(
    reg: &'a Registry,
    sequence: &str,
    scope: &str,
    ctx: &BTreeMap<String, String>,
    preset: &str,
) -> Option<&'a Command> {
    let seq = tokens_of(sequence);
    if seq.is_empty() {
        return None;
    }
    let hits: Vec<&Command> = reg
        .commands
        .iter()
        .filter(|c| {
            in_play(c, scope)
                && eval_when(c.when.as_deref(), ctx)
                && c.keys.column(preset).iter().any(|k| {
                    let t = tokens_of(k);
                    t.len() == seq.len() && t.iter().zip(&seq).all(|(tok, s)| token_matches(tok, s))
                })
        })
        .collect();
    if hits.is_empty() {
        return None;
    }
    if seq.len() == 1 && seq[0] == "Escape" {
        return hits
            .into_iter()
            .min_by_key(|c| c.dismiss_order.unwrap_or(i64::MAX));
    }
    hits.iter()
        .find(|c| c.scope == scope)
        .copied()
        .or_else(|| hits.first().copied())
}

/// Every command whose sequence EXTENDS `prefix` — what which-key lists, and
/// what `explain` prints when a key is a prefix rather than a command.
pub fn continuations<'a>(
    reg: &'a Registry,
    prefix: &str,
    scope: &str,
    ctx: &BTreeMap<String, String>,
    preset: &str,
) -> Vec<(&'a Command, String)> {
    let p = tokens_of(prefix);
    let mut out = Vec::new();
    for c in &reg.commands {
        if !in_play(c, scope) || !eval_when(c.when.as_deref(), ctx) {
            continue;
        }
        for k in c.keys.column(preset) {
            let t = tokens_of(k);
            if t.len() <= p.len() {
                continue;
            }
            if !p.iter().enumerate().all(|(i, s)| token_matches(t[i], s)) {
                continue;
            }
            out.push((c, k.clone()));
            break;
        }
    }
    out.sort_by(|a, b| (&a.0.group, &a.1).cmp(&(&b.0.group, &b.1)));
    out
}

// ── conflicts ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Conflict {
    pub preset: String,
    pub key: String,
    pub a: String,
    pub b: String,
    pub kind: ConflictKind,
    pub ratified: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConflictKind {
    /// Two COACTIVE scopes at the SAME modal depth bind one key to different
    /// commands. Nothing resolves this but a human ruling.
    SameDepth,
    /// One scope binds a key twice under `when` predicates that are not
    /// provably disjoint.
    SameScope,
}

/// The generated collision report. A key bound in a DEEPER scope shadows the
/// shallower one BY DESIGN (that is what a modal stack is) and is not a
/// conflict; a `scope: global` key redeclared narrower is a HARD error and is
/// reported separately by [`doctor`], never here — it is not ratifiable.
pub fn conflicts(reg: &Registry) -> Vec<Conflict> {
    let depth: BTreeMap<&str, i64> = reg
        .scopes
        .iter()
        .map(|s| (s.id.as_str(), s.depth))
        .collect();
    let coactive: BTreeMap<&str, BTreeSet<&str>> = reg
        .scopes
        .iter()
        .map(|s| {
            (
                s.id.as_str(),
                s.coactive_with.iter().map(String::as_str).collect(),
            )
        })
        .collect();

    let mut out = Vec::new();
    for preset in reg.presets.iter().map(|p| p.id.as_str()) {
        // key → the rows that bind it, in this preset.
        let mut by_key: BTreeMap<&str, Vec<&Command>> = BTreeMap::new();
        for c in &reg.commands {
            for k in c.keys.column(preset) {
                by_key.entry(k.as_str()).or_default().push(c);
            }
        }
        for (key, rows) in by_key {
            // The dismiss stack is a scope ladder by construction: several
            // Escape rows sharing the key is the DESIGN, resolved by
            // `dismiss_order`. Their ordering is gated separately in `doctor`.
            if key == "Escape" {
                continue;
            }
            for i in 0..rows.len() {
                for j in (i + 1)..rows.len() {
                    let (a, b) = (rows[i], rows[j]);
                    if a.id == b.id {
                        continue;
                    }
                    // `global` vs narrower is the shadowing lint's business.
                    if a.scope == "global" || b.scope == "global" {
                        continue;
                    }
                    let kind = if a.scope == b.scope {
                        if when_disjoint(a.when.as_deref(), b.when.as_deref()) {
                            continue;
                        }
                        ConflictKind::SameScope
                    } else {
                        let co = coactive
                            .get(a.scope.as_str())
                            .is_some_and(|s| s.contains(b.scope.as_str()));
                        let same_depth = depth.get(a.scope.as_str()) == depth.get(b.scope.as_str());
                        if !co || !same_depth {
                            continue; // not coactive, or a designed shadow
                        }
                        if when_disjoint(a.when.as_deref(), b.when.as_deref()) {
                            continue;
                        }
                        ConflictKind::SameDepth
                    };
                    let ratified = a.ratified_conflicts.contains(&b.id)
                        && b.ratified_conflicts.contains(&a.id);
                    out.push(Conflict {
                        preset: preset.to_string(),
                        key: key.to_string(),
                        a: a.id.clone(),
                        b: b.id.clone(),
                        kind,
                        ratified,
                    });
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

// ── doctor: the CI gate ───────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct DoctorReport {
    pub checks: Vec<(String, bool, String)>,
    pub failures: Vec<String>,
    pub notes: Vec<String>,
}

impl DoctorReport {
    fn check(&mut self, name: &str, problems: Vec<String>) {
        let ok = problems.is_empty();
        let detail = if ok {
            String::new()
        } else {
            problems.join("\n    ")
        };
        if !ok {
            for p in &problems {
                self.failures.push(format!("{name}: {p}"));
            }
        }
        self.checks.push((name.to_string(), ok, detail));
    }
    pub fn ok(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Walk the CLI's own clap tree for a `kb-code a b c` path. Cross-joining
/// against the REAL command tree (not a string list) is what stops a `cli:`
/// twin from rotting into a stale string: rename a verb and the gate fails.
fn cli_twin_resolves(root: &clap::Command, twin: &str) -> bool {
    let mut parts = twin.split_whitespace();
    if parts.next() != Some("kb-code") {
        return false;
    }
    let mut cur = root;
    let mut matched_any = false;
    for p in parts {
        if p.starts_with('-') {
            break; // flags/args are not part of the subcommand path
        }
        match cur.find_subcommand(p) {
            Some(sub) => {
                cur = sub;
                matched_any = true;
            }
            None => {
                // A trailing positional (`kb-code cat <PATH>`) is legal; a
                // trailing UNKNOWN SUBCOMMAND is not. Distinguish: if the
                // current node has no subcommands at all, whatever follows
                // must be a positional.
                return matched_any && cur.get_subcommands().next().is_none();
            }
        }
    }
    matched_any
}

/// The gate. Every check the brief names, in one pass, with a machine-
/// readable report so both the `#[test]` and the CLI verb can use it.
pub fn doctor(reg: &Registry, cli: &clap::Command) -> DoctorReport {
    let mut r = DoctorReport::default();

    // 1 — schema + shape (parsing already happened to get here).
    r.check(
        "schema is kbc-cmd/1",
        if reg.schema == "kbc-cmd/1" {
            vec![]
        } else {
            vec![format!("got {:?}", reg.schema)]
        },
    );

    // 2 — ids unique + namespaced.
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut problems = Vec::new();
    for c in &reg.commands {
        if !seen.insert(c.id.as_str()) {
            problems.push(format!("duplicate id {}", c.id));
        }
        if !c.id.contains('.') {
            problems.push(format!("{} is not namespaced", c.id));
        }
    }
    r.check("ids are unique and namespaced", problems);

    // 3 — every scope named is declared, and coactivity is symmetric.
    let declared: BTreeSet<&str> = reg.scopes.iter().map(|s| s.id.as_str()).collect();
    let mut problems = Vec::new();
    for c in &reg.commands {
        if !declared.contains(c.scope.as_str()) {
            problems.push(format!("{} names undeclared scope {}", c.id, c.scope));
        }
    }
    for a in &reg.scopes {
        for b_id in &a.coactive_with {
            match reg.scopes.iter().find(|s| &s.id == b_id) {
                None => problems.push(format!("{} names unknown scope {b_id}", a.id)),
                Some(b) if !b.coactive_with.contains(&a.id) => {
                    problems.push(format!("{b_id} does not list {} back", a.id));
                }
                _ => {}
            }
        }
    }
    r.check("scopes are declared and coactivity is symmetric", problems);

    // 4 — every SHIPPED row's CLI twin resolves in the real clap tree, or is
    // an honest `none:<reason>`.
    let mut problems = Vec::new();
    for c in &reg.commands {
        if c.lifecycle != "shipped" {
            continue;
        }
        if let Some(reason) = c.cli.strip_prefix("none:") {
            if reason.trim().len() < 4 {
                problems.push(format!("{}: `none:` with no reason", c.id));
            }
        } else if !cli_twin_resolves(cli, &c.cli) {
            problems.push(format!("{}: `{}` is not a kb-code verb", c.id, c.cli));
        }
    }
    r.check(
        "every shipped row names a real CLI twin or none:<reason>",
        problems,
    );

    // 5 — reserved browser chords. `hard` can never be claimed; a
    // `passthrough` chord needs an explicit acknowledgement on the row.
    let mut problems = Vec::new();
    let mut acks = 0usize;
    for c in &reg.commands {
        for preset in reg.presets.iter().map(|p| p.id.as_str()) {
            for k in c.keys.column(preset) {
                let Some(first) = tokens_of(k).first().copied() else {
                    continue;
                };
                let reserved = reg.reserved_chords.hard.iter().any(|h| h == first)
                    || reg.reserved_chords.passthrough.iter().any(|h| h == first);
                if !reserved {
                    continue;
                }
                // A reserved chord is claimable ONLY with an explicit
                // acknowledgement on the row: `browser_passthrough` (the row
                // WANTS the browser's behaviour) or a `browser_reserved`
                // string naming the caveat. Both are printed by `doctor` so
                // the compromise stays visible instead of becoming folklore
                // — which is exactly what `Ctrl-w v` was before A5 (the old
                // cheat sheet promised it with no caveat at all, recon R7).
                if c.browser_reserved.is_none() && !c.browser_passthrough {
                    problems.push(format!(
                        "{} binds {first:?} ({preset}) — a reserved browser chord; declare \
                         `browser_passthrough` or acknowledge it in `browser_reserved`",
                        c.id
                    ));
                } else {
                    acks += 1;
                }
            }
        }
    }
    r.check("no row claims a reserved browser chord", problems);
    if acks > 0 {
        r.notes.push(format!(
            "{acks} acknowledged reserved-chord binding(s) — see each row's `browser_reserved`"
        ));
    }

    // 6 — the internal shadowing lint. A `scope: global` key may not be
    // redeclared in a narrower scope: two homes for one keystroke is the
    // exact thing this registry exists to make impossible.
    //
    // Three exemptions, each structural rather than convenient:
    //
    // 1. `Escape` — the dismiss stack IS a scope ladder by construction.
    //    Gated instead by check 7 (a distinct `dismiss_order`).
    // 2. a row with a `vim_kind` — its executor is the CM6 layer, which
    //    installs at `Prec.highest` and stops the event before the window
    //    host ever sees it (and `CommandRoot`'s buffer guard makes that
    //    explicit). `:` is the live case: go-to-line inside the buffer, the
    //    command palette everywhere else.
    // 3. an explicitly RATIFIED pair. The resolver prefers the scope-specific
    //    row, so which one wins is defined; requiring the ratification on
    //    BOTH rows is what makes it a decision somebody wrote down rather
    //    than an accident nobody noticed.
    let mut problems = Vec::new();
    for preset in reg.presets.iter().map(|p| p.id.as_str()) {
        let global_keys: BTreeMap<&str, &Command> = reg
            .commands
            .iter()
            .filter(|c| c.scope == "global")
            .flat_map(|c| c.keys.column(preset).iter().map(move |k| (k.as_str(), c)))
            .filter(|(k, _)| *k != "Escape")
            .collect();
        for c in &reg.commands {
            if c.scope == "global" || c.vim_kind.is_some() {
                continue;
            }
            for k in c.keys.column(preset) {
                let Some(owner) = global_keys.get(k.as_str()) else {
                    continue;
                };
                let ratified = c.ratified_conflicts.contains(&owner.id)
                    && owner.ratified_conflicts.contains(&c.id);
                if !ratified {
                    problems.push(format!(
                        "{} re-declares global key {k:?} ({preset}), owned by {}",
                        c.id, owner.id
                    ));
                }
            }
        }
    }
    r.check("no global key is re-declared in a narrower scope", problems);

    // 7 — the Esc class: every Escape row carries a dismiss_order, orders are
    // distinct, and Esc never navigates or mutates.
    let mut problems = Vec::new();
    let mut orders: BTreeMap<i64, &str> = BTreeMap::new();
    for c in &reg.commands {
        let binds_esc = ["vim", "plain", "helix"]
            .iter()
            .any(|p| c.keys.column(p).iter().any(|k| k == "Escape"));
        if !binds_esc {
            continue;
        }
        match c.dismiss_order {
            None => problems.push(format!("{} binds Escape with no dismiss_order", c.id)),
            Some(o) => {
                if let Some(other) = orders.insert(o, c.id.as_str()) {
                    problems.push(format!("{} and {other} share dismiss_order {o}", c.id));
                }
            }
        }
        if c.id.starts_with("nav.") {
            problems.push(format!("{} — Esc must dismiss, never navigate", c.id));
        }
        if c.mutation != "none" {
            problems.push(format!("{} — a dismissal never mutates", c.id));
        }
    }
    r.check(
        "Esc rows carry a distinct dismiss_order and never navigate",
        problems,
    );

    // 8 — vim_kind rows sit in a scope the CM6 buffer can actually be in.
    let mut problems = Vec::new();
    for c in &reg.commands {
        if c.vim_kind.is_some() && c.scope != "reader" && c.scope != "global" {
            problems.push(format!("{} claims a vim_kind outside the buffer", c.id));
        }
    }
    r.check("vim_kind rows are reader/global", problems);

    // 9 — the conflicts gate. §P2: "v7.0 exits with zero unratified rows."
    let all = conflicts(reg);
    let unratified: Vec<String> = all
        .iter()
        .filter(|c| !c.ratified)
        .map(|c| format!("{} [{}] {} ↔ {} ({:?})", c.key, c.preset, c.a, c.b, c.kind))
        .collect();
    let unratified_count = unratified.len();
    r.check("every conflict is ratified", unratified);
    r.notes.push(format!(
        "{} conflict row(s) total, {} unratified — `kb-code commands conflicts` prints them",
        all.len(),
        unratified_count
    ));

    r
}

// ── rendering ─────────────────────────────────────────────────────────────

fn key_cell(c: &Command, preset: &str) -> String {
    c.keys
        .column(preset)
        .first()
        .cloned()
        .unwrap_or_else(|| "—".to_string())
}

pub fn print_manifest(reg: &Registry, json: bool, md: bool, preset: &str) -> Result<()> {
    if json {
        println!("{}", REGISTRY_JSON.trim_end());
        return Ok(());
    }
    if md {
        println!("# kb-code commands ({})\n", reg.schema);
        println!("Leader: `{}` · preset: `{preset}`\n", reg.leader);
        for s in &reg.scopes {
            let rows: Vec<&Command> = reg.commands.iter().filter(|c| c.scope == s.id).collect();
            if rows.is_empty() {
                continue;
            }
            println!("## {} (`{}`)\n", s.title, s.id);
            println!("{}\n", s.covers);
            println!("| Key | Command | Group | CLI |");
            println!("|---|---|---|---|");
            for c in rows {
                println!(
                    "| `{}` | {}{} | {} | {} |",
                    key_cell(c, preset),
                    c.title,
                    if c.lifecycle == "shipped" {
                        ""
                    } else {
                        " *(planned)*"
                    },
                    c.group,
                    if c.cli.starts_with("none:") {
                        "—".to_string()
                    } else {
                        format!("`{}`", c.cli)
                    }
                );
            }
            println!();
        }
        return Ok(());
    }
    println!(
        "{} · leader {} · {} commands · {} scopes · preset {preset}",
        reg.schema,
        reg.leader,
        reg.commands.len(),
        reg.scopes.len()
    );
    println!("\npresets (columns, never inheritance):");
    for p in &reg.presets {
        println!(
            "  {:<8} {:<10}{} {}",
            p.id,
            p.title,
            if p.default { " (default)" } else { "         " },
            p.note
        );
    }
    println!("\ncontext keys a `when` predicate may name:");
    for k in &reg.context_keys {
        let vals = if k.values.is_empty() {
            String::new()
        } else {
            format!(" — {}", k.values.join(" | "))
        };
        println!("  {}{vals}", k.key);
    }
    println!("\nreserved browser chords:");
    println!("  hard        {}", reg.reserved_chords.hard.join(" "));
    println!(
        "  passthrough {}",
        reg.reserved_chords.passthrough.join(" ")
    );
    for s in &reg.scopes {
        let rows: Vec<&Command> = reg.commands.iter().filter(|c| c.scope == s.id).collect();
        println!("\n{} ({}) — {} row(s)", s.title, s.id, rows.len());
        for c in rows {
            println!("  {:<16} {:<34} {}", key_cell(c, preset), c.id, c.title);
            if !c.aka.is_empty() {
                println!("  {:<16} {:<34} aka {}", "", "", c.aka.join(", "));
            }
        }
    }
    Ok(())
}

pub fn print_cheatsheet(reg: &Registry, scope: &str, md: bool, preset: &str) -> Result<()> {
    if !reg.scopes.iter().any(|s| s.id == scope) {
        bail!(
            "unknown scope {scope:?} — one of: {}",
            reg.scopes
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let title = reg
        .scopes
        .iter()
        .find(|s| s.id == scope)
        .map(|s| s.title.as_str())
        .unwrap_or(scope);
    // Same split the `?` sheet renders: this scope's rows, then the global
    // ones — the two surfaces iterate the same registry order on purpose.
    let local: Vec<&Command> = reg.commands.iter().filter(|c| c.scope == scope).collect();
    let global: Vec<&Command> = reg
        .commands
        .iter()
        .filter(|c| c.scope == "global")
        .collect();

    let section = |heading: &str, rows: &[&Command]| {
        if rows.is_empty() {
            return;
        }
        if md {
            println!("## {heading}\n");
        } else {
            println!("\n{heading}");
        }
        let mut group = "";
        for c in rows {
            if c.group != group {
                group = c.group.as_str();
                if md {
                    println!("\n**{group}**\n");
                    println!("| Key | What |");
                    println!("|---|---|");
                } else {
                    println!("  — {group} —");
                }
            }
            let planned = if c.lifecycle == "shipped" {
                ""
            } else {
                " (planned)"
            };
            if md {
                println!("| `{}` | {}{planned} |", key_cell(c, preset), c.title);
            } else {
                println!("  {:<16} {}{planned}", key_cell(c, preset), c.title);
            }
        }
        println!();
    };

    if md {
        println!("# kb-code keys — {title} (`{preset}`)\n");
    } else {
        println!(
            "kb-code keys — {title} · preset {preset} · leader {}",
            reg.leader
        );
    }
    section(title, &local);
    section("Everywhere", &global);
    Ok(())
}

pub fn print_conflicts(reg: &Registry, json: bool) -> Result<()> {
    let rows = conflicts(reg);
    if json {
        let out: Vec<serde_json::Value> = rows
            .iter()
            .map(|c| {
                serde_json::json!({
                    "preset": c.preset,
                    "key": c.key,
                    "a": c.a,
                    "b": c.b,
                    "kind": format!("{:?}", c.kind),
                    "ratified": c.ratified,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": reg.schema,
                "total": rows.len(),
                "unratified": rows.iter().filter(|c| !c.ratified).count(),
                "conflicts": out,
            }))?
        );
        return Ok(());
    }
    let unratified = rows.iter().filter(|c| !c.ratified).count();
    println!("{} conflict row(s), {unratified} unratified\n", rows.len());
    println!("A conflict is ONE key bound to two different commands in two coactive scopes at the");
    println!(
        "SAME modal depth (a deeper scope shadowing a shallower one is the design, not a bug).\n"
    );
    for c in &rows {
        println!(
            "  {} {:<10} {:<12} {:<32} {:<32} {}",
            if c.ratified { "✓" } else { "✗" },
            c.preset,
            c.key,
            c.a,
            c.b,
            if c.ratified { "ratified" } else { "UNRATIFIED" }
        );
    }
    Ok(())
}

pub fn print_explain(
    reg: &Registry,
    key: &str,
    scope: &str,
    ctx: &BTreeMap<String, String>,
    preset: &str,
    json: bool,
) -> Result<()> {
    if !reg.scopes.iter().any(|s| s.id == scope) {
        bail!("unknown scope {scope:?}");
    }
    let hit = resolve(reg, key, scope, ctx, preset);
    let cont = continuations(reg, key, scope, ctx, preset);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "key": key,
                "scope": scope,
                "preset": preset,
                "context": ctx,
                "resolves_to": hit.map(|c| serde_json::json!({
                    "id": c.id, "title": c.title, "scope": c.scope, "group": c.group,
                    "aka": c.aka, "targets": c.targets, "dispatch": c.dispatch,
                    "affordance_anchor": c.affordance_anchor,
                    "cli": c.cli, "mutation": c.mutation, "side_effect": c.side_effect,
                    "lifecycle": c.lifecycle,
                    "when": c.when, "dismiss_order": c.dismiss_order,
                })),
                "continuations": cont.iter().map(|(c, k)| serde_json::json!({
                    "keys": k, "id": c.id, "title": c.title,
                })).collect::<Vec<_>>(),
            }))?
        );
        return Ok(());
    }
    println!("{key:?} in scope {scope} (preset {preset})");
    if !ctx.is_empty() {
        let bag: Vec<String> = ctx.iter().map(|(k, v)| format!("{k}={v}")).collect();
        println!("  context: {}", bag.join(" "));
    }
    match hit {
        Some(c) => {
            println!("  → {} — {}", c.id, c.title);
            println!(
                "    scope {} · group {} · {}",
                c.scope, c.group, c.lifecycle
            );
            if let Some(w) = &c.when {
                println!("    when: {w}");
            }
            if let Some(o) = c.dismiss_order {
                println!("    dismiss order: {o} (lower dismisses first)");
            }
            println!(
                "    mutation {} · side effect {} · cli {}",
                c.mutation, c.side_effect, c.cli
            );
            // `dispatch` is the two-layer contract in one word: who is
            // expected to own execution at the window level. Surfacing it in
            // `explain` is how "why did nothing happen" gets an answer.
            println!("    dispatch: {} (window-level owner)", c.dispatch);
            if !c.aka.is_empty() {
                println!("    aka: {}", c.aka.join(", "));
            }
            if !c.targets.is_empty() {
                println!("    acts on: {}", c.targets.join(", "));
            }
            if let Some(a) = &c.affordance_anchor {
                println!("    mouse affordance: [data-cmd=\"{a}\"]");
            }
            if let Some(n) = &c.note {
                println!("    note: {n}");
            }
        }
        None if !cont.is_empty() => {
            println!("  → a PREFIX. {} continuation(s):", cont.len());
            for (c, k) in &cont {
                println!("    {k:<16} {} — {}", c.id, c.title);
            }
        }
        None => println!("  → nothing. This key is free in this scope."),
    }
    Ok(())
}

pub fn print_doctor(reg: &Registry, cli: &clap::Command, json: bool) -> Result<()> {
    let rep = doctor(reg, cli);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema": reg.schema,
                "ok": rep.ok(),
                "checks": rep.checks.iter().map(|(n, ok, d)| serde_json::json!({
                    "check": n, "ok": ok, "detail": d,
                })).collect::<Vec<_>>(),
                "failures": rep.failures,
                "notes": rep.notes,
            }))?
        );
    } else {
        println!("kb-code commands doctor — {}\n", reg.schema);
        for (name, ok, detail) in &rep.checks {
            println!("  {} {name}", if *ok { "✓" } else { "✗" });
            if !detail.is_empty() {
                println!("    {detail}");
            }
        }
        for n in &rep.notes {
            println!("\n  · {n}");
        }
    }
    if !rep.ok() {
        bail!("{} registry problem(s)", rep.failures.len());
    }
    Ok(())
}

/// Parse `--context k=v,k2=v2` into the bag the resolver reads.
pub fn parse_context(raw: Option<&str>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(raw) = raw else { return out };
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        match part.split_once('=') {
            Some((k, v)) => out.insert(k.trim().to_string(), v.trim().to_string()),
            // A bare key is the boolean form: `--context help.open` reads the
            // same as `help.open=true`, which is how a human types it.
            None => out.insert(part.to_string(), "true".to_string()),
        };
    }
    out
}
