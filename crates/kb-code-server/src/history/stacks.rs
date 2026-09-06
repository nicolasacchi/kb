//! Stack awareness (V3.3 R10 / V3.3-S2) — detect dependent-branch stacks
//! and serve each layer's INCREMENTAL diff.
//!
//! Operators build dependent-branch stacks (`B` atop `A` atop `main`);
//! reviewing `B` against `main` drowns the reviewer in `A`'s diff. This
//! module SEES the stack (deterministic base detection over local branch
//! tips + first-parent walks) and exposes each layer as `base_tip..tip`.
//!
//! Absolute boundary: pure derivation over existing refs. Never writes a
//! ref, never touches the working tree. All git I/O is blocking —
//! route handlers MUST run it inside `spawn_blocking`.
//!
//! # Base detection (deterministic)
//!
//! For each branch `B ≠ D` (D = default):
//! 1. Same tip as another branch: the lex-smaller name is the base
//!    candidate; both layers get `tip_shared: true`.
//! 2. Otherwise, walk `B`'s first-parent chain (at most
//!    [`MAX_FIRST_PARENT_WALK`] commits). For every other local branch
//!    `X`, compute `mb = merge-base(B, X)`. `X` is a candidate when `mb`
//!    lies on that first-parent walk and is strictly behind `B`'s tip.
//!    Rank candidates by (distance tip→mb ascending, tip-on-chain first,
//!    name ascending). Distance-nearest wins — so a moved base whose fork
//!    point still sits on the chain remains the base (and is later
//!    flagged `stale`).
//! 3. If the nearest hit is the merge-base with D / D's tip (or no other
//!    branch qualifies before the walk ends), the base is D.
//! 4. Walk bound exceeded with no candidate → `unresolved: true` (never
//!    silently classified).
//! 5. Stale: `merge-base(layer, base) ≠ base's tip` — the base moved
//!    since the layer was cut. Surfaced, never "fixed".
//!
//! Chains of base-links form stacks: maximal chains `D ← A ← B ← …`.
//! Single-layer stacks (branch based on D with no dependents) are
//! excluded from the default listing; `include_all` keeps them.

use super::{
    branches::ahead_behind,
    compare::{compare, Compare},
    merge_base, parse_log_summary_line, reject_dash_prefixed, resolve_sha, run_git_raw,
    HistoryError, Result, LOG_SUMMARY_FMT,
};
use crate::git::{RefRange, Revspec};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

/// First-parent walk ceiling per branch — exceeded ⇒ `unresolved: true`.
pub const MAX_FIRST_PARENT_WALK: usize = 2000;

/// One branch tip's identity + summary, for wire/CLI.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LayerTip {
    pub sha: String,
    pub subject: String,
    /// Author unix seconds (`%at`) — same epoch the rest of this crate's
    /// commit surfaces use (`CommitSummary::author_time`, branches
    /// `last.author_time`).
    pub date: i64,
}

/// One stack layer (a branch relative to its detected base).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Layer {
    pub branch: String,
    /// Empty string when [`Layer::unresolved`] — never a guessed base.
    pub base: String,
    pub ahead: u32,
    pub behind: u32,
    pub stale: bool,
    pub tip_shared: bool,
    pub unresolved: bool,
    pub tip: LayerTip,
}

/// One maximal dependent-branch chain from the default branch upward
/// (default itself is NOT listed as a layer — layers are the successors).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Stack {
    pub layers: Vec<Layer>,
}

/// Full detection result for a repo.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Stacks {
    pub default_branch: Option<String>,
    pub stacks: Vec<Stack>,
}

/// Incremental layer diff envelope: the compare shape (files, numstat
/// totals, commits) plus the layer's own identity so the SPA can reuse
/// the compare renderer without a second round-trip.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LayerDiff {
    pub branch: String,
    pub base: String,
    pub base_tip: String,
    pub tip: String,
    pub stale: bool,
    pub compare: Compare,
}

/// A local branch name + tip sha — the same enumeration
/// `routes::branches_route` builds from `GitRepo::list_refs` (kind=Branch).
#[derive(Debug, Clone)]
pub struct BranchTip {
    pub name: String,
    pub tip_sha: String,
}

