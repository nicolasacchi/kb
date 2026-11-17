//! `kb bench recall` — SG-M1: the memory-recall policy bench.
//!
//! A harness for validating recall POLICY (what gets injected, and how many
//! off-topic memories ride along), not a leaderboard. It is internal policy
//! validation: the fixture is labelled by the operator, the corpus is a frozen
//! copy served by a throwaway daemon, and the report carries aggregates only.
//!
//! Data path: for each fixture row the bench calls `GET /api/memory/recall`
//! on the daemon named by `--daemon` with `no_floor=true` and the route's hard
//! maximum `limit` ([`POOL_CAP`] = 50), once with the bare prompt and once
//! with the SG-Q2 composed query (`compose_recall_query`). The returned hits
//! plus their score decomposition are the POOL. Every arm is then computed
//! here, from the pool, with a pinned clock:
//!
//! - `shipped`: the route's own order.
//! - `content`: retrieval-rank order only (no salience/decay).
//! - `ce`: cross-encoder logit as the base, two bounded multipliers
//!   (salience, decay), and a logit floor. Needs the `kb-embedder` sidecar
//!   (invariant #26: kb-cli links no ONNX); when the binary is missing the arm
//!   is reported as skipped, not failed.
//!
//! The two no-reranker floor arms from the plan (vector-cosine floor,
//! BM25-and-vector agreement) are NOT implemented: the recall route returns
//! no per-arm raw scores (no cosine, no BM25 rank), and this phase must not
//! change the route.
//!
//! Report hygiene: the report writer refuses to emit any string that
//! originates in the fixture (query, context) or the pool (memory title,
//! body); see [`check_no_leak`].

use crate::http;
use anyhow::{anyhow, bail, Context, Result};
use kb_core::sessions::view::Role;
use kb_core::sessions::{compose_recall_query, RecentTurn, RECALL_QUERY_DEFAULT_TOTAL_CHARS};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// The recall route's hard `limit` cap (`MAX_LIMIT` in kb-server's
/// `routes/memory.rs`, a private const): the pool is top-N with N = 50.
pub const POOL_CAP: usize = 50;
const WILSON_Z: f64 = 1.96;
const SCHEMA: &str = "kb-recall-bench/1";

// ---- fixture --------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Class {
    Legacy,
    Real,
    Null,
    Trivial,
}

