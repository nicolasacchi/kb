//! The Claude Code `parentUuid` chain, shared by the handoff reader
//! (`handoff/claude.rs`) and the session-view engine (`view.rs`).
//!
//! Claude persists a branching tree (`uuid`/`parentUuid`). A rewound or edited
//! prompt stays in the same JSONL on a branch the conversation left, so the
//! LIVE conversation is the chain walked back from the resumed leaf. There is
//! exactly ONE walker, [`live_order`]; [`chain_verdicts`] is the view's
//! question asked of it ("which records did the conversation leave behind?"),
//! never a second walk.

use std::collections::{HashMap, HashSet};

use serde_json::Value;

pub(crate) fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

pub(crate) fn flag(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

pub(crate) fn type_of(v: &Value) -> &str {
    str_of(v, "type").unwrap_or("")
}

/// Records that can sit on the parentUuid chain.
pub(crate) fn is_chain_type(v: &Value) -> bool {
    matches!(type_of(v), "user" | "assistant" | "system" | "attachment")
}

pub(crate) fn is_conversation_type(v: &Value) -> bool {
    matches!(type_of(v), "user" | "assistant")
}

/// Indices of `records` in chronological order of the live conversation.
///
/// With uuids on user/assistant records: the parentUuid chain from the leaf,
/// continued across `compact_boundary` through `logicalParentUuid`. Without
/// them (kb's omp captures): every record in file order.
pub(crate) fn live_order(records: &[Value], notes: &mut Vec<String>) -> Vec<usize> {
    let chained = records
        .iter()
        .any(|r| is_conversation_type(r) && str_of(r, "uuid").is_some());
    if !chained {
        return (0..records.len())
            .filter(|&i| is_conversation_type(&records[i]))
            .collect();
    }

    let by_uuid: HashMap<&str, usize> = records
        .iter()
        .enumerate()
        .filter(|(_, r)| is_chain_type(r))
        .filter_map(|(i, r)| str_of(r, "uuid").map(|u| (u, i)))
        .collect();
    let leaf = records
        .iter()
        .rev()
        .filter(|r| type_of(r) == "last-prompt")
        .filter_map(|r| str_of(r, "leafUuid"))
        .find_map(|u| by_uuid.get(u).copied())
        .or_else(|| {
            records.iter().rposition(|r| {
                is_chain_type(r) && !flag(r, "isSidechain") && str_of(r, "uuid").is_some()
            })
        });
    let Some(mut cur) = leaf else {
        return Vec::new();
    };

    let mut order = Vec::new();
    let mut seen: HashSet<usize> = HashSet::new();
    loop {
        if !seen.insert(cur) {
            notes.push("parentUuid cycle; recovered chain cut short".to_string());
            break;
        }
        order.push(cur);
        let r = &records[cur];
        let next = match str_of(r, "parentUuid") {
            Some(p) => {
                let n = by_uuid.get(p).copied();
                if n.is_none() {
                    notes.push(
                        "parentUuid chain broken (parent record missing); earlier records not \
                         recovered"
                            .to_string(),
                    );
                }
                n
            }
            // A compact boundary starts a new chain; its logicalParentUuid is
            // the pre-compaction leaf.
            None => str_of(r, "logicalParentUuid").and_then(|u| by_uuid.get(u).copied()),
        };
        match next {
            Some(n) => cur = n,
            None => break,
        }
    }
    order.reverse();
    order
}

/// Where one chain record sits relative to the live conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// On the live parentUuid chain.
    On,
    /// A branch the conversation left: off the chain and written BEFORE the
    /// leaf in file order.
    Off,
    /// Off the chain but written AFTER the leaf. Not an abandoned branch: it
    /// is the tail written after `last-prompt` last named a leaf (a live or
    /// resumed capture), so the honest answer is "unknown", never "rewound".
    Unknown,
}

/// The verdicts [`chain_verdicts`] reached, two ways: by line (for the turns
/// whose `raw_lines` name their records) and by record uuid (a human turn
/// carries no `raw_lines`, but its id IS its record's uuid).
#[derive(Debug, Default)]
pub(crate) struct ChainVerdicts {
    pub(crate) by_line: HashMap<u32, Verdict>,
    pub(crate) by_uuid: HashMap<String, Verdict>,
}

