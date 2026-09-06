//! V71-D2 — the results PAGE's server half: the facet census and the
//! grouping of a page the lanes have already produced (design D3, "results
//! page: facet rail that writes the query, grouping, live preview").
//!
//! Two rules govern everything here, and both are about honesty rather than
//! cleverness.
//!
//! # 1. The basis is the PAGE, and it says so
//!
//! Every count in [`Facets`] is a count of hits **this response actually
//! returned** — after each lane's own `limit`, budget and truncation. It is
//! never a corpus-wide estimate. That is why [`Facets::basis`] is a wire
//! field with the literal value `"page"` rather than a comment: a facet
//! whose number disagrees with the rows visible underneath it is worse than
//! no facet at all, and the corpus-wide alternative would need a second full
//! scan per keystroke on a box whose text lane already cannot finish one
//! (D1b's finding). When a section is `truncated`, the caller can see both
//! facts and draw the honest conclusion.
//!
//! # 2. A facet WRITES the query; it is never hidden state
//!
//! Each [`FacetValue`] carries the kbcq/1 token that narrows the query to
//! it, so clicking a facet appends a clause to the string in the box — the
//! same discipline kb's root CLAUDE.md #35 records for `galleryUrl` (one
//! builder, golden-pinned, every facet click a shareable query). A facet
//! whose selection could not be expressed in the grammar is NOT EMITTED —
//! there is no such thing here as a filter you can see but cannot say.
//! [`tests::every_facet_field_writes_a_clause_that_reparses`] walks
//! [`FACET_FIELDS`] against `grammar::parse` and fails, by name, on a field
//! whose clause does not come back as the filter it claims — the v7.0
//! dead-surface defect class, closed for this surface.
//!
//! The `lane` field is the one that writes a lane PREFIX rather than a
//! `key:value` filter (kbcq/1 selects a lane by prefix, not by a key), which
//! is why [`FacetGroup::writes`] exists: a client must replace the prefix
//! there rather than append. Saying which of the two a group needs is the
//! whole reason the field is on the wire.
//!
//! # Grouping partitions, it never re-ranks
//!
//! [`group_hits`] assigns every row of a section to exactly one group, keeps
//! the rows in the order the lane ranked them, and orders the groups by
//! first appearance. It cannot drop a row (asserted) and it cannot promote
//! one. A lane that has no answer for the requested key (grouping
//! `sessions` by `kind`) puts its rows in ONE honestly-labelled group rather
//! than inventing a key or silently returning nothing.
//!
//! Rows are addressed by POSITION in the section's own `results` array
//! ([`HitGroup::indices`]). `hit_ids` rides alongside for the two lanes that
//! have one (V71-D1 gave stable ids to files and symbols); for the other
//! four it is EMPTY rather than fabricated, which is why positions are the
//! primary address.

use super::grammar::{self, GroupKey, Lane};
use serde::Serialize;

/// How many values one facet group may carry before the tail is dropped —
/// and the drop is REPORTED ([`FacetGroup::omitted`]), never silent.
pub const FACET_VALUE_CAP: usize = 12;

/// One facet field's DECLARATION. [`FACET_FIELDS`] is the single home for
/// the set: [`facets_for`] iterates it, [`clause_for`] renders from it, and
/// [`tests::every_facet_field_writes_a_clause_that_reparses`] walks it
/// against the real grammar.
#[derive(Debug, Clone, Copy)]
pub struct FacetFieldSpec {
    pub field: &'static str,
    pub label: &'static str,
    /// `"clause"` — [`FacetValue::clause`] is APPENDED to the query.
    /// `"prefix"` — it REPLACES the query's lane prefix (kbcq/1 selects a
    /// lane by prefix, so there is no `lane:` key to append).
    pub writes: &'static str,
}

