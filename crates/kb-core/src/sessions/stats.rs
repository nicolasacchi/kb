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

use super::view::{view_append, view_finish, Role, TokenLine, ViewCarry, ViewEvent};

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

/// Extract the usage reading from an assistant record, or `None` when the
/// record carries no usage or is harness-synthesised (`isApiErrorMessage`,
/// `model:"<synthetic>"`).
///
/// omp records carrying `message.harness_usage` (schema `omp-usage/1`) key on
/// its `response_id` and use its RAW token fields; the folded
/// `message.usage.input_tokens` is ignored for them.
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
    if let Some(h) = msg.get("harness_usage").filter(|h| h.is_object()) {
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
}

impl UsageLedger {
    /// Count one usage reading. A repeat of a key with identical usage is a
    /// no-op; a repeat with different usage replaces the old values (last
    /// wins) and bumps the conflict census. Totals never double count a key.
    pub fn record(&mut self, key: Option<&str>, usage: Usage) {
        if let Some(k) = key {
            if let Some(old) = self.seen.get_mut(k) {
                if *old != usage {
                    self.totals.input = self.totals.input.saturating_sub(old.input);
                    self.totals.output = self.totals.output.saturating_sub(old.output);
                    self.totals.cache_read = self.totals.cache_read.saturating_sub(old.cache_read);
                    self.totals.cache_write =
                        self.totals.cache_write.saturating_sub(old.cache_write);
                    self.add(usage);
                    *old = usage;
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
        if let Some(r) = usage_reading(v) {
            self.record(r.key.as_deref(), r.usage);
        }
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
}

/// Deterministic session statistics. Not serialized by any route in this
/// unit; the derive exists so a later surface only wires a route.
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
}

impl SessionStats {
    pub(crate) fn from_parts(
        ledger: &UsageLedger,
        turns_human: u32,
        tool_calls: u32,
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
                "session_stats",
                "UsageLedger",
                "usage_conflicts",
                "sessions::stats",
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