/// Detect stacks over `branches` with default branch `default`.
///
/// `include_all`: when false (default listing), drop single-layer stacks
/// (a branch based on D with no dependents). When true, keep them.
///
/// Ordering is total and deterministic: stacks sorted by first layer's
/// branch name ascending; layers ordered base-first along the chain.
pub fn detect_stacks(
    repo_root: &Path,
    branches: &[BranchTip],
    default: Option<&str>,
    include_all: bool,
) -> Result<Stacks> {
    let Some(default_name) = default else {
        return Ok(Stacks {
            default_branch: None,
            stacks: vec![],
        });
    };

    // Tip → sorted branch names at that tip (lex order for multi-tip).
    let mut tip_to_names: HashMap<String, Vec<String>> = HashMap::new();
    let mut name_to_tip: BTreeMap<String, String> = BTreeMap::new();
    for b in branches {
        name_to_tip.insert(b.name.clone(), b.tip_sha.clone());
        tip_to_names
            .entry(b.tip_sha.clone())
            .or_default()
            .push(b.name.clone());
    }
    for names in tip_to_names.values_mut() {
        names.sort();
    }

    // Cache tip metadata (sha already known; subject+date via one log).
    let mut tip_meta: HashMap<String, LayerTip> = HashMap::new();
    for b in branches {
        if tip_meta.contains_key(&b.tip_sha) {
            continue;
        }
        tip_meta.insert(b.tip_sha.clone(), load_tip(repo_root, &b.tip_sha)?);
    }

    // base_of[branch] = (base_name, tip_shared, unresolved)
    let mut base_of: BTreeMap<String, (String, bool, bool)> = BTreeMap::new();

    for b in branches {
        if b.name == default_name {
            continue;
        }
        let (base, tip_shared, unresolved) = detect_base(
            repo_root,
            &b.name,
            &b.tip_sha,
            default_name,
            &tip_to_names,
            &name_to_tip,
            MAX_FIRST_PARENT_WALK,
        )?;
        base_of.insert(b.name.clone(), (base, tip_shared, unresolved));
    }

    // Build dependent adjacency: base → sorted children.
    let mut children: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (branch, (base, _, unresolved)) in &base_of {
        if *unresolved || base.is_empty() {
            continue;
        }
        children
            .entry(base.clone())
            .or_default()
            .push(branch.clone());
    }
    for kids in children.values_mut() {
        kids.sort();
    }

    // Emit every maximal chain starting from each direct child of D.
    // A diamond (A←B, A←C) yields two stacks sharing the A layer.
    let mut stacks: Vec<Stack> = Vec::new();
    let direct = children.get(default_name).cloned().unwrap_or_default();
    let ctx = ChainCtx {
        default_name,
        children: &children,
        base_of: &base_of,
        name_to_tip: &name_to_tip,
        tip_meta: &tip_meta,
        repo_root,
    };
    for root_layer in direct {
        emit_chains(&root_layer, &ctx, &mut Vec::new(), &mut stacks)?;
    }

    // Unresolved branches never join a chain via base_of — surface each
    // as its own single-layer stack so the operator still sees them
    // (only when include_all, since a lone unresolved is single-layer).
    for (branch, (base, tip_shared, unresolved)) in &base_of {
        if !*unresolved {
            continue;
        }
        let tip_sha = name_to_tip.get(branch).cloned().unwrap_or_default();
        let tip = tip_meta.get(&tip_sha).cloned().unwrap_or_else(|| LayerTip {
            sha: tip_sha.clone(),
            subject: String::new(),
            date: 0,
        });
        let layer = finish_layer(repo_root, branch, base, *tip_shared, true, tip)?;
        stacks.push(Stack {
            layers: vec![layer],
        });
    }

    // Default listing drops single-layer stacks (real stacks only).
    // Unresolved layers always surface — "never silently classified."
    if !include_all {
        stacks.retain(|s| s.layers.len() >= 2 || s.layers.iter().any(|l| l.unresolved));
    }

    // Total order: first layer's branch name ascending.
    stacks.sort_by(|a, b| {
        let an = a.layers.first().map(|l| l.branch.as_str()).unwrap_or("");
        let bn = b.layers.first().map(|l| l.branch.as_str()).unwrap_or("");
        an.cmp(bn)
    });

    Ok(Stacks {
        default_branch: Some(default_name.to_string()),
        stacks,
    })
}

/// Immutable detection context threaded through the `emit_chains`
/// recursion (one logical unit — not nine loose parameters).
struct ChainCtx<'a> {
    default_name: &'a str,
    children: &'a BTreeMap<String, Vec<String>>,
    base_of: &'a BTreeMap<String, (String, bool, bool)>,
    name_to_tip: &'a BTreeMap<String, String>,
    tip_meta: &'a HashMap<String, LayerTip>,
    repo_root: &'a Path,
}

