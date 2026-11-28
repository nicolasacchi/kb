//! Deterministic per-session statistics derived from the session-view/1 walk.
//!
//! Claude Code writes one JSONL record per content block and repeats the full
//! `message.usage` object on each of them, so summing usage per record
//! overcounts a block-split message several times over. This module owns the
//! ONE dedupe rule every token total in kb uses: [`UsageLedger`] keys usage by
//! `message.id` (falling back to the record's `requestId`; omp keys on its
//! `harness_usage.response_id`) and counts each key once.
//!
//! [`SessionStats`] is pure and LLM-free, computed per request from the
//! transcript, persisted nowhere, and SURFACED, NEVER SCORED (invariant #11):
//! it is not a rank term, a sort key or a filter in any ranking module (the
//! test `no_ranking_module_reads_session_stats` below enforces the
//! needles against the ranker sources).

use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;

use super::constants::ACTIVE_DELTA_CLAMP_SECS;
use super::view::{view_append, view_finish, Role, TokenLine, ViewCarry, ViewEvent};

/// The one `message.harness_usage.schema` string this module understands.
pub(crate) const OMP_USAGE_SCHEMA: &str = "omp-usage/1";

/// Cap on distinct dedupe keys a ledger remembers. Past it, new keys are
/// counted undeduped (totals keep rising, memory stays bounded, nothing
/// panics).
pub(crate) const LEDGER_KEY_CAP: usize = 50_000;

/// One message's four token counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

/// A usage reading extracted from one assistant record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UsageReading {
    /// Dedupe key; `None` = the record carries no id, count it undeduped.
    pub key: Option<String>,
    pub usage: Usage,
}

fn u64_field(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(|x| x.as_u64()).unwrap_or(0)
}

fn harness_schema_ok(h: &Value) -> bool {
    h.get("schema").and_then(|x| x.as_str()) == Some(OMP_USAGE_SCHEMA)
}

/// True when the record carries a `message.harness_usage` object whose
/// `schema` is not [`OMP_USAGE_SCHEMA`]: its numbers are never read, the
/// record falls back to its legacy `usage`, and the ledger counts it.
pub(crate) fn harness_schema_unknown(v: &Value) -> bool {
    v.get("message")
        .and_then(|m| m.get("harness_usage"))
        .is_some_and(|h| h.is_object() && !harness_schema_ok(h))
}

/// Extract the usage reading from an assistant record, or `None` when the
/// record carries no usage or is harness-synthesised (`isApiErrorMessage`,
/// `model:"<synthetic>"`).
///
/// omp records carrying `message.harness_usage` (schema `omp-usage/1`) key on
/// its `response_id` and use its RAW token fields; the folded
/// `message.usage.input_tokens` is ignored for them. A `harness_usage` with
/// any other schema is NOT read (see [`harness_schema_unknown`]).
pub(crate) fn usage_reading(v: &Value) -> Option<UsageReading> {
    let msg = v.get("message")?;
    if v.get("isApiErrorMessage").and_then(|x| x.as_bool()) == Some(true)
        || msg.get("model").and_then(|x| x.as_str()) == Some("<synthetic>")
    {
        return None;
    }
    let fallback_key = || {
        msg.get("id")
            .and_then(|x| x.as_str())
            .or_else(|| v.get("requestId").and_then(|x| x.as_str()))
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    if let Some(h) = msg
        .get("harness_usage")
        .filter(|h| h.is_object() && harness_schema_ok(h))
    {
        let key = h
            .get("response_id")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(fallback_key);
        return Some(UsageReading {
            key,
            usage: Usage {
                input: u64_field(h, "input"),
                output: u64_field(h, "output"),
                cache_read: u64_field(h, "cache_read"),
                cache_write: u64_field(h, "cache_write"),
            },
        });
    }
    let u = msg.get("usage").filter(|u| u.is_object())?;
    Some(UsageReading {
        key: fallback_key(),
        usage: Usage {
            input: u64_field(u, "input_tokens"),
            output: u64_field(u, "output_tokens"),
            cache_read: u64_field(u, "cache_read_input_tokens"),
            cache_write: u64_field(u, "cache_creation_input_tokens"),
        },
    })
}

/// Last-seen usage per dedupe key plus running totals.
#[derive(Debug, Clone, Default)]
pub(crate) struct UsageLedger {
    seen: HashMap<String, Usage>,
    totals: Usage,
    conflicts: u32,
    messages: u32,
    schema_unknown: u32,
    /// STATSX4: context tokens of the LAST non-synthetic main-thread record
    /// (not a max: usage is not monotonic across a compaction).
    context: Option<u64>,
    /// STATSX4: harness-reported cost, summed once per response key.
    cost_by_key: HashMap<String, f64>,
    cost_keyless: f64,
    cost_seen: bool,
}

/// STATSX4: the harness-reported cost a record carries
/// (`harness_usage.cost.total`), omp only. Claude records carry none.
fn reported_cost(v: &Value) -> Option<f64> {
    let h = v
        .get("message")?
        .get("harness_usage")
        .filter(|h| h.is_object() && harness_schema_ok(h))?;
    h.get("cost")?
        .get("total")?
        .as_f64()
        .filter(|c| c.is_finite() && *c >= 0.0)
}

/// STATSX4: context tokens held by one record: omp's
/// `harness_usage.context.prompt_tokens` when the record carries the omp
/// block (None when that block has no snapshot), else Claude's last
/// `input + cache_read + cache_creation`.
fn context_reading(v: &Value, usage: &Usage) -> Option<u64> {
    let msg = v.get("message")?;
    if let Some(h) = msg
        .get("harness_usage")
        .filter(|h| h.is_object() && harness_schema_ok(h))
    {
        return h
            .get("context")
            .and_then(|c| c.get("prompt_tokens"))
            .and_then(|x| x.as_u64());
    }
    Some(
        usage
            .input
            .saturating_add(usage.cache_read)
            .saturating_add(usage.cache_write),
    )
}

impl UsageLedger {
    /// Count one usage reading. A repeat of a key with identical usage is a
    /// no-op; a repeat with different usage replaces the old values (last
    /// wins) and bumps the conflict census. Totals never double count a key.
    pub fn record(&mut self, key: Option<&str>, usage: Usage) {
        if let Some(k) = key {
            if let Some(&old) = self.seen.get(k) {
                if old != usage {
                    self.totals.input = self.totals.input.saturating_sub(old.input);
                    self.totals.output = self.totals.output.saturating_sub(old.output);
                    self.totals.cache_read = self.totals.cache_read.saturating_sub(old.cache_read);
                    self.totals.cache_write =
                        self.totals.cache_write.saturating_sub(old.cache_write);
                    self.add(usage);
                    self.seen.insert(k.to_string(), usage);
                    self.conflicts = self.conflicts.saturating_add(1);
                }
                return;
            }
            if self.seen.len() < LEDGER_KEY_CAP {
                self.seen.insert(k.to_string(), usage);
            }
        }
        self.messages = self.messages.saturating_add(1);
        self.add(usage);
    }

    /// Convenience: extract and record in one step.
    pub fn record_record(&mut self, v: &Value) {
        if harness_schema_unknown(v) {
            self.schema_unknown = self.schema_unknown.saturating_add(1);
        }
        if let Some(r) = usage_reading(v) {
            self.record(r.key.as_deref(), r.usage);
            if v.get("isSidechain").and_then(|x| x.as_bool()) != Some(true) {
                if let Some(c) = context_reading(v, &r.usage) {
                    self.context = Some(c);
                }
            }
            if let Some(c) = reported_cost(v) {
                self.record_cost(r.key.as_deref(), c);
            }
        }
    }

    fn record_cost(&mut self, key: Option<&str>, cost: f64) {
        self.cost_seen = true;
        match key {
            Some(k)
                if self.cost_by_key.contains_key(k) || self.cost_by_key.len() < LEDGER_KEY_CAP =>
            {
                self.cost_by_key.insert(k.to_string(), cost);
            }
            _ => self.cost_keyless += cost,
        }
    }

    pub fn context_tokens(&self) -> Option<u64> {
        self.context
    }

    /// Harness-reported cost, `None` when no record carried one.
    pub fn cost_usd(&self) -> Option<f64> {
        self.cost_seen
            .then(|| self.cost_by_key.values().sum::<f64>() + self.cost_keyless)
    }

    fn add(&mut self, u: Usage) {
        self.totals.input = self.totals.input.saturating_add(u.input);
        self.totals.output = self.totals.output.saturating_add(u.output);
        self.totals.cache_read = self.totals.cache_read.saturating_add(u.cache_read);
        self.totals.cache_write = self.totals.cache_write.saturating_add(u.cache_write);
    }

    pub fn totals(&self) -> Usage {
        self.totals
    }

    /// `input + output` (cache excluded): the `sessions.token_total` figure.
    pub fn input_plus_output(&self) -> u64 {
        self.totals.input.saturating_add(self.totals.output)
    }

    pub fn token_line(&self) -> TokenLine {
        TokenLine {
            input: self.totals.input,
            output: self.totals.output,
            cache_write: self.totals.cache_write,
            cache_read: self.totals.cache_read,
        }
    }

    pub fn conflicts(&self) -> u32 {
        self.conflicts
    }

    pub fn messages(&self) -> u32 {
        self.messages
    }

    pub fn schema_unknown(&self) -> u32 {
        self.schema_unknown
    }
}

/// Where the per-turn durations came from. `wall`, `active` and the operator
/// wait are fixed by definition (record timestamps); only the per-turn figure
/// can mix sources, so only it carries a runtime label.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeSource {
    // Every timed turn used record timestamps.
    Timestamps,
    // Every timed turn used a duration the harness itself reported
    // (Claude `system/turn_duration`, omp `harness_usage.completed_at`).
    HarnessReported,
    // Some turns each way.
    Mixed,
}