pub const FACET_FIELDS: &[FacetFieldSpec] = &[
    FacetFieldSpec {
        field: "lane",
        label: "Lane",
        writes: "prefix",
    },
    FacetFieldSpec {
        field: "repo",
        label: "Repo",
        writes: "clause",
    },
    FacetFieldSpec {
        field: "lang",
        label: "Language",
        writes: "clause",
    },
    FacetFieldSpec {
        field: "ext",
        label: "Extension",
        writes: "clause",
    },
    FacetFieldSpec {
        field: "dir",
        label: "Directory",
        writes: "clause",
    },
    FacetFieldSpec {
        field: "kind",
        label: "Symbol kind",
        writes: "clause",
    },
];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FacetValue {
    pub value: String,
    /// Hits on THIS page carrying this value — see the module doc's basis
    /// rule. A text row counts its MATCHES, not itself (a text hit is a
    /// file with N matches, which is also what `--count-only` counts).
    pub count: usize,
    /// The kbcq/1 token that narrows the query to this value. Appended or
    /// prefix-substituted per the group's [`FacetGroup::writes`].
    pub clause: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FacetGroup {
    pub field: &'static str,
    pub label: &'static str,
    pub writes: &'static str,
    /// Descending by count, ties broken by value so the order is stable
    /// across two identical requests.
    pub values: Vec<FacetValue>,
    /// Values past [`FACET_VALUE_CAP`], dropped from `values` — reported so
    /// a rail never implies it is showing the whole census.
    #[serde(skip_serializing_if = "is_zero")]
    pub omitted: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// The whole census for one response — see the module doc.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Facets {
    /// Always `"page"`. See the module doc's basis rule.
    pub basis: &'static str,
    /// The sentence a client renders beside the rail, so the basis is
    /// visible to a human and not only to a schema reader.
    pub note: &'static str,
    /// Only groups with at least one value; a field nothing on the page can
    /// answer is absent, not present-and-empty.
    pub groups: Vec<FacetGroup>,
}

pub const FACETS_NOTE: &str =
    "counted over the hits on this page, not the whole corpus — a truncated lane's facets are \
     partial and its section says so";

/// One group of a section's hits under `group:`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HitGroup {
    /// The grouping VALUE (a path, a directory, a symbol kind, a lane
    /// name). Empty when the lane cannot answer the requested key.
    pub key: String,
    /// What a header should read — never empty, so a client never has to
    /// invent a caption for the "this lane has no such key" case.
    pub label: String,
    pub count: usize,
    /// Positions into the section's own `results` array, ascending — the
    /// primary address, because four of the six lanes carry no stable hit
    /// id (see the module doc).
    pub indices: Vec<usize>,
    /// The stable ids of the same rows, for the lanes that have them
    /// (files/symbols, V71-D1). EMPTY rather than fabricated elsewhere.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hit_ids: Vec<String>,
}

/// The kbcq/1 token that selects `value` for `field`. `None` for a field
/// this grammar cannot express (there is none today — the point of the
/// return type is that adding an inexpressible field FAILS the walk instead
/// of shipping a rail row that does nothing).
pub fn clause_for(field: &str, value: &str) -> Option<String> {
    let quoted = |v: &str| {
        if v.contains(' ') {
            format!("\"{v}\"")
        } else {
            v.to_string()
        }
    };
    match field {
        // A lane is selected by PREFIX, not a key — see `FacetGroup::writes`.
        "lane" => lane_from_str(value).and_then(|l| lane_prefix(l).map(str::to_string)),
        "repo" => Some(format!("repo:{}", quoted(value))),
        "lang" => Some(format!("lang:{}", quoted(value))),
        "ext" => Some(format!("ext:{}", quoted(value))),
        // A directory clause is a `path:` SUBSTRING with its trailing slash
        // kept, so `app/models` cannot also match `app/models_old`.
        "dir" => Some(format!("path:{}", quoted(&format!("{value}/")))),
        "kind" => Some(format!("kind:{}", quoted(value))),
        _ => None,
    }
}

fn lane_from_str(s: &str) -> Option<Lane> {
    grammar::LANE_ORDER.into_iter().find(|l| l.as_str() == s)
}

/// The prefix that selects one lane alone. The text lane's prefix carries a
/// pattern body, so a bare `/` is what a client must splice a pattern into —
/// which is exactly what the SPA's existing prefix-chip writer does.
fn lane_prefix(l: Lane) -> Option<&'static str> {
    match l {
        Lane::Files => Some("#"),
        Lane::Symbols => Some("@"),
        Lane::Text => Some("/"),
        Lane::Semantic => Some("?nl "),
        Lane::Sessions => Some("~"),
        Lane::Transcripts => Some("~~"),
    }
}

/// One row of one section, as the census and the grouper see it — read from
/// the lane-shaped JSON rather than six typed paths, so there is ONE grouper
/// and ONE census for all six lanes.
struct Row<'a> {
    path: Option<&'a str>,
    kind: Option<&'a str>,
    repo: Option<&'a str>,
    hit_id: Option<&'a str>,
    /// A text row is a FILE carrying N matches; every other lane's row is
    /// one hit. See [`FacetValue::count`].
    weight: usize,
}