/// DFS: extend the current path from `branch` (whose base is already
/// known) and push a stack whenever `branch` has no further dependents
/// (a leaf of the base-link tree). Shared prefixes (diamonds) are emitted
/// once per leaf path so each maximal chain is present.
fn emit_chains(
    branch: &str,
    ctx: &ChainCtx<'_>,
    path: &mut Vec<Layer>,
    out: &mut Vec<Stack>,
) -> Result<()> {
    let (base, tip_shared, unresolved) = ctx
        .base_of
        .get(branch)
        .cloned()
        .unwrap_or_else(|| (ctx.default_name.to_string(), false, false));
    if unresolved {
        // Unresolved branches are handled separately.
        return Ok(());
    }
    // Defensive backstop: `may_be_base`'s direction filter should make
    // base-links acyclic, but nothing PROVES it for exotic multi-branch
    // divergence topologies — and an undetected cycle here would recurse
    // to stack overflow inside a route handler. If the branch is already
    // on the current path, emit the chain up to this point (visible, not
    // silently dropped) and stop.
    if path.iter().any(|l| l.branch == branch) {
        out.push(Stack {
            layers: path.clone(),
        });
        return Ok(());
    }
    let tip_sha = ctx
        .name_to_tip
        .get(branch)
        .cloned()
        .ok_or_else(|| HistoryError::GitFailed {
            status: -1,
            stderr: format!("missing tip for branch {branch:?}"),
        })?;
    let tip = ctx
        .tip_meta
        .get(&tip_sha)
        .cloned()
        .ok_or_else(|| HistoryError::GitFailed {
            status: -1,
            stderr: format!("missing tip meta for {tip_sha}"),
        })?;
    let layer = finish_layer(ctx.repo_root, branch, &base, tip_shared, false, tip)?;
    path.push(layer);

    let kids = ctx.children.get(branch).cloned().unwrap_or_default();
    if kids.is_empty() {
        out.push(Stack {
            layers: path.clone(),
        });
    } else {
        for kid in kids {
            emit_chains(&kid, ctx, path, out)?;
        }
    }
    path.pop();
    Ok(())
}

fn finish_layer(
    repo_root: &Path,
    branch: &str,
    base: &str,
    tip_shared: bool,
    unresolved: bool,
    tip: LayerTip,
) -> Result<Layer> {
    let (ahead, behind, stale) = if unresolved || base.is_empty() {
        (0, 0, false)
    } else {
        // Branch names come from ref ENUMERATION, but git permits a ref
        // whose name starts with `-` — so a repo-controlled string still
        // reaches argv here. V70-A2 validates it through the same
        // `Revspec` constructor a caller-supplied ref goes through
        // (strictly stronger than the `reject_dash_prefixed` `is_stale`
        // does one line below), and fails closed.
        let ab = ahead_behind(
            repo_root,
            &RefRange::new(Revspec::parse(base)?, Revspec::parse(branch)?, true),
        )?;
        let stale = is_stale(repo_root, branch, base)?;
        (ab.ahead, ab.behind, stale)
    };
    Ok(Layer {
        branch: branch.to_string(),
        base: base.to_string(),
        ahead,
        behind,
        stale,
        tip_shared,
        unresolved,
        tip,
    })
}

/// `merge-base(layer, base) ≠ base's tip` ⇒ the base moved since the
/// layer was cut.
fn is_stale(repo_root: &Path, layer: &str, base: &str) -> Result<bool> {
    // Branch names come from ref enumeration, but git permits refs that
    // START with `-` — the same argv-injection surface `resolve_sha`'s
    // module doc guards `compare` against. Fail closed, never pass a
    // dash-prefixed name into a composed revspec.
    reject_dash_prefixed(layer)?;
    reject_dash_prefixed(base)?;
    let layer_sha = resolve_sha(repo_root, layer)?;
    let base_sha = resolve_sha(repo_root, base)?;
    match merge_base(repo_root, &layer_sha, &base_sha) {
        Some(mb) => Ok(mb != base_sha),
        None => Ok(true), // disjoint histories — honest "not on base tip"
    }
}