/// A [`Verdict`] for every chain record, keyed by decoded-transcript line
/// index (0-based, `str::lines()` numbering - the same numbering as
/// `Turn::raw_lines`). Lines that are not chain records (snapshots, modes,
/// sidechain records) are absent.
///
/// `None` (no mark, never a guess) when the chain cannot be trusted:
/// - the transcript carries no uuids (omp captures, hand-built fixtures);
/// - no leaf resolves;
/// - [`live_order`] noted a cycle or a missing parent, so earlier records were
///   not recovered and "not on the chain" would not mean "rewound" (current
///   Claude Code compaction boundaries can do this).
///
/// - the walk ended silently before the first conversation record in file
///   order (an unresolved `logicalParentUuid`, or a second `parentUuid`-null
///   root): the earlier history is live but unrecovered. This also declines
///   to mark a prompt edited at the very first turn of a file.
///
/// A uuid that appears on the chain is on it wherever else it is repeated
/// (re-delivered lines).
pub(crate) fn chain_verdicts(jsonl: &str) -> Option<ChainVerdicts> {
    let mut records: Vec<Value> = Vec::new();
    let mut line_of: Vec<u32> = Vec::new();
    for (idx, line) in jsonl.lines().enumerate() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(t) {
            records.push(v);
            line_of.push(idx as u32);
        }
    }
    if !records
        .iter()
        .any(|r| is_conversation_type(r) && str_of(r, "uuid").is_some())
    {
        return None;
    }
    let mut notes: Vec<String> = Vec::new();
    let order = live_order(&records, &mut notes);
    if !notes.is_empty() || order.is_empty() {
        return None;
    }
    let leaf = *order.last()?;
    let on_chain: HashSet<&str> = order
        .iter()
        .filter_map(|&i| str_of(&records[i], "uuid"))
        .collect();
    // The walk can also end SILENTLY: a record with `parentUuid` null and no
    // resolvable `logicalParentUuid` (a boundary whose target is not in the
    // file, or a second root) stops it without a note, and every earlier
    // record would then read as "off the chain". The walk is only complete
    // when the first live conversation record in file order is on it.
    let first_conversation = records.iter().find(|r| {
        is_conversation_type(r) && !flag(r, "isSidechain") && str_of(r, "uuid").is_some()
    });
    if let Some(first) = first_conversation {
        if !str_of(first, "uuid").is_some_and(|u| on_chain.contains(u)) {
            return None;
        }
    }
    let mut out = ChainVerdicts::default();
    for (i, r) in records.iter().enumerate() {
        if !is_chain_type(r) || flag(r, "isSidechain") {
            continue;
        }
        let Some(u) = str_of(r, "uuid") else { continue };
        let v = if on_chain.contains(u) {
            Verdict::On
        } else if i < leaf {
            Verdict::Off
        } else {
            Verdict::Unknown
        };
        out.by_line.insert(line_of[i], v);
        out.by_uuid.entry(u.to_string()).or_insert(v);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(ty: &str, uuid: &str, parent: Option<&str>) -> String {
        let p = parent.map_or("null".to_string(), |p| format!("\"{p}\""));
        format!(
            r#"{{"type":"{ty}","uuid":"{uuid}","parentUuid":{p},"message":{{"role":"{ty}","content":"x"}}}}"#
        )
    }

    fn off_lines(d: &str) -> Option<Vec<u32>> {
        Some(
            chain_verdicts(d)?
                .by_line
                .into_iter()
                .filter(|(_, v)| *v == Verdict::Off)
                .map(|(l, _)| l)
                .collect(),
        )
    }

    fn doc(lines: &[String]) -> String {
        lines.join("\n")
    }

    #[test]
    fn a_rewound_branch_is_off_chain_and_the_live_one_is_not() {
        // a1 -> b1 (abandoned) ; a1 -> b2 (live leaf). Line 0 = permission-mode.
        let d = doc(&[
            r#"{"type":"permission-mode","permissionMode":"plan"}"#.to_string(),
            rec("user", "u1", None),
            rec("assistant", "a1", Some("u1")),
            rec("user", "u2-old", Some("a1")),
            rec("assistant", "a2-old", Some("u2-old")),
            rec("user", "u2-new", Some("a1")),
            rec("assistant", "a2-new", Some("u2-new")),
        ]);
        let mut v = off_lines(&d).expect("chain is trustworthy");
        v.sort();
        assert_eq!(v, vec![3, 4]);
    }

    #[test]
    fn a_linear_transcript_has_nothing_off_chain() {
        let d = doc(&[
            rec("user", "u1", None),
            rec("assistant", "a1", Some("u1")),
            rec("user", "u2", Some("a1")),
        ]);
        assert!(off_lines(&d).expect("trusted").is_empty());
    }

    #[test]
    fn records_after_the_leaf_are_unknown_not_rewound() {
        // last-prompt names a1 as the leaf; u2/a2 were written after it.
        let d = doc(&[
            rec("user", "u1", None),
            rec("assistant", "a1", Some("u1")),
            r#"{"type":"last-prompt","leafUuid":"a1"}"#.to_string(),
            rec("user", "u2", Some("a1")),
            rec("assistant", "a2", Some("u2")),
        ]);
        assert!(off_lines(&d).expect("trusted").is_empty());
    }

    #[test]
    fn a_missing_parent_degrades_to_no_mark() {
        let d = doc(&[
            rec("user", "u1", Some("gone")),
            rec("assistant", "a1", Some("u1")),
        ]);
        assert!(chain_verdicts(&d).is_none());
    }

    #[test]
    fn a_cycle_degrades_to_no_mark() {
        // u1 and a1 point at each other and a1 is the last record, so the walk
        // from the leaf revisits a node and notes a cycle.
        let d = doc(&[
            rec("user", "u1", Some("a1")),
            rec("assistant", "a1", Some("u1")),
        ]);
        assert!(chain_verdicts(&d).is_none());
    }

    #[test]
    fn a_compaction_boundary_is_crossed_without_marking_the_earlier_turns() {
        let d = doc(&[
            rec("user", "u1", None),
            rec("assistant", "a1", Some("u1")),
            r#"{"type":"system","subtype":"compact_boundary","uuid":"cb","parentUuid":null,"logicalParentUuid":"a1"}"#.to_string(),
            rec("user", "u2", Some("cb")),
            rec("assistant", "a2", Some("u2")),
        ]);
        let off = off_lines(&d).expect("compaction crossing is trusted");
        assert!(off.is_empty(), "{off:?}");
    }

    #[test]
    fn an_unresolved_logical_parent_degrades_to_no_mark() {
        // The boundary's logicalParentUuid names a record that is not in the
        // file, so the walk stops at the boundary silently; u1..a4 are live
        // history, never "rewound".
        let d = doc(&[
            rec("user", "u1", None),
            rec("assistant", "a2", Some("u1")),
            rec("user", "u3", Some("a2")),
            rec("assistant", "a4", Some("u3")),
            r#"{"type":"system","subtype":"compact_boundary","uuid":"cb","parentUuid":null,"logicalParentUuid":"not-in-file"}"#.to_string(),
            rec("user", "u5", Some("cb")),
            rec("assistant", "a6", Some("u5")),
        ]);
        assert!(chain_verdicts(&d).is_none());
    }

    #[test]
    fn a_second_null_parent_root_degrades_to_no_mark() {
        let d = doc(&[
            rec("user", "u1", None),
            rec("assistant", "a2", Some("u1")),
            rec("user", "u3", None),
            rec("assistant", "a4", Some("u3")),
        ]);
        assert!(chain_verdicts(&d).is_none());
    }

    #[test]
    fn a_transcript_without_uuids_is_never_marked() {
        let d = r#"{"type":"user","message":{"role":"user","content":"hi"}}"#;
        assert!(chain_verdicts(d).is_none());
    }
}
