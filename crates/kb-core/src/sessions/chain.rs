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
///
/// Compaction: a boundary's `logicalParentUuid` is followed as-is when it names
/// a record written before the boundary (every normal boundary). Current
/// auto-compactions instead name the preserved segment's tail, written AFTER
/// the boundary and chained back to it; there the walk continues from the last
/// chain record written before the boundary. A boundary with nothing before it
/// ends the walk, and the callers' first-record check then declines to trust it.
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
            None => {
                let target = str_of(r, "logicalParentUuid").and_then(|u| by_uuid.get(u).copied());
                match target {
                    // Current Claude Code auto-compactions name as the target
                    // (the preserved segment's tail) a record written AFTER the
                    // boundary whose own parentUuid chain leads back to it.
                    // Following it would loop; the pre-compaction history is
                    // what precedes the boundary in the file.
                    Some(t) if t > cur && reaches(records, &by_uuid, t, cur) => {
                        pre_boundary_leaf(records, cur)
                    }
                    other => other,
                }
            }
        };
        match next {
            Some(n) => cur = n,
            None => break,
        }
    }
    order.reverse();
    order
}

/// True when the `parentUuid` chain walked back from `from` reaches `target`.
/// Bounded by a visited set; a missing parent or a cycle answers false.
fn reaches(records: &[Value], by_uuid: &HashMap<&str, usize>, from: usize, target: usize) -> bool {
    let mut cur = from;
    let mut seen: HashSet<usize> = HashSet::new();
    while seen.insert(cur) {
        if cur == target {
            return true;
        }
        match str_of(&records[cur], "parentUuid").and_then(|p| by_uuid.get(p).copied()) {
            Some(n) => cur = n,
            None => return false,
        }
    }
    false
}