fn rows_of<'a>(results: &'a serde_json::Value) -> Vec<Row<'a>> {
    let Some(arr) = results.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .map(|v| Row {
            path: v.get("path").and_then(|x| x.as_str()),
            kind: v.get("kind").and_then(|x| x.as_str()),
            repo: v.get("repo").and_then(|x| x.as_str()),
            hit_id: v.get("hit_id").and_then(|x| x.as_str()),
            weight: v
                .get("matches")
                .and_then(|m| m.as_array())
                .map(|m| m.len().max(1))
                .unwrap_or(1),
        })
        .collect()
}

/// The directory a path sits in — everything before the last `/`, or `""`
/// for a repo-root file (which is a real, nameable place, so it gets the
/// label `(repo root)` rather than being dropped).
fn dir_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

/// The lowercase extension of a path, or `None` for an extensionless file
/// (a `Rakefile`, a `Dockerfile`) — which is not an error and not an empty
/// extension, so it simply contributes nothing to the `ext` facet.
fn ext_of(path: &str) -> Option<String> {
    let base = path.rsplit('/').next().unwrap_or(path);
    let dot = base.rfind('.')?;
    if dot == 0 || dot + 1 >= base.len() {
        return None;
    }
    Some(base[dot + 1..].to_ascii_lowercase())
}

/// One `(lane, results)` pair the census should count — the caller hands
/// over exactly the sections it is returning, so the basis rule holds by
/// construction.
pub struct CountedSection<'a> {
    pub lane: &'a str,
    pub results: &'a serde_json::Value,
}

/// The facet census over every section of one response. Pure: no store, no
/// clock, no config.
pub fn facets_for(sections: &[CountedSection<'_>]) -> Facets {
    // `Vec<(value, count)>` rather than a map, kept in first-appearance
    // order, so the sort below is the ONLY thing that decides output order
    // (a `HashMap` iteration would make two identical requests disagree).
    let mut tally: Vec<(&'static str, String, usize)> = Vec::new();
    let mut bump = |field: &'static str, value: String, by: usize| {
        if value.is_empty() && field != "dir" {
            return;
        }
        match tally
            .iter_mut()
            .find(|(f, v, _)| *f == field && *v == value)
        {
            Some(entry) => entry.2 += by,
            None => tally.push((field, value, by)),
        }
    };

    for sec in sections {
        let rows = rows_of(sec.results);
        if rows.is_empty() {
            continue;
        }
        let lane_weight: usize = rows.iter().map(|r| r.weight).sum();
        bump("lane", sec.lane.to_string(), lane_weight);
        for row in &rows {
            if let Some(repo) = row.repo {
                bump("repo", repo.to_string(), row.weight);
            }
            if let Some(kind) = row.kind {
                bump("kind", kind.to_string(), row.weight);
            }
            let Some(path) = row.path else { continue };
            bump("dir", dir_of(path).to_string(), row.weight);
            if let Some(ext) = ext_of(path) {
                bump("ext", ext, row.weight);
            }
            if let Some(info) = crate::lang::detect(path, None) {
                bump("lang", info.id.to_string(), row.weight);
            }
        }
    }

    let mut groups: Vec<FacetGroup> = Vec::new();
    for spec in FACET_FIELDS {
        let mut values: Vec<FacetValue> = tally
            .iter()
            .filter(|(f, _, _)| *f == spec.field)
            .filter_map(|(_, v, n)| {
                clause_for(spec.field, v).map(|clause| FacetValue {
                    value: v.clone(),
                    count: *n,
                    clause,
                })
            })
            .collect();
        if values.is_empty() {
            continue;
        }
        values.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.cmp(&b.value)));
        let omitted = values.len().saturating_sub(FACET_VALUE_CAP);
        values.truncate(FACET_VALUE_CAP);
        groups.push(FacetGroup {
            field: spec.field,
            label: spec.label,
            writes: spec.writes,
            values,
            omitted,
        });
    }

    Facets {
        basis: "page",
        note: FACETS_NOTE,
        groups,
    }
}

/// The label a group header reads for `key` under `by` — never empty, so a
/// client never invents a caption. The "this lane has no such key" case is
/// stated in words rather than left blank.
fn group_label(by: GroupKey, key: &str) -> String {
    match (by, key) {
        (_, "") if by == GroupKey::Dir => "(repo root)".to_string(),
        (GroupKey::File, "") => "(no path)".to_string(),
        (GroupKey::Kind, "") => "(no symbol kind)".to_string(),
        _ => key.to_string(),
    }
}