/// Deterministic, LLM-free time figures. Every figure is `None` when the
/// transcript does not carry what is needed (never zero-filled). SURFACED,
/// NEVER SCORED (invariant #11): no ranking module reads any of it.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SessionTimeStats {
    /// Last record timestamp minus first (the header's `span_secs` rule,
    /// floored at 0). Source: record timestamps. `None` under two timestamps.
    pub wall_secs: Option<i64>,
    /// The header's `active_secs` (consecutive record gaps each clamped to
    /// the idle threshold). Source: record timestamps. `None` under two
    /// timestamps.
    pub active_secs: Option<i64>,
    /// Human-prompt cycles that yielded a duration.
    pub turns_timed: u32,
    /// Mean per-turn duration (integer division).
    pub turn_mean_secs: Option<i64>,
    /// Median per-turn duration; an even count takes the lower middle.
    pub turn_median_secs: Option<i64>,
    pub turn_max_secs: Option<i64>,
    /// Which source the per-turn figures used; `None` when no turn was timed.
    pub turn_source: Option<TimeSource>,
    /// Sum of closed waits on the operator: assistant/tool-result end to the
    /// next real prompt, and an ask tool_use to its answer. Source: record
    /// timestamps. Only gaps within the idle threshold are summed.
    pub wait_secs: Option<i64>,
    /// Closed gaps that were summed.
    pub wait_gaps: u32,
    pub wait_median_secs: Option<i64>,
    /// Gaps longer than the idle threshold: counted, never summed, so an
    /// overnight gap is never "waiting".
    pub wait_gaps_excluded: u32,
    /// An `AskUserQuestion`/`ExitPlanMode` call has no answer yet.
    pub pending_ask: bool,
    /// Records whose timestamp was earlier than the previous record's.
    /// Every delta involving one is clamped to 0.
    pub clock_regressions: u32,
}

/// Cap on remembered unanswered ask calls (bounded memory).
const OPEN_ASK_CAP: usize = 64;

fn is_ask_tool(name: &str) -> bool {
    matches!(name, "AskUserQuestion" | "ExitPlanMode")
}

