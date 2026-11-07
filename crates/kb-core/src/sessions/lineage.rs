//! Session lineage fold (V0047, Track A).
//!
//! Pure, no I/O: given a session file path and the header ids the caller
//! already read, fold the omp on-disk tree into `(parent, root)`. The
//! hook's `cap_lineage` shell twin implements the same rules; the shared
//! fixture `tests/fixtures/session_lineage_cases.json` pins both.
//!
//! Layout facts (synthetic; the verbatim omp on-disk layout and whether
//! `parentSession` is an absolute/relative path are UNVERIFIED — the
//! operator's host scan saw only directory/file names):
//!
//! * Early-September headers carry no `parentSession`: a top-level file is
//!   `<ts>_<id>.jsonl` directly in a bucket dir; a depth-1 child lives at
//!   `<bucket>/<ts>_<id>/<Name>.jsonl` (the `<ts>_<id>` ancestor dir IS the
//!   top session's stem); a nested child lives at
//!   `<bucket>/<ts>_<id>/<Name>/<Name>.<Child>.jsonl`.
//! * October headers carry `parentSession` (a path to the parent file) on
//!   60/60 sampled files: `parent` is then the `id` read from THAT file by
//!   the caller (`parent_header_id`).
//!
//! Roots are NOT self-referenced: a top-level file yields `{None, None}`.
//! A child whose root is known never gets `parent: None` except for a
//! self-parent (a parent equal to the child's own id is dropped) or an
//! unsafe id; the directory fallback floors `parent` at `root`.

/// Folded lineage for one session file: `(parent, root)` session ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lineage {
    /// The parent session id, if any (never the child's own id).
    pub parent: Option<String>,
    /// The root (top) session id of this file's tree, if any.
    pub root: Option<String>,
}

/// A plain token: non-empty, at most 128 bytes, starts alphanumeric, and
/// only alphanumerics or `.` `_` `:` `-` after that (copies
/// `sessions::resume::is_plain_token` semantics).
pub(crate) fn is_plain_token_pub(s: &str) -> bool {
    is_plain_token(s)
}

fn is_plain_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.as_bytes()
            .iter()
            .all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b':' | b'-'))
}

/// The id after the FINAL `_` of a `<ts>_<id>` ancestor component, if the
/// component looks like one (a non-empty prefix and a plain-token suffix).
fn stem_id(component: &str) -> Option<&str> {
    let pos = component.rfind('_')?;
    let (prefix, suffix) = component.split_at(pos);
    if prefix.is_empty() {
        return None;
    }
    let id = &suffix[1..];
    if id.is_empty() {
        return None;
    }
    Some(id)
}