/// Detect base for one branch. Returns `(base_name, tip_shared, unresolved)`.
///
/// `max_walk` is the first-parent ceiling (production:
/// [`MAX_FIRST_PARENT_WALK`]; tests may lower it to exercise the bound).
fn detect_base(
    repo_root: &Path,
    branch: &str,
    tip_sha: &str,
    default_name: &str,
    tip_to_names: &HashMap<String, Vec<String>>,
    name_to_tip: &BTreeMap<String, String>,
    max_walk: usize,
) -> Result<(String, bool, bool)> {
    // Fast path: same-tip group — lex-smallest name is the base candidate
    // for every larger name; both sides get tip_shared.
    if let Some(names) = tip_to_names.get(tip_sha) {
        if names.len() > 1 {
            let smallest = names[0].as_str();
            if smallest != branch {
                return Ok((smallest.to_string(), true, false));
            }
            // We ARE the lex-smallest — continue to score other branches
            // for our own base; tip_shared stays true (spec: "on both").
        }
    }

    let tip_shared = tip_to_names
        .get(tip_sha)
        .map(|n| n.len() > 1)
        .unwrap_or(false);

    // Fetch one extra so we can tell "exactly max_walk" from "more exist".
    let full = first_parent_chain(repo_root, tip_sha, max_walk.saturating_add(1))?;
    let truncated = full.len() > max_walk;
    let chain = &full[..full.len().min(max_walk)];
    let chain_idx: HashMap<&str, usize> = chain
        .iter()
        .enumerate()
        .map(|(i, s)| (s.as_str(), i))
        .collect();
    let chain_set: HashSet<&str> = chain.iter().map(String::as_str).collect();

    // Rank key: (dist tip→mb, tip_not_on_chain, name). Lower wins.
    // dist=0 (mb at B's tip) means X is B or a descendant — never a base.
    // tip_on_chain preferred so a live ancestor tip beats a sibling that
    // merely shares an older merge-base at the same distance.
    //
    // Direction filter (cycle break): when A advances after B was cut from
    // A, both tips diverge from the old fork — without a filter each would
    // pick the other as base (A↔B cycle, no chain from D). Allow X as base
    // of B only when tip(X) is an ancestor of tip(B), or (diverged/stale)
    // tip(B) is NOT an ancestor of tip(X) and X ranks "below" B toward D
    // (fewer first-parent commits from D, else lex-smaller name).
    let mut best: Option<(usize, u8, String)> = None;
    let default_tip_sha = name_to_tip.get(default_name).cloned();

    for (x_name, x_tip) in name_to_tip {
        if x_name == branch {
            continue;
        }
        // Skip same-tip cohort members (already handled / not our base).
        if x_tip == tip_sha {
            continue;
        }
        let Some(mb) = merge_base(repo_root, tip_sha, x_tip) else {
            continue;
        };
        let Some(&dist) = chain_idx.get(mb.as_str()) else {
            // merge-base not on the bounded first-parent walk.
            continue;
        };
        if dist == 0 {
            continue;
        }
        if !may_be_base(
            repo_root,
            x_name,
            x_tip,
            branch,
            tip_sha,
            default_tip_sha.as_deref(),
        )? {
            continue;
        }
        let tip_on_chain: u8 = if chain_set.contains(x_tip.as_str()) {
            0
        } else {
            1
        };
        let key = (dist, tip_on_chain, x_name.clone());
        match &best {
            None => best = Some(key),
            Some(cur) if key < *cur => best = Some(key),
            _ => {}
        }
    }

    if let Some((_dist, _, name)) = best {
        return Ok((name, tip_shared, false));
    }

    // No other-branch candidate inside the walk window.
    if truncated {
        return Ok((String::new(), tip_shared, true));
    }

    // Exhausted first-parent chain (or only D would qualify and D was
    // absent from name_to_tip — still default to D).
    Ok((default_name.to_string(), tip_shared, false))
}

/// Whether `x` may be the base of `branch` (cycle-breaking direction filter).
fn may_be_base(
    repo_root: &Path,
    x_name: &str,
    x_tip: &str,
    branch: &str,
    branch_tip: &str,
    default_tip: Option<&str>,
) -> Result<bool> {
    // Classic: X's tip is an ancestor of B's tip.
    if is_ancestor(repo_root, x_tip, branch_tip)? {
        return Ok(true);
    }
    // Child cannot be the base of its ancestor.
    if is_ancestor(repo_root, branch_tip, x_tip)? {
        return Ok(false);
    }
    // Diverged (stale-base candidate): only allow the side "closer to D"
    // (or lex-smaller name on a tie) so A↔B cycles cannot form.
    let dx = match default_tip {
        Some(d) => fp_distance(repo_root, d, x_tip)?,
        None => u32::MAX,
    };
    let db = match default_tip {
        Some(d) => fp_distance(repo_root, d, branch_tip)?,
        None => u32::MAX,
    };
    if dx != db {
        return Ok(dx < db);
    }
    Ok(x_name < branch)
}