/// Partition one section's `results` by `by`. See the module doc: this
/// never re-ranks and never drops a row.
pub fn group_hits(lane: &str, results: &serde_json::Value, by: GroupKey) -> Vec<HitGroup> {
    if by == GroupKey::None {
        return Vec::new();
    }
    let rows = rows_of(results);
    let mut out: Vec<HitGroup> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let key = match by {
            GroupKey::File => row.path.unwrap_or("").to_string(),
            GroupKey::Dir => row.path.map(dir_of).unwrap_or("").to_string(),
            GroupKey::Kind => row.kind.unwrap_or("").to_string(),
            GroupKey::Lane => lane.to_string(),
            GroupKey::None => unreachable!("returned above"),
        };
        // `GroupKey::File`/`Dir` on a lane with no path both collapse to the
        // empty key, which `group_label` renders honestly rather than
        // dropping the rows.
        match out.iter_mut().find(|g| g.key == key) {
            Some(g) => {
                g.count += 1;
                g.indices.push(i);
                if let Some(id) = row.hit_id {
                    g.hit_ids.push(id.to_string());
                }
            }
            None => out.push(HitGroup {
                label: group_label(by, &key),
                key,
                count: 1,
                indices: vec![i],
                hit_ids: row
                    .hit_id
                    .map(|id| vec![id.to_string()])
                    .unwrap_or_default(),
            }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn files_section() -> serde_json::Value {
        json!([
            {"repo": "kb", "path": "app/models/order.rb", "hit_id": "h-1", "score": 1.0},
            {"repo": "kb", "path": "app/models/user.rb", "hit_id": "h-2", "score": 0.9},
            {"repo": "kb", "path": "README.md", "hit_id": "h-3", "score": 0.5},
        ])
    }

    fn counted<'a>(lane: &'a str, v: &'a serde_json::Value) -> CountedSection<'a> {
        CountedSection { lane, results: v }
    }

    /// THE dead-surface walk for this module: a facet row a human can click
    /// must write a kbcq/1 token the daemon's own parser reads back as the
    /// filter the row claimed. A field whose clause does not round-trip is a
    /// rail row that visibly does nothing — the v7.0 defect class.
    #[test]
    fn every_facet_field_writes_a_clause_that_reparses() {
        for spec in FACET_FIELDS {
            let sample = match spec.field {
                "lane" => "symbols",
                "repo" => "kb",
                "lang" => "ruby",
                "ext" => "rb",
                "dir" => "app/models",
                "kind" => "class",
                other => panic!("{other}: add a sample to this walk"),
            };
            let clause = clause_for(spec.field, sample).unwrap_or_else(|| {
                panic!(
                    "{}: FACET_FIELDS declares it, clause_for does not build it",
                    spec.field
                )
            });
            match spec.writes {
                "prefix" => {
                    let p = grammar::parse(&format!("{clause}needle"));
                    assert_eq!(
                        p.lanes.iter().map(|l| l.as_str()).collect::<Vec<_>>(),
                        vec![sample],
                        "{}: `{clause}` did not select the {sample} lane alone",
                        spec.field
                    );
                }
                "clause" => {
                    let p = grammar::parse(&format!("needle {clause}"));
                    assert!(
                        p.diagnostics.is_empty(),
                        "{}: `{clause}` warned: {:?}",
                        spec.field,
                        p.diagnostics
                    );
                    assert_eq!(
                        p.query, "needle",
                        "{}: `{clause}` was not consumed as a filter",
                        spec.field
                    );
                    let landed = match spec.field {
                        "repo" => p.filters.repo.as_deref() == Some(sample),
                        "lang" => p.filters.lang.as_deref() == Some(sample),
                        "ext" => p.filters.ext == vec![sample.to_string()],
                        "dir" => p.filters.path.as_deref() == Some("app/models/"),
                        "kind" => p.filters.kind == vec![sample.to_string()],
                        _ => false,
                    };
                    assert!(
                        landed,
                        "{}: `{clause}` parsed but landed nowhere the lanes read",
                        spec.field
                    );
                }
                other => panic!("{}: unknown `writes` value {other:?}", spec.field),
            }
        }
    }

    #[test]
    fn facets_count_the_page_and_name_their_basis() {
        let files = files_section();
        let f = facets_for(&[counted("files", &files)]);
        assert_eq!(f.basis, "page");
        assert!(f.note.contains("this page"));
        let dirs = f.groups.iter().find(|g| g.field == "dir").unwrap();
        assert_eq!(dirs.values[0].value, "app/models");
        assert_eq!(dirs.values[0].count, 2);
        assert_eq!(dirs.values[0].clause, "path:app/models/");
        // A repo-root file is a real place with a nameable clause, not a
        // dropped row.
        let root = dirs.values.iter().find(|v| v.value.is_empty()).unwrap();
        assert_eq!(root.count, 1);
        let lanes = f.groups.iter().find(|g| g.field == "lane").unwrap();
        assert_eq!(lanes.values[0].value, "files");
        assert_eq!(lanes.values[0].count, 3);
        assert_eq!(lanes.values[0].clause, "#");
    }

    #[test]
    fn a_text_row_is_weighted_by_its_matches_not_by_itself() {
        let text = json!([
            {"path": "app/a.rb", "matches": [{"line_no": 1}, {"line_no": 2}, {"line_no": 9}]},
            {"path": "app/b.rb", "matches": [{"line_no": 4}]},
        ]);
        let f = facets_for(&[counted("text", &text)]);
        let lanes = f.groups.iter().find(|g| g.field == "lane").unwrap();
        assert_eq!(lanes.values[0].count, 4, "3 + 1 matches, not 2 files");
    }

    #[test]
    fn a_field_nothing_on_the_page_answers_is_absent_not_empty() {
        let sessions = json!([{"session_id": "s-1", "title": "x"}]);
        let f = facets_for(&[counted("sessions", &sessions)]);
        assert!(f.groups.iter().all(|g| g.field != "kind"));
        assert!(f.groups.iter().all(|g| g.field != "dir"));
        assert!(f.groups.iter().any(|g| g.field == "lane"));
    }

    #[test]
    fn the_value_cap_reports_what_it_dropped() {
        let rows: Vec<serde_json::Value> = (0..FACET_VALUE_CAP + 5)
            .map(|i| json!({"repo": "kb", "path": format!("d{i}/f.rs")}))
            .collect();
        let v = serde_json::Value::Array(rows);
        let f = facets_for(&[counted("files", &v)]);
        let dirs = f.groups.iter().find(|g| g.field == "dir").unwrap();
        assert_eq!(dirs.values.len(), FACET_VALUE_CAP);
        assert_eq!(dirs.omitted, 5);
    }

    /// The other half of grammar's `every_declared_group_key_parses`: a key
    /// that parses must actually GROUP. Every declared key partitions the
    /// page — every row in exactly one group, no row lost, no row duplicated.
    #[test]
    fn every_group_key_partitions_the_page() {
        let files = files_section();
        let n = files.as_array().unwrap().len();
        for by in grammar::GROUP_KEYS {
            let groups = group_hits("files", &files, by);
            if by == GroupKey::None {
                assert!(groups.is_empty(), "group:none must produce no groups");
                continue;
            }
            let total: usize = groups.iter().map(|g| g.count).sum();
            assert_eq!(total, n, "group:{} lost or duplicated rows", by.as_str());
            let mut seen: Vec<usize> = groups.iter().flat_map(|g| g.indices.clone()).collect();
            seen.sort_unstable();
            assert_eq!(
                seen,
                (0..n).collect::<Vec<_>>(),
                "group:{} did not partition the page",
                by.as_str()
            );
            for g in &groups {
                assert!(
                    !g.label.is_empty(),
                    "group:{} left a blank header",
                    by.as_str()
                );
            }
        }
    }

    #[test]
    fn grouping_keeps_the_lanes_own_order_inside_a_group() {
        let files = files_section();
        let groups = group_hits("files", &files, GroupKey::Dir);
        assert_eq!(groups[0].key, "app/models");
        assert_eq!(groups[0].indices, vec![0, 1]);
        assert_eq!(groups[0].hit_ids, vec!["h-1", "h-2"]);
        // Groups come out in FIRST-APPEARANCE order, not alphabetical — the
        // best-ranked row's group leads.
        assert_eq!(groups[1].key, "");
        assert_eq!(groups[1].label, "(repo root)");
    }

    #[test]
    fn a_lane_with_no_answer_gets_one_honest_group_not_zero() {
        let sessions = json!([{"session_id": "s-1"}, {"session_id": "s-2"}]);
        let groups = group_hits("sessions", &sessions, GroupKey::Kind);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].count, 2);
        assert_eq!(groups[0].label, "(no symbol kind)");
        assert!(groups[0].hit_ids.is_empty(), "never a fabricated id");
    }
}