/// Streaming accumulator for [`SessionTimeStats`], fed by the ONE
/// session-view/1 walk (`ViewCarry`); it never parses a transcript itself.
/// Cheap to clone and append-equivalent: a split `view_append` ends in the
/// same state as the full walk.
#[derive(Debug, Clone, Default)]
pub(crate) struct TimeAcc {
    /// Timestamp (unix secs) of the record being ingested.
    cur_ts: Option<i64>,
    prev_ts: Option<i64>,
    regressions: u32,
    cycle_active: bool,
    cycle_start: Option<i64>,
    cycle_end: Option<i64>,
    cycle_harness_ms: Option<i64>,
    cycle_completed_ms: Option<i64>,
    /// Finished cycles: `(secs, harness_reported)`.
    turns: Vec<(i64, bool)>,
    waits: Vec<i64>,
    waits_excluded: u32,
    open_asks: HashMap<String, Option<i64>>,
}

fn ms_field(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_f64().map(|f| f as i64))
        .filter(|n| *n >= 0)
}

impl TimeAcc {
    /// Called for every record, before dispatch, with its parsed timestamp.
    pub fn on_record_ts(&mut self, ts: Option<i64>) {
        self.cur_ts = ts;
        if let Some(t) = ts {
            if self.prev_ts.is_some_and(|p| t < p) {
                self.regressions = self.regressions.saturating_add(1);
            }
            self.prev_ts = Some(t);
        }
    }

    fn add_wait(&mut self, from: i64, to: i64) {
        let gap = (to - from).max(0);
        if gap <= ACTIVE_DELTA_CLAMP_SECS {
            self.waits.push(gap);
        } else {
            self.waits_excluded = self.waits_excluded.saturating_add(1);
        }
    }

    fn cycle_secs(&self) -> Option<(i64, bool)> {
        if !self.cycle_active {
            return None;
        }
        if let Some(ms) = self.cycle_harness_ms {
            return Some((ms / 1000, true));
        }
        if let (Some(done), Some(start)) = (self.cycle_completed_ms, self.cycle_start) {
            return Some(((done / 1000 - start).max(0), true));
        }
        match (self.cycle_start, self.cycle_end) {
            (Some(s), Some(e)) => Some(((e - s).max(0), false)),
            _ => None,
        }
    }

    /// A real (non-sidechain) human prompt: closes the previous cycle,
    /// books its wait gap, and opens a new cycle.
    pub fn on_real_prompt(&mut self) {
        if let Some(t) = self.cycle_secs() {
            self.turns.push(t);
        }
        if let (Some(end), Some(p)) = (self.cycle_end, self.cur_ts) {
            self.add_wait(end, p);
        }
        // A prompt after an unanswered ask abandons it: not "pending".
        self.open_asks.clear();
        self.cycle_active = true;
        self.cycle_start = self.cur_ts;
        self.cycle_end = None;
        self.cycle_harness_ms = None;
        self.cycle_completed_ms = None;
    }

    /// An assistant record (sidechain records are excluded by the caller's
    /// flag): extends the cycle, tracks the omp completion time and any ask
    /// tool_use.
    pub fn on_assistant(&mut self, v: &Value, sidechain: bool) {
        if sidechain {
            return;
        }
        if self.cur_ts.is_some() {
            self.cycle_end = self.cur_ts;
        }
        let msg = v.get("message");
        if let Some(h) = msg
            .and_then(|m| m.get("harness_usage"))
            .filter(|h| h.is_object() && harness_schema_ok(h))
        {
            if let Some(ms) = h.get("completed_at").and_then(ms_field) {
                if ms > 0 {
                    self.cycle_completed_ms = Some(ms);
                }
            }
        }
        let blocks = msg
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array());
        for b in blocks.into_iter().flatten() {
            if b.get("type").and_then(|x| x.as_str()) != Some("tool_use") {
                continue;
            }
            let name = b.get("name").and_then(|x| x.as_str()).unwrap_or("");
            let id = b.get("id").and_then(|x| x.as_str()).unwrap_or("");
            if is_ask_tool(name) && !id.is_empty() && self.open_asks.len() < OPEN_ASK_CAP {
                self.open_asks.insert(id.to_string(), self.cur_ts);
            }
        }
    }

    /// One `tool_result` block: extends the cycle and answers an open ask.
    pub fn on_tool_result(&mut self, tool_use_id: &str, sidechain: bool) {
        if sidechain {
            return;
        }
        if self.cur_ts.is_some() {
            self.cycle_end = self.cur_ts;
        }
        if let Some(asked) = self.open_asks.remove(tool_use_id) {
            if let (Some(a), Some(now)) = (asked, self.cur_ts) {
                self.add_wait(a, now);
            }
        }
    }

    /// A Claude `system/turn_duration` record.
    pub fn on_turn_duration(&mut self, v: &Value) {
        if v.get("isSidechain").and_then(|x| x.as_bool()) == Some(true) {
            return;
        }
        if let Some(ms) = v.get("durationMs").and_then(ms_field) {
            self.cycle_harness_ms = Some(self.cycle_harness_ms.unwrap_or(0).saturating_add(ms));
        }
    }

    /// Finish into the wire block. `wall`/`active` come from the carry's own
    /// header computation so they cannot drift from it.
    pub fn finish(&self, wall: Option<i64>, active: Option<i64>) -> SessionTimeStats {
        let mut turns = self.turns.clone();
        if let Some(t) = self.cycle_secs() {
            turns.push(t);
        }
        let mut secs: Vec<i64> = turns.iter().map(|t| t.0).collect();
        secs.sort_unstable();
        let n = secs.len();
        let harness = turns.iter().filter(|t| t.1).count();
        let mut waits = self.waits.clone();
        waits.sort_unstable();
        let wn = waits.len();
        SessionTimeStats {
            wall_secs: wall,
            active_secs: active,
            turns_timed: n as u32,
            turn_mean_secs: (n > 0).then(|| secs.iter().sum::<i64>() / n as i64),
            turn_median_secs: (n > 0).then(|| secs[(n - 1) / 2]),
            turn_max_secs: secs.last().copied(),
            turn_source: match (n, harness) {
                (0, _) => None,
                (_, h) if h == n => Some(TimeSource::HarnessReported),
                (_, 0) => Some(TimeSource::Timestamps),
                _ => Some(TimeSource::Mixed),
            },
            wait_secs: (wn > 0).then(|| waits.iter().sum()),
            wait_gaps: wn as u32,
            wait_median_secs: (wn > 0).then(|| waits[(wn - 1) / 2]),
            wait_gaps_excluded: self.waits_excluded,
            pending_ask: !self.open_asks.is_empty(),
            clock_regressions: self.regressions,
        }
    }
}