/// `git merge-base --is-ancestor a b` — true when `a` is an ancestor of `b`.
fn is_ancestor(repo_root: &Path, a: &str, b: &str) -> Result<bool> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["merge-base", "--is-ancestor", a, b])
        .output()
        .map_err(HistoryError::Spawn)?;
    // exit 0 = yes, 1 = no, other = error
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(HistoryError::GitFailed {
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        }),
    }
}

/// First-parent commit count from `from` to `to` (`from..to` exclusive of
/// `from`) — used only for the diverged-base direction tie-break.
fn fp_distance(repo_root: &Path, from: &str, to: &str) -> Result<u32> {
    let revspec = format!("{from}..{to}");
    let out = run_git_raw(
        repo_root,
        &["rev-list", "--first-parent", "--count", &revspec],
    )?;
    let text = String::from_utf8_lossy(&out).trim().to_string();
    text.parse().map_err(|_| HistoryError::GitFailed {
        status: -1,
        stderr: format!("malformed rev-list --count output: {text:?}"),
    })
}

/// `git rev-list --first-parent -n <limit> <tip>` — newest-first full shas.
fn first_parent_chain(repo_root: &Path, tip_sha: &str, limit: usize) -> Result<Vec<String>> {
    let n = limit.to_string();
    let out = run_git_raw(
        repo_root,
        &["rev-list", "--first-parent", "-n", &n, tip_sha],
    )?;
    let text = String::from_utf8_lossy(&out);
    Ok(text
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

fn load_tip(repo_root: &Path, sha: &str) -> Result<LayerTip> {
    let out = run_git_raw(
        repo_root,
        &["log", "-1", &format!("--format={LOG_SUMMARY_FMT}"), sha],
    )?;
    let line = String::from_utf8_lossy(&out);
    let line = line.lines().next().unwrap_or("");
    let summary = parse_log_summary_line(line).ok_or_else(|| HistoryError::GitFailed {
        status: -1,
        stderr: format!("malformed tip log for {sha}: {line:?}"),
    })?;
    Ok(LayerTip {
        sha: summary.sha,
        subject: summary.subject,
        date: summary.author_time,
    })
}

/// Incremental diff of one layer: `base_tip..branch_tip` (two-dot),
/// reusing [`compare`] so the response shape matches `/api/compare`.
///
/// When the layer is stale we still diff against the RECORDED base tip
/// (honest incremental view of what the layer actually contains vs the
/// base as detection named it).
pub fn layer_diff(
    repo_root: &Path,
    branches: &[BranchTip],
    default: Option<&str>,
    branch: &Revspec,
) -> Result<LayerDiff> {
    // V70-A2 (SEC-17) — `?branch=` is caller-supplied and reaches
    // `resolve_sha`/`compare` below, so it arrives as a validated
    // `Revspec` rather than a bare `&str` a guard has to remember.
    let branch = branch.as_str();
    if default == Some(branch) {
        return Err(HistoryError::GitFailed {
            status: -1,
            stderr: format!("branch {branch:?} is the default branch — not a stack layer"),
        });
    }
    let tip_entry =
        branches
            .iter()
            .find(|b| b.name == branch)
            .ok_or_else(|| HistoryError::GitFailed {
                status: -1,
                stderr: format!("unknown branch {branch:?}"),
            })?;

    // Re-run detection for this one branch (cheap relative to the diff).
    let stacks = detect_stacks(repo_root, branches, default, true)?;
    let layer = stacks
        .stacks
        .iter()
        .flat_map(|s| s.layers.iter())
        .find(|l| l.branch == branch)
        .ok_or_else(|| HistoryError::GitFailed {
            status: -1,
            stderr: format!("branch {branch:?} not present in stack detection"),
        })?;

    if layer.unresolved || layer.base.is_empty() {
        return Err(HistoryError::GitFailed {
            status: -1,
            stderr: format!("branch {branch:?} base is unresolved — cannot compute layer-diff"),
        });
    }

    let base_tip = resolve_sha(repo_root, &layer.base)?;
    let tip = tip_entry.tip_sha.clone();
    // Two-dot: exact commits unique to the layer vs its base tip.
    // Both endpoints are shas this daemon just resolved itself, never
    // caller text — `Revspec::trusted` is the honest spelling of that.
    let cmp = compare(
        repo_root,
        &Revspec::trusted(base_tip.clone()),
        &Revspec::trusted(tip.clone()),
        false,
    )?;

    Ok(LayerDiff {
        branch: branch.to_string(),
        base: layer.base.clone(),
        base_tip,
        tip,
        stale: layer.stale,
        compare: cmp,
    })
}

/// Test-only: detect stacks with a custom first-parent walk ceiling.
#[cfg(test)]
fn detect_stacks_with_walk_limit(
    repo_root: &Path,
    branches: &[BranchTip],
    default: Option<&str>,
    include_all: bool,
    max_walk: usize,
) -> Result<Stacks> {
    let Some(default_name) = default else {
        return Ok(Stacks {
            default_branch: None,
            stacks: vec![],
        });
    };
    let mut tip_to_names: HashMap<String, Vec<String>> = HashMap::new();
    let mut name_to_tip: BTreeMap<String, String> = BTreeMap::new();
    for b in branches {
        name_to_tip.insert(b.name.clone(), b.tip_sha.clone());
        tip_to_names
            .entry(b.tip_sha.clone())
            .or_default()
            .push(b.name.clone());
    }
    for names in tip_to_names.values_mut() {
        names.sort();
    }
    let mut tip_meta: HashMap<String, LayerTip> = HashMap::new();
    for b in branches {
        if tip_meta.contains_key(&b.tip_sha) {
            continue;
        }
        tip_meta.insert(b.tip_sha.clone(), load_tip(repo_root, &b.tip_sha)?);
    }
    let mut base_of: BTreeMap<String, (String, bool, bool)> = BTreeMap::new();
    for b in branches {
        if b.name == default_name {
            continue;
        }
        let (base, tip_shared, unresolved) = detect_base(
            repo_root,
            &b.name,
            &b.tip_sha,
            default_name,
            &tip_to_names,
            &name_to_tip,
            max_walk,
        )?;
        base_of.insert(b.name.clone(), (base, tip_shared, unresolved));
    }
    // Only care about unresolved labeling in this helper.
    let mut stacks = Vec::new();
    for (branch, (base, tip_shared, unresolved)) in &base_of {
        let tip_sha = name_to_tip.get(branch).cloned().unwrap_or_default();
        let tip = tip_meta.get(&tip_sha).cloned().unwrap_or(LayerTip {
            sha: tip_sha,
            subject: String::new(),
            date: 0,
        });
        let layer = finish_layer(repo_root, branch, base, *tip_shared, *unresolved, tip)?;
        stacks.push(Stack {
            layers: vec![layer],
        });
    }
    if !include_all {
        stacks.retain(|s| s.layers.iter().any(|l| l.unresolved) || s.layers.len() >= 2);
    }
    Ok(Stacks {
        default_branch: Some(default_name.to_string()),
        stacks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;

    fn git(dir: &Path, args: &[&str]) {
        let out = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git -C {} {:?} failed: {}",
            dir.display(),
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn git_out(dir: &Path, args: &[&str]) -> String {
        let out = StdCommand::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn init_repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        git(dir, &["config", "user.name", "Test"]);
        // Deterministic timestamps so tip.date is stable across runs.
        git(dir, &["config", "commit.gpgsign", "false"]);
        tmp
    }

    fn commit(dir: &Path, file: &str, contents: &str, msg: &str) {
        std::fs::write(dir.join(file), contents).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", msg]);
    }

    fn list_branch_tips(dir: &Path) -> Vec<BranchTip> {
        let out = git_out(
            dir,
            &[
                "for-each-ref",
                "--format=%(refname:short) %(objectname)",
                "refs/heads",
            ],
        );
        let mut tips: Vec<BranchTip> = out
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| {
                let mut parts = l.splitn(2, ' ');
                let name = parts.next().unwrap().to_string();
                let tip_sha = parts.next().unwrap().to_string();
                BranchTip { name, tip_sha }
            })
            .collect();
        tips.sort_by(|a, b| a.name.cmp(&b.name));
        tips
    }

    /// D ← A ← B chain fixture.
    fn stack_fixture() -> tempfile::TempDir {
        let tmp = init_repo();
        let dir = tmp.path();
        commit(dir, "base.txt", "base\n", "base");
        git(dir, &["checkout", "-q", "-b", "A"]);
        commit(dir, "a.txt", "a\n", "A one");
        git(dir, &["checkout", "-q", "-b", "B"]);
        commit(dir, "b.txt", "b\n", "B one");
        git(dir, &["checkout", "-q", "main"]);
        tmp
    }

    #[test]
    fn detects_two_layer_stack_d_a_b() {
        let tmp = stack_fixture();
        let dir = tmp.path();
        let tips = list_branch_tips(dir);
        let stacks = detect_stacks(dir, &tips, Some("main"), false).unwrap();
        assert_eq!(stacks.default_branch.as_deref(), Some("main"));
        assert_eq!(
            stacks.stacks.len(),
            1,
            "one real stack: {:?}",
            stacks.stacks
        );
        let layers = &stacks.stacks[0].layers;
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0].branch, "A");
        assert_eq!(layers[0].base, "main");
        assert_eq!(layers[0].ahead, 1);
        assert!(!layers[0].stale);
        assert!(!layers[0].unresolved);
        assert_eq!(layers[1].branch, "B");
        assert_eq!(layers[1].base, "A");
        assert_eq!(layers[1].ahead, 1);
        assert!(!layers[1].stale);
    }

    #[test]
    fn single_layer_off_d_excluded_by_default_included_with_all() {
        let tmp = init_repo();
        let dir = tmp.path();
        commit(dir, "base.txt", "base\n", "base");
        git(dir, &["checkout", "-q", "-b", "feature"]);
        commit(dir, "f.txt", "f\n", "feature one");
        git(dir, &["checkout", "-q", "main"]);

        let tips = list_branch_tips(dir);
        let def = detect_stacks(dir, &tips, Some("main"), false).unwrap();
        assert!(
            def.stacks.is_empty(),
            "single-layer excluded by default: {:?}",
            def.stacks
        );

        let all = detect_stacks(dir, &tips, Some("main"), true).unwrap();
        assert_eq!(all.stacks.len(), 1);
        assert_eq!(all.stacks[0].layers.len(), 1);
        assert_eq!(all.stacks[0].layers[0].branch, "feature");
        assert_eq!(all.stacks[0].layers[0].base, "main");
    }

    #[test]
    fn stale_layer_when_base_advances_after_cut() {
        let tmp = stack_fixture();
        let dir = tmp.path();
        // Advance A after B was cut from it.
        git(dir, &["checkout", "-q", "A"]);
        commit(dir, "a2.txt", "a2\n", "A two (after B cut)");
        git(dir, &["checkout", "-q", "main"]);

        let tips = list_branch_tips(dir);
        let stacks = detect_stacks(dir, &tips, Some("main"), false).unwrap();
        assert_eq!(
            stacks.stacks.len(),
            1,
            "expected one multi-layer stack: {stacks:?}"
        );
        let layers = &stacks.stacks[0].layers;
        // B's base is still A (nearest fork on first-parent), but stale.
        let b = layers.iter().find(|l| l.branch == "B").expect("B layer");
        assert_eq!(b.base, "A", "B base wrong: {stacks:?}");
        assert!(b.stale, "B should be stale after A advanced: {b:?}");
        // A itself is not stale vs main.
        let a = layers.iter().find(|l| l.branch == "A").expect("A layer");
        assert!(!a.stale);
    }

    #[test]
    fn same_tip_tie_break_lex_smaller_is_base() {
        let tmp = init_repo();
        let dir = tmp.path();
        commit(dir, "base.txt", "base\n", "base");
        git(dir, &["checkout", "-q", "-b", "zebra"]);
        commit(dir, "z.txt", "z\n", "zebra one");
        // Point apple at the same tip as zebra.
        git(dir, &["branch", "apple", "zebra"]);
        git(dir, &["checkout", "-q", "main"]);

        let tips = list_branch_tips(dir);
        // With only same-tip pair on main: apple base? zebra base?
        // apple (lex smaller) walks to main; zebra's base = apple (tip_shared).
        let all = detect_stacks(dir, &tips, Some("main"), true).unwrap();
        // Maximal chain: main ← apple ← zebra  OR  main ← apple  and zebra on apple.
        // Default listing wants multi-layer: main ← apple ← zebra.
        let stacks = detect_stacks(dir, &tips, Some("main"), false).unwrap();
        assert_eq!(stacks.stacks.len(), 1, "got: {:?}", stacks.stacks);
        let layers = &stacks.stacks[0].layers;
        assert_eq!(layers[0].branch, "apple");
        assert_eq!(layers[0].base, "main");
        assert!(layers[0].tip_shared);
        assert_eq!(layers[1].branch, "zebra");
        assert_eq!(layers[1].base, "apple");
        assert!(layers[1].tip_shared);
        assert_eq!(layers[1].ahead, 0);
        assert_eq!(layers[1].behind, 0);
        let _ = all;
    }

    #[test]
    fn walk_bound_marks_unresolved_when_ceiling_too_low() {
        // A linear feature branch several commits long with a walk
        // ceiling of 2: the first-parent chain is longer than 2 and
        // neither D's tip nor any other branch tip sits inside the
        // window → unresolved (never silently classified as based on D).
        let tmp = init_repo();
        let dir = tmp.path();
        commit(dir, "base.txt", "base\n", "base");
        git(dir, &["checkout", "-q", "-b", "long"]);
        for i in 1..=5 {
            commit(
                dir,
                &format!("l{i}.txt"),
                &format!("{i}\n"),
                &format!("long {i}"),
            );
        }
        git(dir, &["checkout", "-q", "main"]);
        let tips = list_branch_tips(dir);
        let stacks = detect_stacks_with_walk_limit(dir, &tips, Some("main"), true, 2).unwrap();
        let long = stacks
            .stacks
            .iter()
            .flat_map(|s| s.layers.iter())
            .find(|l| l.branch == "long")
            .expect("long layer");
        assert!(long.unresolved, "expected unresolved, got {long:?}");
        assert!(long.base.is_empty());
    }

    #[test]
    fn layer_diff_of_b_shows_only_b_files() {
        let tmp = stack_fixture();
        let dir = tmp.path();
        let tips = list_branch_tips(dir);
        for s in detect_stacks(dir, &tips, Some("main"), true)
            .unwrap()
            .stacks
        {
            for l in s.layers {
                assert!(!l.unresolved, "{l:?}");
            }
        }

        let ld = layer_diff(dir, &tips, Some("main"), &Revspec::parse("B").unwrap()).unwrap();
        assert_eq!(ld.base, "A");
        assert!(!ld.stale);
        let paths: Vec<&str> = ld.compare.files.iter().map(|f| f.path.as_str()).collect();
        assert!(
            paths.contains(&"b.txt"),
            "B layer-diff must include B's file: {paths:?}"
        );
        assert!(
            !paths.contains(&"a.txt"),
            "B layer-diff must NOT include A's file: {paths:?}"
        );
        assert!(
            !paths.contains(&"base.txt"),
            "B layer-diff must NOT include base: {paths:?}"
        );
    }

    #[test]
    fn stacks_sorted_by_first_layer_name() {
        let tmp = init_repo();
        let dir = tmp.path();
        commit(dir, "base.txt", "base\n", "base");
        // Two independent two-layer stacks: main←y←y2 and main←x←x2
        git(dir, &["checkout", "-q", "-b", "y"]);
        commit(dir, "y.txt", "y\n", "y");
        git(dir, &["checkout", "-q", "-b", "y2"]);
        commit(dir, "y2.txt", "y2\n", "y2");
        git(dir, &["checkout", "-q", "main"]);
        git(dir, &["checkout", "-q", "-b", "x"]);
        commit(dir, "x.txt", "x\n", "x");
        git(dir, &["checkout", "-q", "-b", "x2"]);
        commit(dir, "x2.txt", "x2\n", "x2");
        git(dir, &["checkout", "-q", "main"]);

        let tips = list_branch_tips(dir);
        let stacks = detect_stacks(dir, &tips, Some("main"), false).unwrap();
        assert_eq!(stacks.stacks.len(), 2);
        assert_eq!(stacks.stacks[0].layers[0].branch, "x");
        assert_eq!(stacks.stacks[1].layers[0].branch, "y");
    }

    #[test]
    fn no_default_yields_empty() {
        let tmp = init_repo();
        let dir = tmp.path();
        commit(dir, "base.txt", "base\n", "base");
        let tips = list_branch_tips(dir);
        let stacks = detect_stacks(dir, &tips, None, true).unwrap();
        assert!(stacks.stacks.is_empty());
        assert!(stacks.default_branch.is_none());
    }
}