/// Fold one session file's path and header ids into its lineage.
///
/// `session_file` is the file's own path; `header_id` is the `id` from its
/// `session` header; `parent_session_file` is the header `parentSession`
/// path (if any) and `parent_header_id` is the `id` read from THAT file by
/// the caller (`None` if the parent file was unreadable/absent).
pub fn fold_lineage(
    session_file: &str,
    header_id: &str,
    parent_session_file: Option<&str>,
    parent_header_id: Option<&str>,
) -> Lineage {
    let own_valid = is_plain_token(header_id);
    let components: Vec<&str> = session_file.split('/').collect();
    // Ancestor directories: every component before the file name.
    let ancestors = &components[..components.len().saturating_sub(1)];

    // Root: the id after the final `_` of the OUTERMOST `<ts>_<id>`
    // ancestor component (the top session's stem). The stem is used
    // directly; the top file's header is never read.
    let mut root: Option<String> = None;
    let mut root_pos: usize = 0;
    for (i, comp) in ancestors.iter().enumerate() {
        if let Some(id) = stem_id(comp) {
            if !is_plain_token(id) {
                root = None;
                root_pos = i;
                break;
            }
            // A root equal to the child's own id is not a root.
            if own_valid && id == header_id {
                root = None;
                root_pos = i;
                continue;
            }
            root = Some(id.to_string());
            root_pos = i;
            break;
        }
    }
    // No `<ts>_<id>` ancestor: a top-level file (or an unsafe stem).
    let root = match root {
        Some(r) if is_plain_token(&r) && (!own_valid || r != header_id) => Some(r),
        _ => None,
    };
    if root.is_none() {
        return Lineage {
            parent: None,
            root: None,
        };
    }
    let root = root.unwrap();

    // Depth: components between the root dir and the file. Depth-1 is the
    // file directly in the root dir; anything deeper is nested.
    let file_comps = ancestors.len().saturating_sub(root_pos + 1);
    let depth_one = file_comps == 0;

    // Parent: an explicitly read parent header wins; otherwise the
    // directory fallback (depth-1 -> root; deeper -> a supplied sibling
    // header id, else root as a floor).
    let mut parent: Option<String> = None;
    let mut explicit_self = false;
    if parent_session_file.is_some() {
        if let Some(ph) = parent_header_id {
            if ph == header_id {
                explicit_self = true;
            } else if is_plain_token(ph) {
                parent = Some(ph.to_string());
            }
        }
    }
    if parent.is_none() && !explicit_self {
        if !depth_one {
            if let Some(ph) = parent_header_id {
                if is_plain_token(ph) && ph != header_id {
                    parent = Some(ph.to_string());
                }
            }
        }
        if parent.is_none() {
            parent = Some(root.clone());
        }
    }
    if parent.as_deref() == Some(header_id) {
        parent = None;
    }
    Lineage {
        parent,
        root: Some(root),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Case {
        name: &'static str,
        session_file: &'static str,
        header_id: &'static str,
        parent_session_file: Option<&'static str>,
        parent_header_id: Option<&'static str>,
        expect: (Option<&'static str>, Option<&'static str>),
    }

    const CASES: &[Case] = &[
        Case {
            name: "top-level",
            session_file: "/sessions/bucket/2026-09-01T10-00-00-000Z_root0001.jsonl",
            header_id: "root0001",
            parent_session_file: None,
            parent_header_id: None,
            expect: (None, None),
        },
        Case {
            name: "oct-depth-1-with-parentSession",
            session_file: "/sessions/bucket/2026-09-01T10-00-00-000Z_root0001/child0001.jsonl",
            header_id: "child0001",
            parent_session_file: Some("/sessions/bucket/2026-09-01T10-00-00-000Z_root0001.jsonl"),
            parent_header_id: Some("root0001"),
            expect: (Some("root0001"), Some("root0001")),
        },
        Case {
            name: "sep-depth-1-without-parentSession",
            session_file: "/sessions/bucket/2026-09-01T10-00-00-000Z_root0001/child0002.jsonl",
            header_id: "child0002",
            parent_session_file: None,
            parent_header_id: None,
            expect: (Some("root0001"), Some("root0001")),
        },
        Case {
            name: "nested-depth-2-with-sibling-header",
            session_file:
                "/sessions/bucket/2026-09-01T10-00-00-000Z_root0001/Worker/worker.child0003.jsonl",
            header_id: "child0003",
            parent_session_file: None,
            parent_header_id: Some("child0002"),
            expect: (Some("child0002"), Some("root0001")),
        },
        Case {
            name: "nested-depth-2-without-sibling-header",
            session_file:
                "/sessions/bucket/2026-09-01T10-00-00-000Z_root0001/Worker/worker.child0004.jsonl",
            header_id: "child0004",
            parent_session_file: None,
            parent_header_id: None,
            expect: (Some("root0001"), Some("root0001")),
        },
        Case {
            name: "self-parent",
            session_file: "/sessions/bucket/2026-09-01T10-00-00-000Z_root0001/child0005.jsonl",
            header_id: "child0005",
            parent_session_file: Some(
                "/sessions/bucket/2026-09-01T10-00-00-000Z_root0001/child0005.jsonl",
            ),
            parent_header_id: Some("child0005"),
            expect: (None, Some("root0001")),
        },
        Case {
            name: "unsafe-root-id",
            session_file: "/sessions/bucket/2026-09-01T10-00-00-000Z_bad id!/child0006.jsonl",
            header_id: "child0006",
            parent_session_file: None,
            parent_header_id: None,
            expect: (None, None),
        },
        Case {
            name: "unreadable-parent",
            session_file: "/sessions/bucket/2026-09-01T10-00-00-000Z_root0001/child0007.jsonl",
            header_id: "child0007",
            parent_session_file: Some("/sessions/bucket/2026-09-01T10-00-00-000Z_root0001.jsonl"),
            parent_header_id: None,
            expect: (Some("root0001"), Some("root0001")),
        },
        Case {
            name: "oct-depth-1-parent-distinct-from-root",
            session_file: "/sessions/bucket/2026-09-01T10-00-00-000Z_root0001/child0008.jsonl",
            header_id: "child0008",
            parent_session_file: Some("/sessions/bucket/2026-09-01T10-00-00-000Z_root0001.jsonl"),
            parent_header_id: Some("root0001"),
            expect: (Some("root0001"), Some("root0001")),
        },
    ];

    #[test]
    fn fold_lineage_matches_the_shared_cases() {
        let raw = include_str!("../../tests/fixtures/session_lineage_cases.json");
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        let arr = v.as_array().unwrap();
        assert!(arr.len() >= 8, "need >= 8 shared cases, got {}", arr.len());
        // The in-code table pins the same expectations as the fixture.
        assert_eq!(arr.len(), CASES.len());
        for (i, elem) in arr.iter().enumerate() {
            let c = &CASES[i];
            assert_eq!(elem["name"].as_str().unwrap(), c.name);
            assert_eq!(elem["session_file"].as_str().unwrap(), c.session_file);
            assert_eq!(elem["header_id"].as_str().unwrap(), c.header_id);
            let got = fold_lineage(
                c.session_file,
                c.header_id,
                c.parent_session_file,
                c.parent_header_id,
            );
            let (ep, er) = (c.expect.0, c.expect.1);
            // Fixture twin-check: the JSON expects must agree with the table.
            let fp = elem["expect"]["parent"].as_str();
            let fr = elem["expect"]["root"].as_str();
            assert_eq!(fp, ep, "fixture/table parent drift in {}", c.name);
            assert_eq!(fr, er, "fixture/table root drift in {}", c.name);
            assert_eq!(got.parent.as_deref(), ep, "parent mismatch in {}", c.name);
            assert_eq!(got.root.as_deref(), er, "root mismatch in {}", c.name);
        }
    }

    /// Whether `needle` occurs as a standalone identifier — so
    /// `claude_project_slug` (the pre-existing Claude Code slug helper)
    /// does not trip the `project_slug` needle. The V0047 lineage field
    /// itself appears as `project_slug` standalone (column, struct
    /// field, variable), so real lineage reads still match.
    fn contains_word(src: &str, needle: &str) -> bool {
        fn is_ident(c: u8) -> bool {
            c.is_ascii_alphanumeric() || c == b'_'
        }
        let bytes = src.as_bytes();
        src.match_indices(needle).any(|(i, _)| {
            let before_ok = i == 0 || !is_ident(bytes[i - 1]);
            let after = i + needle.len();
            let after_ok = after >= bytes.len() || !is_ident(bytes[after]);
            before_ok && after_ok
        })
    }

    // invariant:11
    #[test]
    fn no_ranking_module_reads_session_lineage() {
        const RANKERS: &[(&str, &str)] = &[
            ("memory.rs", include_str!("../memory.rs")),
            ("fusion.rs", include_str!("../fusion.rs")),
            ("resurface.rs", include_str!("../resurface.rs")),
            ("triage.rs", include_str!("../triage.rs")),
            ("query.rs", include_str!("../query.rs")),
            ("sessions/projects.rs", include_str!("projects.rs")),
        ];
        for (name, src) in RANKERS {
            for needle in ["parent_session_id", "root_session_id", "project_slug"] {
                assert!(
                    !contains_word(src, needle),
                    "{name} names `{needle}` -- session lineage is SURFACED, NEVER SCORED \
                     (V0047's own header, invariant #11): it may be rendered beside a row, \
                     never folded into a rank, a sort key or a filter"
                );
            }
            assert!(
                !src.contains("lineage::"),
                "{name} names `lineage::` -- session lineage fold must not reach a ranker"
            );
        }
    }
}