/// Deterministic session statistics, serialized by the session card's
/// `stats` lane only.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SessionStats {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// `cache_read / (cache_read + cache_write + input)`; `None` when the
    /// denominator is zero.
    pub cache_hit_ratio: Option<f64>,
    /// Human turns (the header's `turns_human`).
    pub turns: u32,
    pub tool_calls: u32,
    /// Distinct dedupe keys that carried usage (a record with no key counts 1).
    pub assistant_messages: u32,
    /// Repeats of one key whose usage differed.
    pub usage_conflicts: u32,
    /// Records whose `harness_usage.schema` was not understood: ignored (the
    /// legacy `usage` is used), counted here, never summed.
    pub usage_schema_unknown: u32,
    /// STATSX4: tokens the context held at the LAST non-synthetic main-thread
    /// record (Claude `input + cache_read + cache_creation`; omp
    /// `harness_usage.context.prompt_tokens`). `None` = not carried. Never a
    /// percentage here: the window is not in a transcript.
    pub context_tokens: Option<u64>,
    /// STATSX4: cost the HARNESS reported (omp `harness_usage.cost.total`,
    /// summed once per response). Always `None` for Claude transcripts;
    /// never estimated from a price table.
    pub cost_usd: Option<f64>,
    /// Time figures (wall, active, per-turn, operator wait). Surfaced, never
    /// scored.
    pub time: SessionTimeStats,
}

impl SessionStats {
    pub(crate) fn from_parts(
        ledger: &UsageLedger,
        turns_human: u32,
        tool_calls: u32,
        time: SessionTimeStats,
    ) -> SessionStats {
        let t = ledger.totals();
        let denom = t
            .cache_read
            .saturating_add(t.cache_write)
            .saturating_add(t.input);
        SessionStats {
            input_tokens: t.input,
            output_tokens: t.output,
            cache_read_tokens: t.cache_read,
            cache_write_tokens: t.cache_write,
            cache_hit_ratio: (denom > 0).then(|| t.cache_read as f64 / denom as f64),
            turns: turns_human,
            tool_calls,
            assistant_messages: ledger.messages(),
            usage_conflicts: ledger.conflicts(),
            usage_schema_unknown: ledger.schema_unknown(),
            context_tokens: ledger.context_tokens(),
            cost_usd: ledger.cost_usd(),
            time,
        }
    }
}