/// The pre-compaction leaf of the boundary at `boundary`: the last
/// non-sidechain chain record written before it. On every normal boundary this
/// IS its `logicalParentUuid` target; it is only consulted when that target
/// sits after the boundary, so a boundary with nothing before it yields `None`
/// and the walk ends (never a guessed chain).
fn pre_boundary_leaf(records: &[Value], boundary: usize) -> Option<usize> {
    records[..boundary]
        .iter()
        .rposition(|r| is_chain_type(r) && !flag(r, "isSidechain") && str_of(r, "uuid").is_some())
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
///   not recovered and "not on the chain" would not mean "rewound" (a boundary
///   whose target is not in the file, or a hand-edited chain).
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

    // --- FUHANDOFF: the preserved-segment tail is written AFTER the boundary ---

    /// A current auto-compaction, shaped like the real thing: the boundary's
    /// `logicalParentUuid` is the preserved segment's tail, an attachment
    /// written after the boundary whose parentUuid chain runs
    /// boundary -> 3 attachments -> the compact summary -> tail.
    fn anomalous_block(n: u32) -> Vec<String> {
        let att = |k: u32, parent: String| {
            format!(r#"{{"type":"attachment","uuid":"att{n}{k}","parentUuid":"{parent}"}}"#)
        };
        vec![
            format!(
                r#"{{"type":"system","subtype":"compact_boundary","uuid":"b{n}","parentUuid":null,"logicalParentUuid":"t{n}","compactMetadata":{{"trigger":"auto","preservedSegment":{{"headUuid":"u1","anchorUuid":"s{n}","tailUuid":"t{n}"}},"preservedMessages":{{"uuids":["u1"],"allUuids":["u1"],"anchorUuid":"s{n}"}}}}}}"#
            ),
            att(1, format!("b{n}")),
            att(2, format!("att{n}1")),
            att(3, format!("att{n}2")),
            format!(
                r#"{{"type":"user","uuid":"s{n}","parentUuid":"att{n}3","isCompactSummary":true,"message":{{"role":"user","content":"summary"}}}}"#
            ),
            format!(r#"{{"type":"attachment","uuid":"t{n}","parentUuid":"s{n}"}}"#),
        ]
    }

    fn live_ids(d: &str) -> (Vec<String>, Vec<String>) {
        let records: Vec<Value> = d
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        let mut notes = Vec::new();
        let order = live_order(&records, &mut notes);
        let ids = order
            .into_iter()
            .map(|i| str_of(&records[i], "uuid").unwrap_or("").to_string())
            .collect();
        (ids, notes)
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_boundary_whose_target_is_written_after_it_continues_from_the_pre_boundary_leaf() {
        let mut lines = vec![
            rec("user", "u1", None),
            rec("assistant", "a1", Some("u1")),
            rec("user", "u2-old", Some("a1")),
            rec("assistant", "a2-old", Some("u2-old")),
            rec("user", "u2", Some("a1")),
            rec("assistant", "a2", Some("u2")),
            // A sidechain record written last before the boundary is never the leaf.
            r#"{"type":"assistant","uuid":"sc","parentUuid":"a2","isSidechain":true,"message":{"role":"assistant","content":"x"}}"#.to_string(),
        ];
        lines.extend(anomalous_block(1));
        lines.push(rec("user", "u3", Some("t1")));
        lines.push(rec("assistant", "a3", Some("u3")));
        let d = doc(&lines);
        let (ids, notes) = live_ids(&d);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(
            ids,
            strs(&[
                "u1", "a1", "u2", "a2", "b1", "att11", "att12", "att13", "s1", "t1", "u3", "a3"
            ])
        );
        // The rewound branch before the boundary is marked; the live turns
        // before and after it are not.
        let mut off = off_lines(&d).expect("the walk is now trusted");
        off.sort();
        assert_eq!(off, vec![2, 3]);
    }

    #[test]
    fn a_normal_and_an_anomalous_boundary_in_one_file_both_cross() {
        let mut lines = vec![
            rec("user", "u1", None),
            rec("assistant", "a1", Some("u1")),
            r#"{"type":"system","subtype":"compact_boundary","uuid":"cbA","parentUuid":null,"logicalParentUuid":"a1"}"#.to_string(),
            rec("user", "uA", Some("cbA")),
            rec("assistant", "aA", Some("uA")),
        ];
        lines.extend(anomalous_block(2));
        lines.push(rec("user", "u9", Some("t2")));
        lines.push(rec("assistant", "a9", Some("u9")));
        let d = doc(&lines);
        let (ids, notes) = live_ids(&d);
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(
            ids,
            strs(&[
                "u1", "a1", "cbA", "uA", "aA", "b2", "att21", "att22", "att23", "s2", "t2", "u9",
                "a9"
            ])
        );
        assert!(off_lines(&d).expect("trusted").is_empty());
    }

    #[test]
    fn a_boundary_with_nothing_before_it_ends_the_walk_without_a_cycle_note() {
        let mut lines = anomalous_block(1);
        lines.push(rec("user", "u3", Some("t1")));
        lines.push(rec("assistant", "a3", Some("u3")));
        let (ids, notes) = live_ids(&doc(&lines));
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(
            ids,
            strs(&["b1", "att11", "att12", "att13", "s1", "t1", "u3", "a3"])
        );
    }

    #[test]
    fn a_target_after_the_boundary_that_does_not_chain_back_to_it_is_left_alone() {
        // `z` is written after the boundary but its parent chain never reaches
        // it, so this is not the preserved-segment shape: the walk follows the
        // named target exactly as before and pulls in nothing from before the
        // boundary.
        let d = doc(&[
            rec("user", "u1", None),
            rec("assistant", "a1", Some("u1")),
            r#"{"type":"system","subtype":"compact_boundary","uuid":"cb","parentUuid":null,"logicalParentUuid":"z"}"#.to_string(),
            rec("user", "u2", Some("cb")),
            rec("user", "z", None),
            rec("assistant", "a2", Some("u2")),
        ]);
        let (ids, _) = live_ids(&d);
        assert!(!ids.contains(&"u1".to_string()), "{ids:?}");
        assert!(!ids.contains(&"a1".to_string()), "{ids:?}");
    }

    #[test]
    fn a_transcript_without_uuids_is_never_marked() {
        let d = r#"{"type":"user","message":{"role":"user","content":"hi"}}"#;
        assert!(chain_verdicts(d).is_none());
    }
}