impl Class {
    fn as_str(self) -> &'static str {
        match self {
            Class::Legacy => "legacy",
            Class::Real => "real",
            Class::Null => "null",
            Class::Trivial => "trivial",
        }
    }
    /// Rows where no memory at grade >= 1 exists (false-injection rows).
    fn is_negative(self) -> bool {
        matches!(self, Class::Null | Class::Trivial)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Split {
    Dev,
    Heldout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum CtxRole {
    Human,
    Assistant,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CtxTurn {
    role: CtxRole,
    text: String,
}

/// One fixture row (one JSONL line). `context` is chronological, oldest
/// first (the same order `compose_recall_query` takes). `labels` keys are a
/// memory id, bare or `kb:id`; unlisted memories are grade 0.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureRow {
    pub id: String,
    pub query: String,
    #[serde(default)]
    pub context: Vec<CtxTurn>,
    #[serde(default)]
    pub labels: BTreeMap<String, u8>,
    pub class: Class,
    pub language: String,
    pub split: Split,
}

impl FixtureRow {
    fn grade_of(&self, kb: &str, id: &str) -> u8 {
        self.labels
            .get(&format!("{kb}:{id}"))
            .or_else(|| self.labels.get(id))
            .copied()
            .unwrap_or(0)
    }
    fn n_grade2(&self) -> usize {
        self.labels.values().filter(|g| **g == 2).count()
    }
    fn turns(&self) -> Vec<RecentTurn> {
        self.context
            .iter()
            .map(|t| RecentTurn {
                role: match t.role {
                    CtxRole::Human => Role::Human,
                    CtxRole::Assistant => Role::Assistant,
                },
                text: t.text.clone(),
            })
            .collect()
    }
    fn composed_query(&self) -> String {
        compose_recall_query(&self.query, &self.turns(), RECALL_QUERY_DEFAULT_TOTAL_CHARS)
    }
}

fn valid_language(s: &str) -> bool {
    let ok_len = (2..=8).contains(&s.len());
    ok_len
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Strict JSONL loader. Every error names the 1-based line.
pub fn load_fixture(text: &str) -> Result<Vec<FixtureRow>> {
    let mut rows: Vec<FixtureRow> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (i, line) in text.lines().enumerate() {
        let ln = i + 1;
        if line.trim().is_empty() {
            continue;
        }
        let row: FixtureRow = serde_json::from_str(line)
            .map_err(|e| anyhow!("fixture line {ln}: invalid row: {e}"))?;
        if row.id.trim().is_empty() || row.id.chars().any(char::is_whitespace) {
            bail!("fixture line {ln}: `id` must be non-empty with no whitespace");
        }
        if !seen.insert(row.id.clone()) {
            bail!("fixture line {ln}: duplicate row id {:?}", row.id);
        }
        if row.query.trim().is_empty() {
            bail!("fixture line {ln}: `query` is empty");
        }
        if !valid_language(&row.language) {
            bail!(
                "fixture line {ln}: `language` must be a lowercase tag of 2-8 chars \
                 ([a-z0-9-]), e.g. \"en\" or \"it\""
            );
        }
        for (k, g) in &row.labels {
            if k.trim().is_empty() || k.chars().any(char::is_whitespace) {
                bail!("fixture line {ln}: label key {k:?} must be a non-empty memory id");
            }
            if *g > 2 {
                bail!(
                    "fixture line {ln}: label grade for {k:?} is {g}; allowed grades are 0, 1, 2"
                );
            }
        }
        for (j, t) in row.context.iter().enumerate() {
            if t.text.trim().is_empty() {
                bail!("fixture line {ln}: context turn {} has empty text", j + 1);
            }
        }
        let max_grade = row.labels.values().copied().max().unwrap_or(0);
        if row.class.is_negative() && max_grade >= 1 {
            bail!(
                "fixture line {ln}: class {:?} means no memory at grade >= 1 exists, \
                 but a label has grade {max_grade}",
                row.class.as_str()
            );
        }
        if !row.class.is_negative() && max_grade == 0 {
            bail!(
                "fixture line {ln}: class {:?} needs at least one label with grade >= 1 \
                 (otherwise use class \"null\")",
                row.class.as_str()
            );
        }
        rows.push(row);
    }
    if rows.is_empty() {
        bail!("fixture has no rows");
    }
    Ok(rows)
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

// ---- pool -----------------------------------------------------------------

/// One recall hit with the decomposition the bench needs.
#[derive(Debug, Clone)]
pub struct PoolHit {
    pub kb: String,
    pub id: String,
    pub title: String,
    pub rank: u32,
    pub salience: f32,
    pub decay: f32,
    pub decay_k: Option<f32>,
    pub age_days: Option<f32>,
    /// Unix seconds the pool was fetched at (age_days is relative to it).
    pub fetched_unix: i64,
}

#[derive(Debug, Clone, Default)]
pub struct RowPool {
    pub plain: Vec<PoolHit>,
    /// `None` = the row has no usable context; the plain pool is reused.
    pub ctx: Option<Vec<PoolHit>>,
}

impl RowPool {
    fn form(&self, f: Form) -> &[PoolHit] {
        match f {
            Form::Plain => &self.plain,
            Form::Context => self.ctx.as_deref().unwrap_or(&self.plain),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Form {
    Plain,
    Context,
}

impl Form {
    fn as_str(self) -> &'static str {
        match self {
            Form::Plain => "plain",
            Form::Context => "context",
        }
    }
}

fn parse_hits(v: &serde_json::Value, fetched_unix: i64) -> Vec<PoolHit> {
    let arr = v.get("hits").and_then(|h| h.as_array());
    arr.map(|a| a.as_slice())
        .unwrap_or(&[])
        .iter()
        .enumerate()
        .filter_map(|(i, h)| {
            let id = h.get("id")?.as_str()?.to_string();
            let kb = h
                .get("kb")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let f = |k: &str| h.get(k).and_then(|x| x.as_f64()).map(|x| x as f32);
            Some(PoolHit {
                kb,
                id,
                title: h
                    .get("title")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                rank: h
                    .get("rank")
                    .and_then(|x| x.as_u64())
                    .map(|x| x as u32)
                    .unwrap_or(i as u32 + 1),
                salience: f("salience").unwrap_or(1.0),
                decay: f("decay").unwrap_or(1.0),
                decay_k: f("decay_k"),
                age_days: f("age_days"),
                fetched_unix,
            })
        })
        .collect()
}

async fn fetch_pool(client: &reqwest::Client, base: &str, q: &str) -> Result<Vec<PoolHit>> {
    let req = client.get(format!("{base}/api/memory/recall")).query(&[
        ("q", q),
        ("scope", "all"),
        ("limit", &POOL_CAP.to_string()),
        ("no_floor", "true"),
    ]);
    let v = http::send_json(req, "GET /api/memory/recall").await?;
    Ok(parse_hits(&v, chrono::Utc::now().timestamp()))
}

// ---- arms -----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Arm {
    Shipped,
    Content,
    Ce,
}

impl Arm {
    fn as_str(self) -> &'static str {
        match self {
            Arm::Shipped => "shipped",
            Arm::Content => "content",
            Arm::Ce => "ce",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct CeParams {
    /// Salience multiplier `1 - c + c * salience`, in `[1 - c, 1]`.
    pub salience_coef: f32,
    /// Decay multiplier `1 - c + c * decay_at_clock`, in `[1 - c, 1]`.
    pub decay_coef: f32,
    /// Hits with a logit below this are dropped.
    pub floor: f32,
}

/// Decay factor at the pinned clock: `exp(-k * age)`, with `age` re-based
/// from the fetch instant to `clock`. Falls back to the route's own `decay`
/// when the decomposition lacks `decay_k`/`age_days`.
fn decay_at(h: &PoolHit, clock_unix: i64) -> f32 {
    match (h.decay_k, h.age_days) {
        (Some(k), Some(age)) => {
            let shift = (h.fetched_unix - clock_unix) as f32 / 86_400.0;
            (-k * (age - shift).max(0.0)).exp().clamp(0.0, 1.0)
        }
        _ => h.decay.clamp(0.0, 1.0),
    }
}

/// Order of pool indices an arm would inject, best first. `logits` is
/// aligned with `pool` and only read by [`Arm::Ce`].
///
/// ce: hits with `logit < floor` are dropped; survivors are ranked by
/// `(logit - floor) * m_sal * m_decay`. The shift keeps the product monotone
/// in the logit when the floor is negative (multiplying a negative logit by a
/// factor below 1 would otherwise RAISE it).
pub fn arm_order(
    arm: Arm,
    pool: &[PoolHit],
    logits: Option<&[f32]>,
    ce: &CeParams,
    clock_unix: i64,
) -> Vec<usize> {
    match arm {
        Arm::Shipped => (0..pool.len()).collect(),
        Arm::Content => {
            let mut idx: Vec<usize> = (0..pool.len()).collect();
            idx.sort_by_key(|&i| pool[i].rank);
            idx
        }
        Arm::Ce => {
            let Some(l) = logits.filter(|l| l.len() == pool.len()) else {
                return Vec::new();
            };
            let mut scored: Vec<(usize, f32)> = (0..pool.len())
                .filter(|&i| l[i] >= ce.floor)
                .map(|i| {
                    let h = &pool[i];
                    let ms = 1.0 - ce.salience_coef + ce.salience_coef * h.salience.clamp(0.0, 1.0);
                    let md = 1.0 - ce.decay_coef + ce.decay_coef * decay_at(h, clock_unix);
                    (i, (l[i] - ce.floor) * ms * md)
                })
                .collect();
            scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            scored.into_iter().map(|(i, _)| i).collect()
        }
    }
}

// ---- metrics --------------------------------------------------------------

/// Per-row outcome of one arm.
#[derive(Debug, Clone, PartialEq)]
pub struct RowOutcome {
    /// grade-2 labels in the arm's top 5 / grade-2 labels of the row
    /// (`None` when the row has no grade-2 label).
    pub recall5: Option<f64>,
    pub rr: Option<f64>,
    /// grade-2 labels present anywhere in the pool / grade-2 labels.
    pub pool_recall: Option<f64>,
    pub injected: usize,
    pub offtopic: usize,
}

pub fn outcome(
    row: &FixtureRow,
    grades_in_order: &[u8],
    pool_grade2: usize,
    inject_k: usize,
) -> RowOutcome {
    let n2 = row.n_grade2();
    let top5_2 = grades_in_order.iter().take(5).filter(|g| **g == 2).count();
    let first2 = grades_in_order.iter().position(|g| *g == 2);
    let inj = &grades_in_order[..grades_in_order.len().min(inject_k)];
    RowOutcome {
        recall5: (n2 > 0).then(|| top5_2 as f64 / n2 as f64),
        rr: (n2 > 0).then(|| first2.map_or(0.0, |p| 1.0 / (p as f64 + 1.0))),
        pool_recall: (n2 > 0).then(|| pool_grade2 as f64 / n2 as f64),
        injected: inj.len(),
        offtopic: inj.iter().filter(|g| **g == 0).count(),
    }
}

/// Wilson score interval upper bound at 95%. `None` when `n == 0`.
pub fn wilson_upper(k: usize, n: usize) -> Option<f64> {
    if n == 0 {
        return None;
    }
    let (k, n) = (k as f64, n as f64);
    let p = k / n;
    let z2 = WILSON_Z * WILSON_Z;
    let centre = p + z2 / (2.0 * n);
    let margin = WILSON_Z * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt();
    Some(((centre + margin) / (1.0 + z2 / n)).min(1.0))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FalseInj {
    pub n: usize,
    pub injecting: usize,
    pub rate: f64,
    pub wilson95_upper: f64,
    pub mean_injected: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Paired {
    pub wins: usize,
    pub losses: usize,
    pub ties: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Stratum {
    pub n: usize,
    /// Rows with a grade-2 label (the recall@5 / MRR denominator).
    pub n_graded: usize,
    pub recall_at_5: Option<f64>,
    pub mrr: Option<f64>,
    pub pool_recall_at_50: Option<f64>,
    pub injected_per_prompt: Option<f64>,
    pub offtopic_per_prompt: Option<f64>,
    pub false_injection: Option<FalseInj>,
    /// Paired recall@5 comparison against `shipped` (absent for shipped).
    pub paired_vs_shipped: Option<Paired>,
}

fn mean(xs: impl Iterator<Item = f64>) -> Option<f64> {
    let (s, n) = xs.fold((0.0, 0usize), |(s, n), x| (s + x, n + 1));
    (n > 0).then(|| s / n as f64)
}

/// Aggregate the rows at `idx`. `shipped` is the shipped arm's outcomes
/// (same indexing) for the paired table.
pub fn stratum(
    rows: &[FixtureRow],
    out: &[RowOutcome],
    shipped: Option<&[RowOutcome]>,
    idx: &[usize],
) -> Stratum {
    let graded: Vec<usize> = idx
        .iter()
        .copied()
        .filter(|&i| out[i].recall5.is_some())
        .collect();
    let neg: Vec<usize> = idx
        .iter()
        .copied()
        .filter(|&i| rows[i].class.is_negative())
        .collect();
    let injecting = neg.iter().filter(|&&i| out[i].injected > 0).count();
    let paired = shipped.map(|s| {
        let mut p = Paired {
            wins: 0,
            losses: 0,
            ties: 0,
        };
        for &i in &graded {
            let (a, b) = (out[i].recall5.unwrap_or(0.0), s[i].recall5.unwrap_or(0.0));
            if (a - b).abs() < 1e-12 {
                p.ties += 1;
            } else if a > b {
                p.wins += 1;
            } else {
                p.losses += 1;
            }
        }
        p
    });
    Stratum {
        n: idx.len(),
        n_graded: graded.len(),
        recall_at_5: mean(graded.iter().filter_map(|&i| out[i].recall5)),
        mrr: mean(graded.iter().filter_map(|&i| out[i].rr)),
        pool_recall_at_50: mean(graded.iter().filter_map(|&i| out[i].pool_recall)),
        injected_per_prompt: mean(idx.iter().map(|&i| out[i].injected as f64)),
        offtopic_per_prompt: mean(idx.iter().map(|&i| out[i].offtopic as f64)),
        false_injection: (!neg.is_empty()).then(|| FalseInj {
            n: neg.len(),
            injecting,
            rate: injecting as f64 / neg.len() as f64,
            wilson95_upper: wilson_upper(injecting, neg.len()).unwrap_or(1.0),
            mean_injected: mean(neg.iter().map(|&i| out[i].injected as f64)).unwrap_or(0.0),
        }),
        paired_vs_shipped: paired,
    }
}

/// `all`, `class=<c>`, `lang=<l>` strata (only non-empty ones), as row-index
/// lists.
pub fn strata_index(rows: &[FixtureRow]) -> BTreeMap<String, Vec<usize>> {
    let mut m: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, r) in rows.iter().enumerate() {
        m.entry("all".into()).or_default().push(i);
        m.entry(format!("class={}", r.class.as_str()))
            .or_default()
            .push(i);
        m.entry(format!("lang={}", r.language)).or_default().push(i);
    }
    m
}

/// Nearest-rank percentile of `xs` (sorted copy). `None` when empty.
pub fn percentile(xs: &[f64], p: f64) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    let mut s = xs.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let rank = ((p / 100.0) * s.len() as f64).ceil().max(1.0) as usize;
    Some(s[rank.min(s.len()) - 1])
}

// ---- report ---------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Latency {
    pub cold_start_ms: f64,
    pub rerank_calls: usize,
    pub warm_p50_ms: Option<f64>,
    pub warm_p95_ms: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SweepPoint {
    pub floor: f32,
    pub false_injection: Option<FalseInj>,
    pub recall_at_5: Option<f64>,
    pub offtopic_per_prompt: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub schema: &'static str,
    pub daemon_build_sha: String,
    pub fixture_hash: String,
    pub fixture_rows: usize,
    pub scored_rows: usize,
    pub split: Option<Split>,
    pub clock_unix: i64,
    pub pool_cap: usize,
    pub inject_k: usize,
    pub reranker_model: String,
    pub ce_params: CeParams,
    pub ce_status: String,
    pub latency: Option<Latency>,
    /// form -> arm -> stratum -> aggregates.
    pub forms: BTreeMap<String, BTreeMap<String, BTreeMap<String, Stratum>>>,
    pub sweep: Option<Vec<SweepPoint>>,
}

pub struct Inputs<'a> {
    pub rows: &'a [FixtureRow],
    pub pools: &'a [RowPool],
    /// Per row, cross-encoder logits aligned with the plain / context pool.
    pub logits: Option<&'a [RowLogits]>,
    pub ce: CeParams,
    pub clock_unix: i64,
    pub inject_k: usize,
}

#[derive(Debug, Clone, Default)]
pub struct RowLogits {
    pub plain: Vec<f32>,
    pub ctx: Option<Vec<f32>>,
}

impl RowLogits {
    fn form(&self, f: Form) -> &[f32] {
        match f {
            Form::Plain => &self.plain,
            Form::Context => self.ctx.as_deref().unwrap_or(&self.plain),
        }
    }
}

fn outcomes_for(inp: &Inputs, arm: Arm, form: Form, ce: &CeParams) -> Vec<RowOutcome> {
    inp.rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let pool = inp.pools[i].form(form);
            let logits = inp.logits.map(|l| l[i].form(form));
            let order = arm_order(arm, pool, logits, ce, inp.clock_unix);
            let grades: Vec<u8> = order
                .iter()
                .map(|&j| row.grade_of(&pool[j].kb, &pool[j].id))
                .collect();
            let pool_g2 = pool
                .iter()
                .filter(|h| row.grade_of(&h.kb, &h.id) == 2)
                .count();
            outcome(row, &grades, pool_g2, inp.inject_k)
        })
        .collect()
}

/// Pure: every arm x form x stratum aggregate. `ce` is skipped when no
/// logits are supplied.
pub fn compute_forms(
    inp: &Inputs,
) -> BTreeMap<String, BTreeMap<String, BTreeMap<String, Stratum>>> {
    let strata = strata_index(inp.rows);
    let mut forms = BTreeMap::new();
    for form in [Form::Plain, Form::Context] {
        let shipped = outcomes_for(inp, Arm::Shipped, form, &inp.ce);
        let mut arms = BTreeMap::new();
        let mut arm_list = vec![Arm::Shipped, Arm::Content];
        if inp.logits.is_some() {
            arm_list.push(Arm::Ce);
        }
        for arm in arm_list {
            let out = if arm == Arm::Shipped {
                shipped.clone()
            } else {
                outcomes_for(inp, arm, form, &inp.ce)
            };
            let per: BTreeMap<String, Stratum> = strata
                .iter()
                .map(|(k, idx)| {
                    let sh = (arm != Arm::Shipped).then_some(shipped.as_slice());
                    (k.clone(), stratum(inp.rows, &out, sh, idx))
                })
                .collect();
            arms.insert(arm.as_str().to_string(), per);
        }
        forms.insert(form.as_str().to_string(), arms);
    }
    forms
}

/// Operating curve of the ce arm over `floors`, on the rows given (the dev
/// split), for one query form.
pub fn sweep(inp: &Inputs, form: Form, floors: &[f32]) -> Vec<SweepPoint> {
    let all: Vec<usize> = (0..inp.rows.len()).collect();
    floors
        .iter()
        .map(|&f| {
            let p = CeParams { floor: f, ..inp.ce };
            let out = outcomes_for(inp, Arm::Ce, form, &p);
            let s = stratum(inp.rows, &out, None, &all);
            SweepPoint {
                floor: f,
                false_injection: s.false_injection,
                recall_at_5: s.recall_at_5,
                offtopic_per_prompt: s.offtopic_per_prompt,
            }
        })
        .collect()
}

fn f3(x: Option<f64>) -> String {
    x.map_or("-".to_string(), |v| format!("{v:.3}"))
}

pub fn render_markdown(r: &Report) -> String {
    let mut s = String::new();
    s.push_str("# Recall bench (aggregates only)\n\n");
    s.push_str(&format!("- schema: {}\n", r.schema));
    s.push_str(&format!("- daemon build sha: {}\n", r.daemon_build_sha));
    s.push_str(&format!("- fixture hash: {}\n", r.fixture_hash));
    s.push_str(&format!(
        "- rows: {} in fixture, {} scored, split: {}\n",
        r.fixture_rows,
        r.scored_rows,
        r.split
            .map_or("all", |p| if p == Split::Dev { "dev" } else { "heldout" })
    ));
    s.push_str(&format!("- pinned clock (unix): {}\n", r.clock_unix));
    s.push_str(&format!(
        "- pool cap: top {} (route hard maximum)\n",
        r.pool_cap
    ));
    s.push_str(&format!("- injected per prompt (k): {}\n", r.inject_k));
    s.push_str(&format!("- reranker model: {}\n", r.reranker_model));
    s.push_str(&format!(
        "- ce params: salience_coef {}, decay_coef {}, floor {}\n",
        r.ce_params.salience_coef, r.ce_params.decay_coef, r.ce_params.floor
    ));
    s.push_str(&format!("- ce arm: {}\n", r.ce_status));
    if let Some(l) = &r.latency {
        s.push_str(&format!(
            "- rerank latency ms: cold start {:.0}, warm p50 {}, warm p95 {} ({} calls)\n",
            l.cold_start_ms,
            f3(l.warm_p50_ms),
            f3(l.warm_p95_ms),
            l.rerank_calls
        ));
    }
    for (form, arms) in &r.forms {
        s.push_str(&format!("\n## Query form: {form}\n"));
        let strata: BTreeSet<&String> = arms.values().flat_map(|m| m.keys()).collect();
        for st in strata {
            let n = arms.values().find_map(|m| m.get(st)).map_or(0, |x| x.n);
            s.push_str(&format!("\n### {st} (n={n})\n\n"));
            s.push_str("| arm | graded n | recall@5 | MRR | pool recall@50 | injected/prompt | off-topic/prompt | false-inj (k/n) | Wilson95 upper |\n");
            s.push_str("|---|---|---|---|---|---|---|---|---|\n");
            for (arm, m) in arms {
                let Some(x) = m.get(st) else { continue };
                let (fk, fu) = x
                    .false_injection
                    .as_ref()
                    .map_or(("-".into(), "-".into()), |f| {
                        (
                            format!("{:.3} ({}/{})", f.rate, f.injecting, f.n),
                            format!("{:.3}", f.wilson95_upper),
                        )
                    });
                s.push_str(&format!(
                    "| {arm} | {} | {} | {} | {} | {} | {} | {fk} | {fu} |\n",
                    x.n_graded,
                    f3(x.recall_at_5),
                    f3(x.mrr),
                    f3(x.pool_recall_at_50),
                    f3(x.injected_per_prompt),
                    f3(x.offtopic_per_prompt),
                ));
            }
            let paired: Vec<_> = arms
                .iter()
                .filter_map(|(a, m)| {
                    m.get(st)
                        .and_then(|x| x.paired_vs_shipped.as_ref())
                        .map(|p| (a, p))
                })
                .collect();
            if !paired.is_empty() {
                s.push_str("\nPaired recall@5 vs shipped:\n\n| arm | wins | losses | ties |\n|---|---|---|---|\n");
                for (a, p) in paired {
                    s.push_str(&format!(
                        "| {a} | {} | {} | {} |\n",
                        p.wins, p.losses, p.ties
                    ));
                }
            }
        }
    }
    if let Some(sw) = &r.sweep {
        s.push_str("\n## Floor sweep (ce arm, dev split)\n\n| floor | false-inj rate | Wilson95 upper | recall@5 | off-topic/prompt |\n|---|---|---|---|---|\n");
        for p in sw {
            let (rate, up) = p
                .false_injection
                .as_ref()
                .map_or(("-".into(), "-".into()), |f| {
                    (format!("{:.3}", f.rate), format!("{:.3}", f.wilson95_upper))
                });
            s.push_str(&format!(
                "| {} | {rate} | {up} | {} | {} |\n",
                p.floor,
                f3(p.recall_at_5),
                f3(p.offtopic_per_prompt)
            ));
        }
    }
    s
}

// ---- leak check -----------------------------------------------------------

const LEAK_WINDOW: usize = 24;
const LEAK_STRIDE: usize = 4;

fn norm(s: &str) -> Vec<char> {
    let mut out = Vec::with_capacity(s.len());
    let mut sp = true;
    for c in s.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_whitespace() {
            if !sp {
                out.push(' ');
            }
            sp = true;
        } else {
            out.push(c);
            sp = false;
        }
    }
    while out.last() == Some(&' ') {
        out.pop();
    }
    out
}

fn hash_window(w: &[char]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    w.hash(&mut h);
    h.finish()
}

fn contains_word(hay: &[char], needle: &[char]) -> bool {
    if needle.is_empty() || hay.len() < needle.len() {
        return false;
    }
    (0..=hay.len() - needle.len()).any(|i| {
        hay[i..i + needle.len()] == *needle
            && (i == 0 || !hay[i - 1].is_alphanumeric())
            && (i + needle.len() == hay.len() || !hay[i + needle.len()].is_alphanumeric())
    })
}

/// Refuse a rendered report that contains text taken from the fixture or the
/// pool. Whitespace/case-normalised. Strings under 3 chars are ignored; up to
/// 7 chars they must match as a whole word; up to the window size, as a
/// substring; longer strings are caught by any shared 24-char run (a stride-4
/// scan of the forbidden text against every window of the report, so any leak
/// of 27+ chars is caught). The error names WHICH source class leaked, never
/// the leaked text.
pub fn check_no_leak(rendered: &str, forbidden: &[(&'static str, String)]) -> Result<()> {
    let hay = norm(rendered);
    let windows: HashSet<u64> = if hay.len() >= LEAK_WINDOW {
        hay.windows(LEAK_WINDOW).map(hash_window).collect()
    } else {
        HashSet::new()
    };
    for (kind, text) in forbidden {
        let n = norm(text);
        let leaked = if n.len() < 3 {
            false
        } else if n.len() < 8 {
            contains_word(&hay, &n)
        } else if n.len() < LEAK_WINDOW + LEAK_STRIDE {
            hay.windows(n.len()).any(|w| w == n.as_slice())
        } else {
            (0..=n.len() - LEAK_WINDOW)
                .step_by(LEAK_STRIDE)
                .any(|i| windows.contains(&hash_window(&n[i..i + LEAK_WINDOW])))
        };
        if leaked {
            bail!("refusing to write the report: it contains text from a fixture/pool {kind}");
        }
    }
    Ok(())
}

/// Render both outputs, check them, and only then write anything.
pub fn write_report(
    report: &Report,
    forbidden: &[(&'static str, String)],
    md_path: Option<&Path>,
    json_path: Option<&Path>,
) -> Result<String> {
    let md = render_markdown(report);
    let json = serde_json::to_string_pretty(report)?;
    check_no_leak(&md, forbidden)?;
    check_no_leak(&json, forbidden)?;
    if let Some(p) = json_path {
        std::fs::write(p, &json).with_context(|| format!("write {}", p.display()))?;
    }
    if let Some(p) = md_path {
        std::fs::write(p, &md).with_context(|| format!("write {}", p.display()))?;
    }
    Ok(md)
}

fn forbidden_texts(
    rows: &[FixtureRow],
    pools: &[RowPool],
    bodies: &BTreeMap<(String, String), String>,
) -> Vec<(&'static str, String)> {
    let mut v: Vec<(&'static str, String)> = Vec::new();
    for r in rows {
        v.push(("query", r.query.clone()));
        v.push(("query", r.composed_query()));
        for t in &r.context {
            v.push(("context", t.text.clone()));
        }
    }
    let mut titles: BTreeSet<&str> = BTreeSet::new();
    for p in pools {
        for h in p.plain.iter().chain(p.ctx.iter().flatten()) {
            titles.insert(&h.title);
        }
    }
    v.extend(titles.into_iter().map(|t| ("memory title", t.to_string())));
    v.extend(bodies.values().map(|b| ("memory body", b.clone())));
    v
}

// ---- sidecar --------------------------------------------------------------

/// One rerank request: a query and its candidate documents (pool order).
pub struct RerankJob {
    pub query: String,
    pub docs: Vec<String>,
}

/// Score every job through the sidecar. Returns per-job logits (input order)
/// and per-call wall times in ms.
pub fn rerank_jobs(
    client: &mut kb_core::embed_ipc::RerankerClient,
    jobs: &[RerankJob],
) -> Result<(Vec<Vec<f32>>, Vec<f64>)> {
    let mut all = Vec::with_capacity(jobs.len());
    let mut times = Vec::new();
    for j in jobs {
        if j.docs.is_empty() {
            all.push(Vec::new());
            continue;
        }
        let t = Instant::now();
        let ranked = client.rerank(&j.query, &j.docs, j.docs.len())?;
        times.push(t.elapsed().as_secs_f64() * 1000.0);
        let mut logits = vec![f32::NEG_INFINITY; j.docs.len()];
        for (i, s) in ranked {
            logits[i] = s;
        }
        all.push(logits);
    }
    Ok((all, times))
}

/// Visible text of an HTML artifact: script/style/template blocks removed,
/// tags stripped, common entities decoded, whitespace collapsed.
pub fn html_to_text(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut keep = String::with_capacity(html.len());
    let mut i = 0;
    'outer: while i < html.len() {
        for tag in ["script", "style", "template"] {
            if lower[i..].starts_with(&format!("<{tag}")) {
                let close = format!("</{tag}>");
                match lower[i..].find(&close) {
                    Some(e) => i += e + close.len(),
                    None => i = html.len(),
                }
                keep.push(' ');
                continue 'outer;
            }
        }
        let c = html[i..].chars().next().unwrap();
        keep.push(c);
        i += c.len_utf8();
    }
    let mut out = String::with_capacity(keep.len());
    let mut in_tag = false;
    for c in keep.chars() {
        match c {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn pair_text(title: &str, body: Option<&str>, cap: usize) -> String {
    let mut t = match body {
        Some(b) if !b.is_empty() => format!("{title}\n{b}"),
        _ => title.to_string(),
    };
    if cap > 0 && t.chars().count() > cap {
        t = t.chars().take(cap).collect();
    }
    t
}

// ---- CLI ------------------------------------------------------------------

#[derive(clap::Args, Debug, Clone)]
pub struct RecallArgs {
    /// Fixture JSONL (one query row per line; see docs/cli.md).
    #[arg(long)]
    pub fixture: PathBuf,
    /// URL of the THROWAWAY daemon serving a frozen copy of the memory
    /// corpora (required: the bench never guesses a daemon).
    #[arg(long)]
    pub daemon: String,
    /// Pinned clock for the decay factor: unix seconds or RFC 3339.
    #[arg(long)]
    pub clock: String,
    /// Score only this split (`dev` or `heldout`). Default: every row.
    #[arg(long)]
    pub split: Option<String>,
    /// Memories injected per prompt (the hook's `--limit`).
    #[arg(long, default_value_t = 5)]
    pub inject_k: usize,
    /// Reranker model for the ce arm.
    #[arg(long, default_value = "bge-reranker-base")]
    pub reranker_model: String,
    /// Skip the ce arm without spawning the sidecar.
    #[arg(long)]
    pub skip_ce: bool,
    /// ce: salience multiplier coefficient in [0,1] (`1 - c + c*salience`).
    #[arg(long, default_value_t = 0.3)]
    pub ce_salience_coef: f32,
    /// ce: decay multiplier coefficient in [0,1] (`1 - c + c*decay`).
    #[arg(long, default_value_t = 0.3)]
    pub ce_decay_coef: f32,
    /// ce: logit floor; hits below it are dropped.
    #[arg(long, default_value_t = 0.0, allow_negative_numbers = true)]
    pub ce_floor: f32,
    /// ce: cap the (title + body) text given to the reranker, in chars
    /// (0 = no cap).
    #[arg(long, default_value_t = 0)]
    pub body_chars: usize,
    /// Print the ce floor operating curve on the dev split.
    #[arg(long)]
    pub sweep: bool,
    #[arg(long, default_value_t = -6.0, allow_negative_numbers = true)]
    pub sweep_from: f32,
    #[arg(long, default_value_t = 6.0, allow_negative_numbers = true)]
    pub sweep_to: f32,
    #[arg(long, default_value_t = 1.0)]
    pub sweep_step: f32,
    /// Query form the sweep uses: `plain` or `context`.
    #[arg(long, default_value = "plain")]
    pub sweep_form: String,
    /// Markdown report path (empty = stdout).
    #[arg(long)]
    pub output: Option<PathBuf>,
    /// JSON report path.
    #[arg(long)]
    pub json: Option<PathBuf>,
}

fn parse_clock(s: &str) -> Result<i64> {
    if let Ok(n) = s.trim().parse::<i64>() {
        return Ok(n);
    }
    chrono::DateTime::parse_from_rfc3339(s.trim())
        .map(|d| d.timestamp())
        .map_err(|_| anyhow!("--clock must be unix seconds or RFC 3339, got {s:?}"))
}

fn parse_split(s: &str) -> Result<Split> {
    match s {
        "dev" => Ok(Split::Dev),
        "heldout" => Ok(Split::Heldout),
        o => bail!("--split must be `dev` or `heldout`, got {o:?}"),
    }
}

pub async fn run(args: RecallArgs, bearer: Option<&str>) -> Result<()> {
    for (n, v) in [
        ("--ce-salience-coef", args.ce_salience_coef),
        ("--ce-decay-coef", args.ce_decay_coef),
    ] {
        if !(0.0..=1.0).contains(&v) {
            bail!("{n} must be within [0, 1], got {v}");
        }
    }
    if args.inject_k == 0 {
        bail!("--inject-k must be >= 1");
    }
    let clock = parse_clock(&args.clock)?;
    let split = args.split.as_deref().map(parse_split).transpose()?;
    let sweep_form = match args.sweep_form.as_str() {
        "plain" => Form::Plain,
        "context" => Form::Context,
        o => bail!("--sweep-form must be `plain` or `context`, got {o:?}"),
    };
    if args.sweep && split == Some(Split::Heldout) {
        bail!("--sweep fits the floor and runs on the dev split only; it cannot be combined with --split heldout");
    }
    if args.sweep && !(args.sweep_step > 0.0 && args.sweep_to >= args.sweep_from) {
        bail!("--sweep needs --sweep-step > 0 and --sweep-to >= --sweep-from");
    }

    let bytes = std::fs::read(&args.fixture)
        .with_context(|| format!("read fixture {}", args.fixture.display()))?;
    let fixture_hash = sha256_hex(&bytes);
    let all_rows = load_fixture(std::str::from_utf8(&bytes).context("fixture is not UTF-8")?)?;
    let fixture_rows = all_rows.len();
    let rows: Vec<FixtureRow> = all_rows
        .iter()
        .filter(|r| split.is_none_or(|s| r.split == s))
        .cloned()
        .collect();
    if rows.is_empty() {
        bail!("no fixture rows in the requested split");
    }

    let base = args.daemon.trim_end_matches('/').to_string();
    let client = http::client_with_timeout_and_bearer(60, bearer)?;
    let ident = http::send_json(
        client.get(format!("{base}/api/identity")),
        "GET /api/identity",
    )
    .await?;
    let build_sha = ident
        .get("build_sha")
        .and_then(|x| x.as_str())
        .unwrap_or("unknown")
        .to_string();

    // Pools.
    let mut pools: Vec<RowPool> = Vec::with_capacity(rows.len());
    for r in &rows {
        let plain = fetch_pool(&client, &base, &r.query).await?;
        let composed = r.composed_query();
        let ctx = if composed != r.query {
            Some(fetch_pool(&client, &base, &composed).await?)
        } else {
            None
        };
        pools.push(RowPool { plain, ctx });
    }

    // ce arm: sidecar + bodies.
    let mut bodies: BTreeMap<(String, String), String> = BTreeMap::new();
    let mut logits: Option<Vec<RowLogits>> = None;
    let mut latency: Option<Latency> = None;
    let ce_status;
    if args.skip_ce {
        ce_status = "skipped (--skip-ce)".to_string();
    } else if let Err(e) = kb_core::embed_ipc::locate_embedder_bin() {
        ce_status = format!("skipped (kb-embedder binary not found: {e})");
    } else {
        let mut need: BTreeSet<(String, String)> = BTreeSet::new();
        for p in &pools {
            for h in p.plain.iter().chain(p.ctx.iter().flatten()) {
                need.insert((h.kb.clone(), h.id.clone()));
            }
        }
        for (kb, id) in need {
            let url = format!(
                "{base}/api/kb/{}/artifact/{}",
                http::encode_path_segment(&kb),
                http::encode_path_segment(&id)
            );
            let resp = client.get(&url).send().await?;
            if resp.status().is_success() {
                bodies.insert((kb, id), html_to_text(&resp.text().await?));
            }
        }
        let mut jobs: Vec<RerankJob> = Vec::new();
        for (i, r) in rows.iter().enumerate() {
            let mk = |q: String, pool: &[PoolHit]| RerankJob {
                query: q,
                docs: pool
                    .iter()
                    .map(|h| {
                        pair_text(
                            &h.title,
                            bodies
                                .get(&(h.kb.clone(), h.id.clone()))
                                .map(String::as_str),
                            args.body_chars,
                        )
                    })
                    .collect(),
            };
            jobs.push(mk(r.query.clone(), &pools[i].plain));
            if let Some(c) = &pools[i].ctx {
                jobs.push(mk(r.composed_query(), c));
            }
        }
        let model = args.reranker_model.clone();
        let spawned = tokio::task::spawn_blocking(move || -> Result<_> {
            let cache =
                kb_core::embed::models_cache_dir(&kb_core::paths::KbPaths::new("default")?.cache);
            let t0 = Instant::now();
            let mut c = kb_core::embed_ipc::RerankerClient::spawn_ipc(&model, cache, 19)?;
            let cold = t0.elapsed().as_secs_f64() * 1000.0;
            let (scores, times) = rerank_jobs(&mut c, &jobs)?;
            Ok((cold, scores, times))
        })
        .await?;
        match spawned {
            Ok((cold, scores, times)) => {
                let mut it = scores.into_iter();
                let mut per_row = Vec::with_capacity(rows.len());
                for p in &pools {
                    let plain = it.next().unwrap_or_default();
                    let ctx = p.ctx.as_ref().map(|_| it.next().unwrap_or_default());
                    per_row.push(RowLogits { plain, ctx });
                }
                logits = Some(per_row);
                // The first call pays model warm-up; warm stats exclude it.
                let warm: &[f64] = if times.len() > 1 { &times[1..] } else { &times };
                latency = Some(Latency {
                    cold_start_ms: cold,
                    rerank_calls: times.len(),
                    warm_p50_ms: percentile(warm, 50.0),
                    warm_p95_ms: percentile(warm, 95.0),
                });
                ce_status = "ran".to_string();
            }
            Err(e) => ce_status = format!("skipped (sidecar failed: {e})"),
        }
    }

    let ce = CeParams {
        salience_coef: args.ce_salience_coef,
        decay_coef: args.ce_decay_coef,
        floor: args.ce_floor,
    };
    let inp = Inputs {
        rows: &rows,
        pools: &pools,
        logits: logits.as_deref(),
        ce,
        clock_unix: clock,
        inject_k: args.inject_k,
    };
    let forms = compute_forms(&inp);
    let sweep_pts = if args.sweep {
        if logits.is_none() {
            bail!("--sweep needs the ce arm, which was skipped: {ce_status}");
        }
        let dev: Vec<usize> = (0..rows.len())
            .filter(|&i| rows[i].split == Split::Dev)
            .collect();
        let dev_rows: Vec<FixtureRow> = dev.iter().map(|&i| rows[i].clone()).collect();
        let dev_pools: Vec<RowPool> = dev.iter().map(|&i| pools[i].clone()).collect();
        let dev_logits: Vec<RowLogits> = dev
            .iter()
            .map(|&i| logits.as_ref().unwrap()[i].clone())
            .collect();
        if dev_rows.is_empty() {
            bail!("--sweep: the fixture has no dev rows");
        }
        let d = Inputs {
            rows: &dev_rows,
            pools: &dev_pools,
            logits: Some(&dev_logits),
            ce,
            clock_unix: clock,
            inject_k: args.inject_k,
        };
        let mut floors = Vec::new();
        let mut f = args.sweep_from;
        while f <= args.sweep_to + 1e-6 {
            floors.push((f * 1000.0).round() / 1000.0);
            f += args.sweep_step;
        }
        Some(sweep(&d, sweep_form, &floors))
    } else {
        None
    };

    let report = Report {
        schema: SCHEMA,
        daemon_build_sha: build_sha,
        fixture_hash,
        fixture_rows,
        scored_rows: rows.len(),
        split,
        clock_unix: clock,
        pool_cap: POOL_CAP,
        inject_k: args.inject_k,
        reranker_model: args.reranker_model.clone(),
        ce_params: ce,
        ce_status,
        latency,
        forms,
        sweep: sweep_pts,
    };
    let forbidden = forbidden_texts(&all_rows, &pools, &bodies);
    let md = write_report(
        &report,
        &forbidden,
        args.output.as_deref(),
        args.json.as_deref(),
    )?;
    if args.output.is_none() {
        println!("{md}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/recall_bench/queries.jsonl");

    fn hit(id: &str, rank: u32, salience: f32, decay: f32) -> PoolHit {
        PoolHit {
            kb: "mem".into(),
            id: id.into(),
            title: format!("Synthetic memory {id}"),
            rank,
            salience,
            decay,
            decay_k: None,
            age_days: None,
            fetched_unix: 0,
        }
    }

    fn row(id: &str, class: &str, split: &str, labels: &str) -> String {
        format!(
            r#"{{"id":"{id}","query":"invented question {id}","labels":{labels},"class":"{class}","language":"en","split":"{split}"}}"#
        )
    }

    #[test]
    fn synthetic_fixture_loads_and_covers_every_stratum() {
        let rows = load_fixture(FIXTURE).unwrap();
        assert!(rows.len() >= 8);
        for c in [Class::Legacy, Class::Real, Class::Null, Class::Trivial] {
            assert!(rows.iter().any(|r| r.class == c), "class {c:?}");
        }
        assert!(rows.iter().any(|r| r.split == Split::Dev));
        assert!(rows.iter().any(|r| r.split == Split::Heldout));
        assert!(rows.iter().any(|r| r.language == "it"));
        assert!(rows.iter().any(|r| !r.context.is_empty()));
    }

    #[test]
    fn schema_validation_errors_are_clear() {
        let e = |t: String| load_fixture(&t).unwrap_err().to_string();
        assert!(e(row("a", "bogus", "dev", "{}")).contains("line 1"));
        assert!(e(row("a", "null", "tuning", "{}")).contains("invalid row"));
        assert!(e(row("a", "real", "dev", r#"{"m1":3}"#)).contains("allowed grades"));
        assert!(e(row("a", "null", "dev", r#"{"m1":1}"#)).contains("no memory at grade >= 1"));
        assert!(e(row("a", "real", "dev", r#"{"m1":0}"#)).contains("at least one label"));
        assert!(e(format!(
            "{}\n{}",
            row("a", "null", "dev", "{}"),
            row("a", "null", "dev", "{}")
        ))
        .contains("duplicate row id"));
        assert!(
            e(r#"{"id":"a","query":"","class":"null","language":"en","split":"dev"}"#.into())
                .contains("`query` is empty")
        );
        assert!(e(
            r#"{"id":"a","query":"q","class":"null","language":"English","split":"dev"}"#.into()
        )
        .contains("`language`"));
        assert!(e(
            r#"{"id":"a","query":"q","class":"null","language":"en","split":"dev","extra":1}"#
                .into()
        )
        .contains("unknown field"));
        assert!(e(String::new()).contains("no rows"));
        // grade-2 labels in kb:id form are accepted
        assert!(load_fixture(&row("a", "real", "dev", r#"{"mem:m1":2}"#)).is_ok());
    }

    #[test]
    fn wilson_bound_arithmetic() {
        assert_eq!(wilson_upper(0, 0), None);
        // 0/10: upper = z^2/(n+z^2) = 3.8416/13.8416
        let u = wilson_upper(0, 10).unwrap();
        assert!((u - 0.27753).abs() < 1e-4, "{u}");
        // 5/10: symmetric about 0.5
        let u = wilson_upper(5, 10).unwrap();
        assert!((u - 0.76341).abs() < 1e-4, "{u}");
        assert!(wilson_upper(10, 10).unwrap() <= 1.0);
    }

    #[test]
    fn recall_mrr_and_offtopic_arithmetic() {
        let rows = load_fixture(&row("a", "real", "dev", r#"{"x":2,"y":2,"z":1}"#)).unwrap();
        // grade order: miss, hit(2), tangential(1), hit(2), miss, miss
        let o = outcome(&rows[0], &[0, 2, 1, 2, 0, 0], 2, 5);
        assert_eq!(o.recall5, Some(1.0));
        assert_eq!(o.rr, Some(0.5));
        assert_eq!(o.injected, 5);
        assert_eq!(o.offtopic, 2); // 6th is beyond inject_k
        assert_eq!(o.pool_recall, Some(1.0));
        let o = outcome(&rows[0], &[0, 0, 0, 0, 0, 2], 1, 5);
        assert_eq!(o.recall5, Some(0.0));
        assert_eq!(o.rr, Some(1.0 / 6.0));
        assert_eq!(o.pool_recall, Some(0.5));
        // no grade-2 label -> excluded from recall
        let neg = load_fixture(&row("n", "null", "dev", "{}")).unwrap();
        assert_eq!(outcome(&neg[0], &[0, 0], 0, 5).recall5, None);
    }

    #[test]
    fn paired_table_and_false_injection_on_canned_rows() {
        let text = [
            row("r1", "real", "dev", r#"{"m1":2}"#),
            row("r2", "real", "dev", r#"{"m2":2}"#),
            row("r3", "legacy", "dev", r#"{"m3":2}"#),
            row("n1", "null", "dev", "{}"),
            row("t1", "trivial", "dev", "{}"),
        ]
        .join("\n");
        let rows = load_fixture(&text).unwrap();
        let arm = |g: &[&[u8]]| -> Vec<RowOutcome> {
            rows.iter()
                .zip(g)
                .map(|(r, g)| outcome(r, g, 0, 5))
                .collect()
        };
        let shipped = arm(&[&[2], &[0, 0], &[0], &[0, 0], &[0]]);
        let other = arm(&[&[0], &[2], &[0], &[], &[]]);
        let all: Vec<usize> = (0..rows.len()).collect();
        let s = stratum(&rows, &other, Some(&shipped), &all);
        let p = s.paired_vs_shipped.unwrap();
        assert_eq!((p.wins, p.losses, p.ties), (1, 1, 1));
        let f = s.false_injection.unwrap();
        assert_eq!((f.n, f.injecting, f.rate), (2, 0, 0.0));
        let sh = stratum(&rows, &shipped, None, &all);
        let f = sh.false_injection.unwrap();
        assert_eq!((f.n, f.injecting), (2, 2));
        assert_eq!(f.mean_injected, 1.5);
        assert!(sh.paired_vs_shipped.is_none());
        assert_eq!(sh.n_graded, 3);
    }

    #[test]
    fn ce_floor_drops_and_multipliers_reorder() {
        let pool = vec![
            hit("a", 1, 1.0, 1.0),
            hit("b", 2, 1.0, 1.0),
            hit("c", 3, 0.0, 1.0),
        ];
        let logits = [1.0f32, 3.0, 2.9];
        let p = CeParams {
            salience_coef: 0.0,
            decay_coef: 0.0,
            floor: 0.0,
        };
        assert_eq!(
            arm_order(Arm::Ce, &pool, Some(&logits), &p, 0),
            vec![1, 2, 0]
        );
        // floor 1.5 drops `a`
        let p = CeParams { floor: 1.5, ..p };
        assert_eq!(arm_order(Arm::Ce, &pool, Some(&logits), &p, 0), vec![1, 2]);
        // a floor above everything drops everything
        let p = CeParams { floor: 9.0, ..p };
        assert!(arm_order(Arm::Ce, &pool, Some(&logits), &p, 0).is_empty());
        // salience multiplier demotes `c` (salience 0) below nothing else here,
        // but with a big coefficient it falls behind `a`.
        let p = CeParams {
            salience_coef: 1.0,
            decay_coef: 0.0,
            floor: 0.0,
        };
        assert_eq!(
            arm_order(Arm::Ce, &pool, Some(&logits), &p, 0),
            vec![1, 0, 2]
        );
        // missing logits -> empty (ce not computable)
        assert!(arm_order(Arm::Ce, &pool, None, &p, 0).is_empty());
        // shipped keeps route order; content sorts by rank
        let mut shuffled = pool.clone();
        shuffled[0].rank = 9;
        assert_eq!(
            arm_order(Arm::Shipped, &shuffled, None, &p, 0),
            vec![0, 1, 2]
        );
        assert_eq!(
            arm_order(Arm::Content, &shuffled, None, &p, 0),
            vec![1, 2, 0]
        );
    }

    #[test]
    fn pinned_clock_makes_decay_independent_of_fetch_time() {
        let mut h = hit("a", 1, 1.0, 0.1);
        h.decay_k = Some(0.01);
        h.age_days = Some(100.0);
        h.fetched_unix = 1_000_000_000;
        let clock = 1_000_000_000 - 50 * 86_400; // 50 days before the fetch
        let d1 = decay_at(&h, clock);
        assert!((d1 - (-0.5f32).exp()).abs() < 1e-5);
        // fetched a day later: age_days is a day older, same pinned decay
        h.fetched_unix += 86_400;
        h.age_days = Some(101.0);
        assert!((decay_at(&h, clock) - d1).abs() < 1e-5);
        // no decomposition -> the route's own decay
        h.decay_k = None;
        assert_eq!(decay_at(&h, clock), 0.1);
    }

    #[test]
    fn split_filter_and_strata() {
        let rows = load_fixture(FIXTURE).unwrap();
        let dev = rows.iter().filter(|r| r.split == Split::Dev).count();
        let held = rows.iter().filter(|r| r.split == Split::Heldout).count();
        assert_eq!(dev + held, rows.len());
        assert!(dev > 0 && held > 0);
        let st = strata_index(&rows);
        assert_eq!(st["all"].len(), rows.len());
        assert!(st.contains_key("class=null") && st.contains_key("lang=it"));
        assert_eq!(parse_split("dev").unwrap(), Split::Dev);
        assert!(parse_split("train").is_err());
    }

    #[test]
    fn percentile_nearest_rank() {
        let xs = [10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0, 90.0, 100.0];
        assert_eq!(percentile(&xs, 50.0), Some(50.0));
        assert_eq!(percentile(&xs, 95.0), Some(100.0));
        assert_eq!(percentile(&[], 50.0), None);
    }

    fn canned_inputs(rows: &[FixtureRow]) -> (Vec<RowPool>, Vec<RowLogits>) {
        let pools: Vec<RowPool> = rows
            .iter()
            .map(|_| RowPool {
                plain: vec![hit("m1", 1, 1.0, 1.0), hit("m2", 2, 1.0, 1.0)],
                ctx: None,
            })
            .collect();
        let logits = rows
            .iter()
            .map(|_| RowLogits {
                plain: vec![-2.0, 4.0],
                ctx: None,
            })
            .collect();
        (pools, logits)
    }

    fn sample_report(rows: &[FixtureRow]) -> Report {
        let (pools, logits) = canned_inputs(rows);
        let ce = CeParams {
            salience_coef: 0.2,
            decay_coef: 0.2,
            floor: 0.0,
        };
        let inp = Inputs {
            rows,
            pools: &pools,
            logits: Some(&logits),
            ce,
            clock_unix: 0,
            inject_k: 5,
        };
        Report {
            schema: SCHEMA,
            daemon_build_sha: "deadbeef".into(),
            fixture_hash: "00".into(),
            fixture_rows: rows.len(),
            scored_rows: rows.len(),
            split: None,
            clock_unix: 0,
            pool_cap: POOL_CAP,
            inject_k: 5,
            reranker_model: "bge-reranker-base".into(),
            ce_params: ce,
            ce_status: "ran".into(),
            latency: None,
            forms: compute_forms(&inp),
            sweep: Some(sweep(&inp, Form::Plain, &[-3.0, 0.0, 5.0])),
        }
    }

    #[test]
    fn end_to_end_on_canned_pool_ce_beats_shipped_on_false_injection() {
        let rows = load_fixture(FIXTURE).unwrap();
        let r = sample_report(&rows);
        let all = &r.forms["plain"];
        let ship_fi = all["shipped"]["all"].false_injection.as_ref().unwrap();
        let ce_fi = all["ce"]["all"].false_injection.as_ref().unwrap();
        assert_eq!(ship_fi.rate, 1.0);
        // logits are 4.0 for m2 (above floor 0): ce still injects it
        assert_eq!(ce_fi.injecting, ce_fi.n);
        // sweep at floor 5.0 drops everything -> zero false injection
        let sw = r.sweep.as_ref().unwrap();
        assert_eq!(sw[2].false_injection.as_ref().unwrap().injecting, 0);
        assert!(
            sw[0].false_injection.as_ref().unwrap().rate
                >= sw[2].false_injection.as_ref().unwrap().rate
        );
        // context form falls back to plain when there is no ctx pool
        assert_eq!(
            r.forms["context"]["shipped"]["all"],
            r.forms["plain"]["shipped"]["all"]
        );
        let md = render_markdown(&r);
        assert!(md.contains("Wilson95") && md.contains("Paired recall@5 vs shipped"));
        assert!(md.contains("pool cap: top 50"));
    }

    #[test]
    fn leak_check_catches_query_title_body_and_context() {
        let rows = load_fixture(FIXTURE).unwrap();
        let clean = render_markdown(&sample_report(&rows));
        let forb = forbidden_texts(&rows, &[], &BTreeMap::new());
        check_no_leak(&clean, &forb).expect("aggregates-only report is clean");

        let q = rows[0].query.clone();
        let leaky = format!("{clean}\nnote: {q}\n");
        let err = check_no_leak(&leaky, &forb).unwrap_err().to_string();
        assert!(err.contains("query") && !err.contains(&q));

        // case/whitespace-insensitive, and a fragment of a long body is enough
        let body = "The quick synthetic body explains how the invented widget cache is invalidated nightly.".to_string();
        let f = vec![("memory body", body.clone())];
        let leaky = format!("{clean} ...invented   WIDGET cache is invalidated nightly... ");
        assert!(check_no_leak(&leaky, &f).is_err());
        check_no_leak(&clean, &f).unwrap();
        // short strings match as whole words only
        let f = vec![("memory title", "cache".to_string())];
        assert!(check_no_leak("a cache here", &f).is_err());
        assert!(check_no_leak("a cachet here", &f).is_ok());
        // context turns count
        let ctx = rows
            .iter()
            .find_map(|r| r.context.first())
            .unwrap()
            .text
            .clone();
        let f = vec![("context", ctx.clone())];
        assert!(check_no_leak(&format!("x {ctx} y"), &f).is_err());
    }

    #[test]
    fn write_report_refuses_a_leak_and_writes_nothing() {
        let rows = load_fixture(FIXTURE).unwrap();
        let mut r = sample_report(&rows);
        // a free-text field smuggling a fixture query into the report
        r.reranker_model = rows[0].query.clone();
        let forb = forbidden_texts(&rows, &[], &BTreeMap::new());
        let dir = tempfile::tempdir().unwrap();
        let (md, js) = (dir.path().join("r.md"), dir.path().join("r.json"));
        let err = write_report(&r, &forb, Some(&md), Some(&js))
            .unwrap_err()
            .to_string();
        assert!(err.contains("refusing to write"));
        assert!(!md.exists() && !js.exists());
        r.reranker_model = "bge-reranker-base".into();
        write_report(&r, &forb, Some(&md), Some(&js)).unwrap();
        assert!(md.exists() && js.exists());
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(js).unwrap()).unwrap();
        assert_eq!(v["pool_cap"], 50);
        assert_eq!(v["daemon_build_sha"], "deadbeef");
    }

    #[test]
    fn html_to_text_strips_markup_and_templates() {
        let t = html_to_text("<html><head><style>p{}</style></head><body><template id=\"kb-prompt\">secret</template><h1>Title &amp; more</h1><p>Body  text</p><script>x()</script></body></html>");
        assert_eq!(t, "Title & more Body text");
    }

    #[cfg(unix)]
    #[test]
    fn rerank_jobs_through_a_fake_shell_sidecar() {
        use std::os::unix::fs::PermissionsExt;
        // The env var is process-global; this is the only test in this
        // binary that sets it.
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-reranker.sh");
        std::fs::write(
            &script,
            r#"#!/bin/sh
printf '{"kind":"ready","model":"bge-reranker-base","dim":0}\n'
while IFS= read -r line; do
  case "$line" in
    *'"kind":"shutdown"'*) exit 0 ;;
    *'"kind":"rerank"'*)
      rid=$(printf '%s' "$line" | sed 's/.*"req_id":\([0-9][0-9]*\).*/\1/')
      printf '{"kind":"rerank_ok","req_id":%s,"scores":[0.5,2.5]}\n' "$rid"
      ;;
  esac
done
"#,
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::env::set_var("KB_EMBEDDER_BIN", &script);
        let mut c = kb_core::embed_ipc::RerankerClient::spawn_ipc(
            "bge-reranker-base",
            dir.path().to_path_buf(),
            19,
        )
        .unwrap();
        let jobs = vec![
            RerankJob {
                query: "q".into(),
                docs: vec!["a".into(), "b".into()],
            },
            RerankJob {
                query: "q".into(),
                docs: vec![],
            },
        ];
        let (scores, times) = rerank_jobs(&mut c, &jobs).unwrap();
        std::env::remove_var("KB_EMBEDDER_BIN");
        assert_eq!(scores, vec![vec![0.5, 2.5], vec![]]);
        assert_eq!(times.len(), 1);
        // the logits drive the ce order: index 1 first
        let pool = vec![hit("a", 1, 1.0, 1.0), hit("b", 2, 1.0, 1.0)];
        let p = CeParams {
            salience_coef: 0.0,
            decay_coef: 0.0,
            floor: 0.0,
        };
        assert_eq!(
            arm_order(Arm::Ce, &pool, Some(&scores[0]), &p, 0),
            vec![1, 0]
        );
    }
}