/// Walk a whole transcript through the session-view/1 engine and return its
/// stats. Agrees with a chunked `view_append` walk by construction.
pub fn session_stats_for_jsonl(jsonl: &str) -> SessionStats {
    let (mut events, mut carry) = view_append(ViewCarry::default(), jsonl);
    events.extend(view_finish(&mut carry));
    let turns_human = events
        .iter()
        .filter(|e| matches!(e, ViewEvent::TurnClosed(t) if t.role == Role::Human))
        .count() as u32;
    carry.session_stats(turns_human)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::view::{session_view, TailBlocks, ViewOptions};

    fn rec(id: Option<&str>, req: Option<&str>, out: u64, extra: &str) -> String {
        // A unique uuid per record: the engine's seen-uuid ring would
        // otherwise drop block repeats before the ledger ever saw them.
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let id = id.map(|i| format!(r#""id":"{i}","#)).unwrap_or_default();
        let req = req
            .map(|r| format!(r#""requestId":"{r}","#))
            .unwrap_or_default();
        format!(
            r#"{{"type":"assistant",{req}"uuid":"u-{n}","timestamp":"2026-01-01T00:00:00Z","message":{{{id}"role":"assistant","model":"m-1","content":[{{"type":"text","text":"x"}}],"usage":{{"input_tokens":10,"output_tokens":{out},"cache_read_input_tokens":100,"cache_creation_input_tokens":5}}{extra}}}}}"#,
        )
    }

    fn user(text: &str) -> String {
        format!(
            r#"{{"type":"user","uuid":"hu-{text}","timestamp":"2026-01-01T00:00:00Z","message":{{"role":"user","content":"{text}"}}}}"#
        )
    }

    fn join(lines: &[String]) -> String {
        let mut s = lines.join("\n");
        s.push('\n');
        s
    }

    #[test]
    fn block_split_message_counts_once() {
        let j = join(&[
            user("hi"),
            rec(Some("msg_1"), Some("req_1"), 7, ""),
            rec(Some("msg_1"), Some("req_1"), 7, ""),
            rec(Some("msg_1"), Some("req_1"), 7, ""),
        ]);
        let s = session_stats_for_jsonl(&j);
        assert_eq!(s.input_tokens, 10);
        assert_eq!(s.output_tokens, 7);
        assert_eq!(s.cache_read_tokens, 100);
        assert_eq!(s.cache_write_tokens, 5);
        assert_eq!(s.assistant_messages, 1);
        assert_eq!(s.usage_conflicts, 0);
        assert_eq!(s.turns, 1);
    }

    #[test]
    fn differing_repeat_last_wins_and_counts_a_conflict() {
        let a = join(&[
            rec(Some("msg_1"), None, 5, ""),
            rec(Some("msg_1"), None, 9, ""),
        ]);
        let s = session_stats_for_jsonl(&a);
        assert_eq!((s.output_tokens, s.usage_conflicts), (9, 1));
        // Order pins "last", not "first" and not "max".
        let b = join(&[
            rec(Some("msg_1"), None, 9, ""),
            rec(Some("msg_1"), None, 5, ""),
        ]);
        let s = session_stats_for_jsonl(&b);
        assert_eq!((s.output_tokens, s.usage_conflicts), (5, 1));
        assert_eq!(s.input_tokens, 10);
    }

    #[test]
    fn request_id_fallback_and_no_key_each_counted() {
        let j = join(&[
            rec(None, Some("req_1"), 3, ""),
            rec(None, Some("req_1"), 3, ""),
        ]);
        let s = session_stats_for_jsonl(&j);
        assert_eq!((s.output_tokens, s.assistant_messages), (3, 1));
        let j = join(&[rec(None, None, 3, ""), rec(None, None, 3, "")]);
        let s = session_stats_for_jsonl(&j);
        assert_eq!((s.output_tokens, s.assistant_messages), (6, 2));
    }

    #[test]
    fn synthetic_and_api_error_records_contribute_nothing() {
        let synth = r#"{"type":"assistant","uuid":"s1","message":{"id":"m_s","role":"assistant","model":"<synthetic>","content":[{"type":"text","text":"x"}],"usage":{"input_tokens":99,"output_tokens":99}}}"#;
        let err = r#"{"type":"assistant","uuid":"s2","isApiErrorMessage":true,"message":{"id":"m_e","role":"assistant","model":"m-1","content":[{"type":"text","text":"x"}],"usage":{"input_tokens":99,"output_tokens":99}}}"#;
        let j = join(&[synth.to_string(), err.to_string()]);
        assert_eq!(session_stats_for_jsonl(&j), SessionStats::default());
    }

    #[test]
    fn compaction_non_monotonic_usage_sums_not_max() {
        let big =
            rec(Some("m1"), None, 50, "").replace("\"input_tokens\":10", "\"input_tokens\":900");
        let j = join(&[big, rec(Some("m2"), None, 4, "")]);
        let s = session_stats_for_jsonl(&j);
        assert_eq!(s.input_tokens, 910);
        assert_eq!(s.output_tokens, 54);
    }

    #[test]
    fn missing_usage_and_sidechain_records() {
        let no_usage = r#"{"type":"assistant","uuid":"n1","message":{"id":"m_n","role":"assistant","model":"m-1","content":[{"type":"text","text":"x"}]}}"#;
        let side = rec(Some("m_side"), None, 8, "").replace(
            r#"{"type":"assistant","#,
            r#"{"type":"assistant","isSidechain":true,"#,
        );
        let j = join(&[no_usage.to_string(), side]);
        let s = session_stats_for_jsonl(&j);
        // Chosen behaviour (unchanged from before): sidechain records pass
        // through ingest and count; a record without usage counts nothing.
        assert_eq!(s.output_tokens, 8);
        assert_eq!(s.assistant_messages, 1);
    }

    #[test]
    fn append_split_anywhere_equals_full_walk() {
        let lines = vec![
            user("a"),
            rec(Some("msg_1"), Some("r1"), 7, ""),
            rec(Some("msg_1"), Some("r1"), 7, ""),
            rec(Some("msg_2"), Some("r2"), 3, ""),
            user("b"),
            rec(Some("msg_2"), Some("r2"), 4, ""),
        ];
        let full = session_stats_for_jsonl(&join(&lines));
        for cut in 0..=lines.len() {
            let (events_a, carry) = view_append(ViewCarry::default(), &join(&lines[..cut]));
            let (events_b, mut carry) = view_append(carry, &join(&lines[cut..]));
            let mut events = events_a;
            events.extend(events_b);
            events.extend(view_finish(&mut carry));
            let humans = events
                .iter()
                .filter(|e| matches!(e, ViewEvent::TurnClosed(t) if t.role == Role::Human))
                .count() as u32;
            assert_eq!(carry.session_stats(humans), full, "cut at {cut}");
        }
        assert_eq!(full.usage_conflicts, 1);
    }

    #[test]
    fn header_tokens_and_stats_agree_with_activity_token_total() {
        let j = join(&[
            user("a"),
            rec(Some("msg_1"), Some("r1"), 7, ""),
            rec(Some("msg_1"), Some("r1"), 7, ""),
            rec(Some("msg_2"), Some("r2"), 3, ""),
        ]);
        let v = session_view(&j, &TailBlocks::default(), &ViewOptions::default());
        let s = session_stats_for_jsonl(&j);
        assert_eq!(v.header.tokens.input, s.input_tokens);
        assert_eq!(v.header.tokens.output, s.output_tokens);
        assert_eq!(v.header.tokens.cache_read, s.cache_read_tokens);
        assert_eq!(v.header.tokens.cache_write, s.cache_write_tokens);
        assert_eq!(v.header.tool_calls, s.tool_calls);
        let act = crate::sessions::parse_session_activity(&j);
        assert_eq!(act.token_total, s.input_tokens + s.output_tokens);
        assert_eq!(act.token_total, 20 + 10);
    }

    #[test]
    fn omp_harness_usage_uses_raw_fields_and_response_id() {
        let line = |inp: u64, n: u32| {
            format!(
                r#"{{"type":"assistant","uuid":"o{n}","message":{{"role":"assistant","model":"m-1","content":[{{"type":"text","text":"x"}}],"usage":{{"input_tokens":9999,"output_tokens":6}},"harness_usage":{{"schema":"omp-usage/1","response_id":"resp_1","input":{inp},"output":6,"cache_read":40,"cache_write":2}}}}}}"#
            )
        };
        let j = join(&[line(11, 1), line(11, 2)]);
        let s = session_stats_for_jsonl(&j);
        assert_eq!(
            (
                s.input_tokens,
                s.output_tokens,
                s.cache_read_tokens,
                s.cache_write_tokens
            ),
            (11, 6, 40, 2)
        );
        assert_eq!(s.assistant_messages, 1);
        let act = crate::sessions::parse_session_activity(&j);
        assert_eq!(act.token_total, 17);
    }

    #[test]
    fn context_tokens_is_the_last_record_not_the_max_across_compaction() {
        let big =
            rec(Some("m1"), None, 5, "").replace("\"input_tokens\":10", "\"input_tokens\":900");
        let j = join(&[big, rec(Some("m2"), None, 4, "")]);
        // m2 = 10 input + 100 cache_read + 5 cache_creation; m1 would be 1005.
        assert_eq!(session_stats_for_jsonl(&j).context_tokens, Some(115));
        assert_eq!(session_stats_for_jsonl("").context_tokens, None);
    }

    #[test]
    fn context_tokens_skips_sidechain_and_synthetic_records() {
        let side = rec(Some("m_side"), None, 8, "").replace(
            r#"{"type":"assistant","#,
            r#"{"type":"assistant","isSidechain":true,"#,
        );
        let synth = r#"{"type":"assistant","uuid":"s1","message":{"id":"m_s","role":"assistant","model":"<synthetic>","content":[{"type":"text","text":"x"}],"usage":{"input_tokens":99,"output_tokens":99}}}"#;
        let j = join(&[rec(Some("m1"), None, 1, ""), side, synth.to_string()]);
        assert_eq!(session_stats_for_jsonl(&j).context_tokens, Some(115));
    }

    #[test]
    fn claude_transcripts_never_carry_a_cost() {
        let j = join(&[user("a"), rec(Some("m1"), None, 1, "")]);
        assert_eq!(session_stats_for_jsonl(&j).cost_usd, None);
    }

    #[test]
    fn omp_cost_and_context_come_from_harness_usage_once_per_response() {
        let line = |n: u32, rid: &str, cost: &str, ctx: &str| {
            format!(
                r#"{{"type":"assistant","uuid":"oc{n}","message":{{"role":"assistant","model":"m-1","content":[{{"type":"text","text":"x"}}],"usage":{{"input_tokens":9999,"output_tokens":6}},"harness_usage":{{"schema":"omp-usage/1","response_id":"{rid}","input":11,"output":6,"cache_read":40,"cache_write":2{cost}{ctx}}}}}}}"#
            )
        };
        let j = join(&[
            line(
                1,
                "r1",
                r#","cost":{"total":0.25}"#,
                r#","context":{"prompt_tokens":5000}"#,
            ),
            // A repeat of r1 must not add its cost twice.
            line(
                2,
                "r1",
                r#","cost":{"total":0.25}"#,
                r#","context":{"prompt_tokens":5000}"#,
            ),
            line(
                3,
                "r2",
                r#","cost":{"total":0.5}"#,
                r#","context":{"prompt_tokens":1200}"#,
            ),
            // No snapshot on the last record: the context stays at r2's.
            line(4, "r3", "", ""),
        ]);
        let s = session_stats_for_jsonl(&j);
        assert_eq!(s.cost_usd, Some(0.75));
        assert_eq!(s.context_tokens, Some(1200));
        // No cost anywhere on an omp transcript = None, never 0.
        let j = join(&[line(5, "r9", "", "")]);
        assert_eq!(session_stats_for_jsonl(&j).cost_usd, None);
    }

    #[test]
    fn cache_hit_ratio_none_on_zero_and_ratio_otherwise() {
        assert_eq!(session_stats_for_jsonl("").cache_hit_ratio, None);
        let j = join(&[rec(Some("m"), None, 1, "")]);
        let r = session_stats_for_jsonl(&j).cache_hit_ratio.unwrap();
        assert!((r - 100.0 / 115.0).abs() < 1e-12);
    }

    #[test]
    fn ledger_overflow_does_not_panic_and_totals_keep_rising() {
        let mut l = UsageLedger::default();
        let u = Usage {
            input: 1,
            ..Default::default()
        };
        for i in 0..(LEDGER_KEY_CAP + 10) {
            l.record(Some(&format!("k{i}")), u);
        }
        assert_eq!(l.totals().input, (LEDGER_KEY_CAP + 10) as u64);
        // A key past the cap is counted undeduped.
        l.record(Some(&format!("k{}", LEDGER_KEY_CAP + 5)), u);
        assert_eq!(l.totals().input, (LEDGER_KEY_CAP + 11) as u64);
    }

    // ---- time model fixtures (synthetic, public-gate clean) ----

    fn at(secs: i64) -> String {
        let day = 1 + secs / 86_400;
        let r = secs % 86_400;
        format!(
            "2026-01-{day:02}T{:02}:{:02}:{:02}Z",
            r / 3600,
            (r / 60) % 60,
            r % 60
        )
    }

    fn base_unix() -> i64 {
        crate::timeparse::parse_iso_utc(&at(0)).unwrap()
    }

    fn uid() -> usize {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1000);
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    fn ts_field(ts: Option<i64>) -> String {
        ts.map(|t| format!(r#""timestamp":"{}","#, at(t)))
            .unwrap_or_default()
    }

    fn tu(ts: Option<i64>, text: &str) -> String {
        format!(
            r#"{{"type":"user","uuid":"tu-{}",{}"message":{{"role":"user","content":"{text}"}}}}"#,
            uid(),
            ts_field(ts)
        )
    }

    fn ta(ts: Option<i64>, id: &str, content: &str, extra: &str) -> String {
        format!(
            r#"{{"type":"assistant","uuid":"ta-{}",{}"message":{{"id":"{id}","role":"assistant","model":"m-1","content":{content},"usage":{{"input_tokens":1,"output_tokens":1}}{extra}}}}}"#,
            uid(),
            ts_field(ts)
        )
    }

    const TEXT: &str = r#"[{"type":"text","text":"ok"}]"#;

    fn ask(id: &str) -> String {
        format!(r#"[{{"type":"tool_use","id":"{id}","name":"AskUserQuestion","input":{{}}}}]"#)
    }

    fn tr(ts: Option<i64>, id: &str) -> String {
        format!(
            r#"{{"type":"user","uuid":"tr-{}",{}"message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"{id}","content":"ok"}}]}}}}"#,
            uid(),
            ts_field(ts)
        )
    }

    fn turn_dur(ts: Option<i64>, ms: i64) -> String {
        format!(
            r#"{{"type":"system","subtype":"turn_duration","uuid":"td-{}",{}"durationMs":{ms}}}"#,
            uid(),
            ts_field(ts)
        )
    }

    #[test]
    fn missing_timestamps_leave_every_time_figure_absent() {
        let j = join(&[tu(None, "a"), ta(None, "m1", TEXT, ""), tu(None, "b")]);
        let t = session_stats_for_jsonl(&j).time;
        assert_eq!(t, SessionTimeStats::default());
        assert_eq!((t.wall_secs, t.active_secs, t.turns_timed), (None, None, 0));
        assert_eq!((t.turn_source, t.wait_secs), (None, None));
    }

    #[test]
    fn single_timestamp_has_no_wall_or_active() {
        let j = join(&[tu(Some(5), "a")]);
        let t = session_stats_for_jsonl(&j).time;
        assert_eq!((t.wall_secs, t.active_secs), (None, None));
    }

    #[test]
    fn overnight_gap_is_never_waiting_and_active_adds_only_the_clamp() {
        let night = 9 * 3600;
        let j = join(&[
            tu(Some(0), "a"),
            ta(Some(30), "m1", TEXT, ""),
            tu(Some(30 + night), "b"),
            ta(Some(50 + night), "m2", TEXT, ""),
        ]);
        let t = session_stats_for_jsonl(&j).time;
        assert_eq!(t.wait_gaps_excluded, 1);
        assert_eq!(
            (t.wait_secs, t.wait_gaps, t.wait_median_secs),
            (None, 0, None)
        );
        assert_eq!(t.active_secs, Some(30 + ACTIVE_DELTA_CLAMP_SECS + 20));
        assert_eq!(t.wall_secs, Some(night + 50));
        assert_eq!(t.turns_timed, 2);
        assert_eq!(t.turn_source, Some(TimeSource::Timestamps));
        assert_eq!((t.turn_max_secs, t.turn_median_secs), (Some(30), Some(20)));
    }

    #[test]
    fn wait_between_turns_within_the_threshold_is_summed() {
        let j = join(&[
            tu(Some(0), "a"),
            ta(Some(10), "m1", TEXT, ""),
            tu(Some(70), "b"),
            ta(Some(80), "m2", TEXT, ""),
            tu(Some(200), "c"),
        ]);
        let t = session_stats_for_jsonl(&j).time;
        assert_eq!((t.wait_secs, t.wait_gaps), (Some(180), 2));
        assert_eq!(t.wait_median_secs, Some(60));
        assert_eq!(t.wait_gaps_excluded, 0);
    }

    #[test]
    fn pending_ask_is_flagged_and_never_summed() {
        let j = join(&[tu(Some(0), "a"), ta(Some(5), "m1", &ask("q1"), "")]);
        let t = session_stats_for_jsonl(&j).time;
        assert!(t.pending_ask);
        assert_eq!((t.wait_secs, t.wait_gaps), (None, 0));
    }

    #[test]
    fn answered_ask_within_the_threshold_is_summed_and_a_late_one_excluded() {
        let quick = join(&[
            tu(Some(0), "a"),
            ta(Some(5), "m1", &ask("q1"), ""),
            tr(Some(65), "q1"),
        ]);
        let t = session_stats_for_jsonl(&quick).time;
        assert!(!t.pending_ask);
        assert_eq!((t.wait_secs, t.wait_gaps), (Some(60), 1));
        let slow = join(&[
            tu(Some(0), "a"),
            ta(Some(5), "m1", &ask("q1"), ""),
            tr(Some(5 + ACTIVE_DELTA_CLAMP_SECS + 1), "q1"),
        ]);
        let t = session_stats_for_jsonl(&slow).time;
        assert_eq!((t.wait_secs, t.wait_gaps_excluded), (None, 1));
    }

    #[test]
    fn a_new_prompt_abandons_an_unanswered_ask() {
        let j = join(&[
            tu(Some(0), "a"),
            ta(Some(5), "m1", &ask("q1"), ""),
            tu(Some(20), "never mind"),
        ]);
        let t = session_stats_for_jsonl(&j).time;
        assert!(!t.pending_ask);
        assert_eq!(t.wait_secs, Some(15));
    }

    #[test]
    fn harness_reported_turn_duration_beats_the_timestamp_span() {
        let j = join(&[
            tu(Some(0), "a"),
            ta(Some(60), "m1", TEXT, ""),
            turn_dur(Some(61), 42_000),
        ]);
        let t = session_stats_for_jsonl(&j).time;
        assert_eq!(t.turn_source, Some(TimeSource::HarnessReported));
        assert_eq!((t.turns_timed, t.turn_max_secs), (1, Some(42)));
        // Several reports inside one cycle add up.
        let j = join(&[
            tu(Some(0), "a"),
            ta(Some(60), "m1", TEXT, ""),
            turn_dur(Some(61), 10_000),
            turn_dur(Some(62), 5_500),
        ]);
        assert_eq!(session_stats_for_jsonl(&j).time.turn_max_secs, Some(15));
    }

    #[test]
    fn mixed_sources_are_labelled_mixed() {
        let j = join(&[
            tu(Some(0), "a"),
            ta(Some(60), "m1", TEXT, ""),
            turn_dur(Some(61), 42_000),
            tu(Some(100), "b"),
            ta(Some(130), "m2", TEXT, ""),
        ]);
        let t = session_stats_for_jsonl(&j).time;
        assert_eq!(t.turn_source, Some(TimeSource::Mixed));
        assert_eq!(t.turns_timed, 2);
        assert_eq!(
            (t.turn_mean_secs, t.turn_median_secs, t.turn_max_secs),
            (Some(36), Some(30), Some(42))
        );
    }

    fn omp_asst(ts: i64, completed_ms: i64, schema: &str) -> String {
        format!(
            r#"{{"type":"assistant","uuid":"oa-{}",{}"message":{{"role":"assistant","model":"m-1","content":[{{"type":"text","text":"x"}}],"usage":{{"input_tokens":9,"output_tokens":4}},"harness_usage":{{"schema":"{schema}","response_id":"r-{}","input":3,"output":2,"cache_read":1,"cache_write":1,"completed_at":{completed_ms}}}}}}}"#,
            uid(),
            ts_field(Some(ts)),
            uid()
        )
    }

    #[test]
    fn omp_completed_at_is_the_harness_reported_turn_end() {
        let done = (base_unix() + 50) * 1000;
        let j = join(&[tu(Some(0), "a"), omp_asst(20, done, "omp-usage/1")]);
        let t = session_stats_for_jsonl(&j).time;
        assert_eq!(t.turn_source, Some(TimeSource::HarnessReported));
        assert_eq!(t.turn_max_secs, Some(50));
        // An unknown schema's completed_at is not read: timestamps win.
        let j = join(&[tu(Some(0), "a"), omp_asst(20, done, "omp-usage/9")]);
        let t = session_stats_for_jsonl(&j).time;
        assert_eq!(t.turn_source, Some(TimeSource::Timestamps));
        assert_eq!(t.turn_max_secs, Some(20));
    }

    #[test]
    fn a_clock_going_backwards_never_yields_a_negative_figure() {
        let j = join(&[
            tu(Some(100), "a"),
            ta(Some(90), "m1", TEXT, ""),
            tu(Some(80), "b"),
        ]);
        let t = session_stats_for_jsonl(&j).time;
        assert_eq!(t.clock_regressions, 2);
        assert_eq!(t.wall_secs, Some(0));
        assert_eq!(t.active_secs, Some(0));
        assert_eq!(t.wait_secs, Some(0));
        assert_eq!(t.turn_max_secs, Some(0));
        for v in [t.turn_mean_secs, t.turn_median_secs, t.wait_median_secs] {
            assert!(v.is_none_or(|n| n >= 0));
        }
    }

    #[test]
    fn median_and_max_are_deterministic_on_odd_and_even_counts() {
        let cyc = |start: i64, ms: i64| {
            vec![
                tu(Some(start), "p"),
                ta(Some(start + 1), &format!("m{start}"), TEXT, ""),
                turn_dur(Some(start + 2), ms),
            ]
        };
        let mut lines = Vec::new();
        for (i, ms) in [10_000, 40_000, 20_000].iter().enumerate() {
            lines.extend(cyc(i as i64 * 100, *ms));
        }
        let t = session_stats_for_jsonl(&join(&lines)).time;
        assert_eq!(
            (t.turn_median_secs, t.turn_max_secs, t.turn_mean_secs),
            (Some(20), Some(40), Some(23))
        );
        lines.extend(cyc(300, 30_000));
        let t = session_stats_for_jsonl(&join(&lines)).time;
        // Even count: the lower middle, never an average.
        assert_eq!(
            (t.turn_median_secs, t.turn_max_secs, t.turn_mean_secs),
            (Some(20), Some(40), Some(25))
        );
    }

    #[test]
    fn time_block_survives_a_split_walk_and_active_matches_the_header() {
        let done = (base_unix() + 120) * 1000;
        let lines = vec![
            tu(Some(0), "a"),
            ta(Some(10), "m1", &ask("q1"), ""),
            tr(Some(40), "q1"),
            ta(Some(50), "m2", TEXT, ""),
            turn_dur(Some(51), 45_000),
            tu(Some(9 * 3600), "b"),
            omp_asst(9 * 3600 + 20, done, "omp-usage/1"),
            tu(Some(9 * 3600 + 5), "clock back"),
            ta(Some(9 * 3600 + 30), "m4", TEXT, ""),
        ];
        let full = session_stats_for_jsonl(&join(&lines));
        assert!(full.time.turns_timed >= 3 && full.time.wait_gaps_excluded >= 1);
        for cut in 0..=lines.len() {
            let (ea, carry) = view_append(ViewCarry::default(), &join(&lines[..cut]));
            let (eb, mut carry) = view_append(carry, &join(&lines[cut..]));
            let mut events = ea;
            events.extend(eb);
            events.extend(view_finish(&mut carry));
            let humans = events
                .iter()
                .filter(|e| matches!(e, ViewEvent::TurnClosed(t) if t.role == Role::Human))
                .count() as u32;
            assert_eq!(carry.session_stats(humans), full, "cut at {cut}");
        }
        let v = session_view(
            &join(&lines),
            &TailBlocks::default(),
            &ViewOptions::default(),
        );
        assert_eq!(full.time.active_secs, Some(v.header.active_secs));
        assert_eq!(full.time.wall_secs, v.header.span_secs);
    }

    #[test]
    fn unknown_harness_usage_schema_is_ignored_counted_and_falls_back() {
        let line = |schema: &str| {
            format!(
                r#"{{"type":"assistant","uuid":"us-{}","message":{{"role":"assistant","model":"m-1","content":[{{"type":"text","text":"x"}}],"usage":{{"input_tokens":9,"output_tokens":4}},"harness_usage":{{"schema":"{schema}","response_id":"r1","input":777,"output":888,"cache_read":5,"cache_write":5}}}}}}"#,
                uid()
            )
        };
        let s = session_stats_for_jsonl(&join(&[line("omp-usage/2")]));
        assert_eq!(s.usage_schema_unknown, 1);
        assert_eq!((s.input_tokens, s.output_tokens), (9, 4));
        assert_eq!((s.cache_read_tokens, s.cache_write_tokens), (0, 0));
        let s = session_stats_for_jsonl(&join(&[line("omp-usage/1")]));
        assert_eq!(s.usage_schema_unknown, 0);
        assert_eq!((s.input_tokens, s.output_tokens), (777, 888));
        // A missing schema string is unknown too.
        let no_schema = line("omp-usage/1").replace(r#""schema":"omp-usage/1","#, "");
        assert_eq!(
            session_stats_for_jsonl(&join(&[no_schema])).usage_schema_unknown,
            1
        );
    }

    // invariant:11 session stats are surfaced, never scored
    #[test]
    fn no_ranking_module_reads_session_stats() {
        const RANKERS: &[(&str, &str)] = &[
            ("memory.rs", include_str!("../memory.rs")),
            ("fusion.rs", include_str!("../fusion.rs")),
            ("resurface.rs", include_str!("../resurface.rs")),
            ("triage.rs", include_str!("../triage.rs")),
            ("query.rs", include_str!("../query.rs")),
            ("sessions/projects.rs", include_str!("projects.rs")),
        ];
        for (name, src) in RANKERS {
            for needle in [
                "SessionStats",
                "StatsLane",
                "session_stats",
                "UsageLedger",
                "usage_conflicts",
                "sessions::stats",
                "TimeAcc",
                "operator_wait",
                "wait_secs",
                "usage_schema_unknown",
            ] {
                assert!(
                    !src.contains(needle),
                    "{name} names `{needle}` -- session stats are SURFACED, NEVER SCORED (invariant #11): \
                     they may be rendered beside a row, never folded into a rank, a sort key or a filter"
                );
            }
        }
    }
}
