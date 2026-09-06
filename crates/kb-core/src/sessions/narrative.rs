//! CT-E5 — one session's STORY, ordered.
//!
//! The pure half of "materialise a session thread as a kb-list/1 reading
//! list in narrative order". The server owns the storage reads (they are
//! the EXISTING per-session reads — `session_files_for_session`,
//! `list_docs_with_kb_session`, `memory_recalls_for_session`, each already
//! newest-capture scoped per invariant #11); this module owns the ordering,
//! the de-duplication, the per-entry notes, and the list description. No
//! clock, no I/O, no storage — every function here is a pure fn of its
//! arguments, so the ordering is golden-testable without a daemon.
//!
//! The narrative is four lanes, in this order:
//!
//! 1. **capture** — the session transcript artifact itself, first;
//! 2. **touched** — the artifacts the session read/edited, in the order the
//!    per-session file manifest already returns them (edits before reads);
//! 3. **produced** — memories whose lance `kb_session` names this session;
//! 4. **recalled** — memories the session's `kb-recall` hook injected, in
//!    `memory_recalls` ledger order (oldest first).
//!
//! An artifact that appears in more than one lane keeps its FIRST (earliest
//! lane) position — a memory that was both recalled and later edited is one
//! entry, told at the point the story first reaches it.
//!
//! **kb NEVER calls kb-code** (invariant #2's ONE call direction). The
//! kb-code session-diff URL this module renders into the list description is
//! built from the inert `[kb.*] code_url` string and is a LINK, never a
//! fetch — and when no `code_url` is configured the line is omitted rather
//! than emitted dead.

/// Which lane of the narrative an entry came from. The declaration order IS
/// the narrative order — `narrative_entries` walks the lanes in this
/// sequence and the first lane to claim an artifact wins it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Lane {
    /// The session transcript artifact itself.
    Capture,
    /// An artifact the session read, wrote, or edited.
    Touched,
    /// A memory the session produced (lance `kb_session` names it).
    Produced,
    /// A memory the session recalled (the `memory_recalls` ledger).
    Recalled,
}

impl Lane {
    /// The stable, human-facing lane label written into the entry note. Kept
    /// short — the list detail renders notes under the entry line.
    pub fn label(self) -> &'static str {
        match self {
            Lane::Capture => "session capture",
            Lane::Touched => "touched",
            Lane::Produced => "memory produced",
            Lane::Recalled => "memory recalled",
        }
    }
}

/// One candidate artifact for a lane, as the caller's existing per-session
/// read already ordered it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneItem {
    /// The artifact's corpus. Only items whose `kb` equals the list's own kb
    /// survive — a kb reading list is single-corpus by construction (the
    /// lists read path enriches every entry against ONE `KbContext`), so a
    /// foreign-corpus item would render as a tombstone. See
    /// [`narrative_entries`]'s `kb` argument.
    pub kb: String,
    pub artifact_id: String,
    /// Lane-specific detail appended to the note after a `·` (the file
    /// action for a touch, `used` for a recall the session went on to
    /// reference). `None` ⇒ the bare lane label.
    pub detail: Option<String>,
}

impl LaneItem {
    /// Convenience constructor for a detail-less item.
    pub fn new(kb: impl Into<String>, artifact_id: impl Into<String>) -> Self {
        Self {
            kb: kb.into(),
            artifact_id: artifact_id.into(),
            detail: None,
        }
    }

    /// Convenience constructor carrying a lane detail.
    pub fn with_detail(
        kb: impl Into<String>,
        artifact_id: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            kb: kb.into(),
            artifact_id: artifact_id.into(),
            detail: Some(detail.into()),
        }
    }
}

/// The four lanes for ONE session, each already ordered WITHIN itself by the
/// caller's existing per-session read. `Default` is the all-empty session —
/// a session with no capture, no touches, no memories yields ZERO entries,
/// never a phantom.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SessionLanes {
    pub capture: Option<LaneItem>,
    pub touched: Vec<LaneItem>,
    pub produced: Vec<LaneItem>,
    pub recalled: Vec<LaneItem>,
}

/// One ordered narrative entry — what the route turns into a
/// [`crate::lists::NewListEntry`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NarrativeEntry {
    pub kb: String,
    pub artifact_id: String,
    pub lane: Lane,
    /// The rendered entry note (`"<lane label>"` or `"<lane label> · <detail>"`).
    pub note: String,
}

/// Upper bound on narrative entries emitted per session. A long session can
/// touch hundreds of files; the tail is truncated AFTER ordering, so the
/// capture and the earliest touches are the ones that survive. Bounded, not
/// silent: the route reports the drop in the list description.
pub const ENTRIES_PER_SESSION_CAP: usize = 250;

/// Upper bound on sessions one narrative save may expand. A thread is a
/// handful of sessions; beyond this the route 400s rather than quietly
/// truncating the story.
pub const SESSIONS_PER_SAVE_CAP: usize = 50;

/// How many sessions the description names individually before collapsing
/// the rest into a `… and N more` line.
pub const DESC_SESSION_CAP: usize = 10;

/// Order one session's four lanes into narrative entries.
///
/// * `kb` — the list's own corpus. Lane items from any OTHER corpus are
///   DROPPED (returned in the skipped count), because a kb reading list is
///   single-corpus: `routes::lists::enrich_artifacts` resolves every entry
///   against one `KbContext`, so a foreign id would render as `(removed)`.
///   Dropping is the honest option; a tombstone entry is a phantom.
/// * `cap` — max entries returned ([`ENTRIES_PER_SESSION_CAP`] in the route).
///
/// Returns the ordered entries plus the number of story items left out
/// (foreign-corpus items + the truncated tail), so the caller can say so.
pub fn narrative_entries(
    lanes: &SessionLanes,
    kb: &str,
    cap: usize,
) -> (Vec<NarrativeEntry>, usize) {
    let mut out: Vec<NarrativeEntry> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut skipped = 0usize;

    // The capture is an `Option`, the rest are `Vec`s; `Option::as_slice`
    // lets all four walk one loop, so the lane order lives in exactly one
    // place (this array) rather than in four copy-pasted blocks.
    let lanes_in_order: [(Lane, &[LaneItem]); 4] = [
        (Lane::Capture, lanes.capture.as_slice()),
        (Lane::Touched, lanes.touched.as_slice()),
        (Lane::Produced, lanes.produced.as_slice()),
        (Lane::Recalled, lanes.recalled.as_slice()),
    ];

    for (lane, items) in lanes_in_order {
        for item in items {
            if item.artifact_id.is_empty() {
                continue;
            }
            if item.kb != kb {
                // Foreign corpus — countable, never listed.
                skipped += 1;
                continue;
            }
            if !seen.insert(item.artifact_id.clone()) {
                // Already told earlier in the story; a duplicate is not a
                // skipped item, it is the same item.
                continue;
            }
            let note = match item.detail.as_deref().filter(|d| !d.is_empty()) {
                Some(d) => format!("{} · {d}", lane.label()),
                None => lane.label().to_string(),
            };
            out.push(NarrativeEntry {
                kb: item.kb.clone(),
                artifact_id: item.artifact_id.clone(),
                lane,
                note,
            });
        }
    }

    if out.len() > cap {
        skipped += out.len() - cap;
        out.truncate(cap);
    }
    (out, skipped)
}

/// One session's identity stamp for the list description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStamp {
    pub session_id: String,
    /// Unix secs — the session's `started_at`. `0`/negative ⇒ the capture
    /// date is omitted rather than rendered as the epoch.
    pub started_at: i64,
}

/// The one-line explanation of the ordering, always the description's first
/// line so an imported/exported kb-list/1 file carries its own contract.
pub const NARRATIVE_ORDER_LINE: &str =
    "Narrative order: capture → files touched → memories produced → memories recalled.";

/// `YYYY-MM-DD` (UTC) for a unix timestamp; `None` for a non-positive or
/// unrepresentable one.
fn ymd(ts: i64) -> Option<String> {
    if ts <= 0 {
        return None;
    }
    chrono::DateTime::from_timestamp(ts, 0).map(|dt| dt.format("%Y-%m-%d").to_string())
}

/// True for a session id safe to splice into a URL path segment verbatim.
/// Session ids are uuids in practice; anything outside the RFC 3986
/// unreserved set is refused rather than percent-encoded, so a weird id
/// yields NO link instead of a guessed one.
fn url_safe_segment(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~'))
}

/// Validate an operator-configured `[kb.*] code_url` and return it without a
/// trailing slash. Absolute `http`/`https` only, a non-empty host, no
/// whitespace or control characters, no fragment/query (it is a base). This
/// is the Rust mirror of the SPA's `validCodeUrl` — kb-side link rendering
/// only; kb never dials the URL (invariant #2).
pub fn valid_code_url(raw: Option<&str>) -> Option<String> {
    let raw = raw?.trim();
    if raw.is_empty() || raw.len() > 512 {
        return None;
    }
    if raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    if raw.contains('#') || raw.contains('?') {
        return None;
    }
    let rest = raw
        .strip_prefix("https://")
        .or_else(|| raw.strip_prefix("http://"))?;
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() {
        return None;
    }
    Some(raw.trim_end_matches('/').to_string())
}

/// The kb-code session-diff URL for one session — `<code_url>/session/<sid>/diff`,
/// the `web-code` SPA route. `None` when no `[kb.*] code_url` is configured,
/// when it is not a valid absolute http(s) base, or when the session id is
/// not a URL-safe path segment. Never a dead or guessed URL.
pub fn session_diff_url(code_url: Option<&str>, session_id: &str) -> Option<String> {
    let base = valid_code_url(code_url)?;
    if !url_safe_segment(session_id) {
        return None;
    }
    Some(format!("{base}/session/{session_id}/diff"))
}

/// Render the reading list's description: the ordering contract, one block
/// per session (id · capture date · the kb-code session-diff link when a
/// `code_url` is configured), and — when non-zero — an honest count of the
/// story items that could not become entries.
///
/// Pure and deterministic; `stamps` is the caller's display order.
pub fn narrative_description(
    stamps: &[SessionStamp],
    code_url: Option<&str>,
    skipped: usize,
) -> String {
    let mut out = String::new();
    out.push_str(NARRATIVE_ORDER_LINE);
    for stamp in stamps.iter().take(DESC_SESSION_CAP) {
        out.push('\n');
        match ymd(stamp.started_at) {
            Some(d) => out.push_str(&format!("Session {} · captured {d}", stamp.session_id)),
            None => out.push_str(&format!("Session {}", stamp.session_id)),
        }
        if let Some(url) = session_diff_url(code_url, &stamp.session_id) {
            out.push('\n');
            out.push_str(&format!("Session diff: {url}"));
        }
    }
    if stamps.len() > DESC_SESSION_CAP {
        out.push('\n');
        let more = stamps.len() - DESC_SESSION_CAP;
        out.push_str(&format!(
            "… and {more} more session{}.",
            if more == 1 { "" } else { "s" }
        ));
    }
    if skipped > 0 {
        out.push('\n');
        out.push_str(&format!(
            "{skipped} story item{} omitted (other corpora, or past the per-session cap).",
            if skipped == 1 { "" } else { "s" }
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lanes() -> SessionLanes {
        SessionLanes {
            capture: Some(LaneItem::new("mem", "cap00000000a")),
            touched: vec![
                LaneItem::with_detail("mem", "touch0000001", "edit"),
                LaneItem::with_detail("mem", "touch0000002", "read"),
            ],
            produced: vec![LaneItem::new("mem", "made00000001")],
            recalled: vec![LaneItem::with_detail("mem", "recall000001", "used")],
        }
    }

    // ---- ordering goldens ------------------------------------------------

    #[test]
    fn narrative_order_is_capture_touched_produced_recalled() {
        let (out, skipped) = narrative_entries(&lanes(), "mem", ENTRIES_PER_SESSION_CAP);
        assert_eq!(skipped, 0);
        let got: Vec<(&str, Lane, &str)> = out
            .iter()
            .map(|e| (e.artifact_id.as_str(), e.lane, e.note.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("cap00000000a", Lane::Capture, "session capture"),
                ("touch0000001", Lane::Touched, "touched · edit"),
                ("touch0000002", Lane::Touched, "touched · read"),
                ("made00000001", Lane::Produced, "memory produced"),
                ("recall000001", Lane::Recalled, "memory recalled · used"),
            ]
        );
    }

    #[test]
    fn each_lane_preserves_its_callers_order() {
        let l = SessionLanes {
            touched: vec![
                LaneItem::with_detail("mem", "z", "edit"),
                LaneItem::with_detail("mem", "a", "edit"),
                LaneItem::with_detail("mem", "m", "read"),
            ],
            ..Default::default()
        };
        let (out, _) = narrative_entries(&l, "mem", ENTRIES_PER_SESSION_CAP);
        let ids: Vec<&str> = out.iter().map(|e| e.artifact_id.as_str()).collect();
        assert_eq!(ids, vec!["z", "a", "m"], "the manifest order is the order");
    }

    #[test]
    fn an_empty_session_yields_no_entries_and_no_phantoms() {
        let (out, skipped) =
            narrative_entries(&SessionLanes::default(), "mem", ENTRIES_PER_SESSION_CAP);
        assert!(out.is_empty());
        assert_eq!(skipped, 0);
    }

    #[test]
    fn a_capture_only_session_is_exactly_one_entry() {
        let l = SessionLanes {
            capture: Some(LaneItem::new("mem", "only")),
            ..Default::default()
        };
        let (out, _) = narrative_entries(&l, "mem", ENTRIES_PER_SESSION_CAP);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].lane, Lane::Capture);
    }

    #[test]
    fn each_lane_alone_renders_only_its_own_entries() {
        let base = lanes();
        for (lane, l) in [
            (
                Lane::Touched,
                SessionLanes {
                    touched: base.touched.clone(),
                    ..Default::default()
                },
            ),
            (
                Lane::Produced,
                SessionLanes {
                    produced: base.produced.clone(),
                    ..Default::default()
                },
            ),
            (
                Lane::Recalled,
                SessionLanes {
                    recalled: base.recalled.clone(),
                    ..Default::default()
                },
            ),
        ] {
            let (out, skipped) = narrative_entries(&l, "mem", ENTRIES_PER_SESSION_CAP);
            assert!(!out.is_empty(), "{lane:?} lane must render");
            assert!(out.iter().all(|e| e.lane == lane), "{lane:?} lane only");
            assert_eq!(skipped, 0);
        }
    }

    #[test]
    fn an_artifact_in_two_lanes_keeps_its_earliest_position_once() {
        let l = SessionLanes {
            capture: Some(LaneItem::new("mem", "shared")),
            touched: vec![LaneItem::with_detail("mem", "shared", "edit")],
            produced: vec![LaneItem::new("mem", "shared")],
            recalled: vec![LaneItem::new("mem", "shared")],
        };
        let (out, skipped) = narrative_entries(&l, "mem", ENTRIES_PER_SESSION_CAP);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].lane, Lane::Capture);
        assert_eq!(skipped, 0, "a duplicate is the same item, not a drop");
    }

    #[test]
    fn a_repeat_within_one_lane_is_deduped() {
        let l = SessionLanes {
            touched: vec![
                LaneItem::with_detail("mem", "f", "edit"),
                LaneItem::with_detail("mem", "f", "read"),
            ],
            ..Default::default()
        };
        let (out, _) = narrative_entries(&l, "mem", ENTRIES_PER_SESSION_CAP);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].note, "touched · edit");
    }

    #[test]
    fn foreign_corpus_items_are_skipped_not_tombstoned() {
        let l = SessionLanes {
            capture: Some(LaneItem::new("mem", "cap")),
            produced: vec![
                LaneItem::new("other", "elsewhere1"),
                LaneItem::new("mem", "here1"),
            ],
            recalled: vec![LaneItem::new("other", "elsewhere2")],
            ..Default::default()
        };
        let (out, skipped) = narrative_entries(&l, "mem", ENTRIES_PER_SESSION_CAP);
        let ids: Vec<&str> = out.iter().map(|e| e.artifact_id.as_str()).collect();
        assert_eq!(ids, vec!["cap", "here1"]);
        assert_eq!(skipped, 2);
    }

    #[test]
    fn empty_artifact_ids_are_ignored() {
        let l = SessionLanes {
            touched: vec![LaneItem::new("mem", ""), LaneItem::new("mem", "real")],
            ..Default::default()
        };
        let (out, _) = narrative_entries(&l, "mem", ENTRIES_PER_SESSION_CAP);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].artifact_id, "real");
    }

    #[test]
    fn the_cap_truncates_the_tail_and_counts_the_drop() {
        let l = SessionLanes {
            capture: Some(LaneItem::new("mem", "cap")),
            touched: (0..10)
                .map(|i| LaneItem::with_detail("mem", format!("t{i}"), "edit"))
                .collect(),
            ..Default::default()
        };
        let (out, skipped) = narrative_entries(&l, "mem", 3);
        let ids: Vec<&str> = out.iter().map(|e| e.artifact_id.as_str()).collect();
        assert_eq!(ids, vec!["cap", "t0", "t1"], "the capture always survives");
        assert_eq!(skipped, 8);
    }

    #[test]
    fn a_zero_cap_yields_nothing() {
        let (out, skipped) = narrative_entries(&lanes(), "mem", 0);
        assert!(out.is_empty());
        assert_eq!(skipped, 5);
    }

    // ---- code_url validation --------------------------------------------

    #[test]
    fn valid_code_url_accepts_http_and_https_and_trims_the_slash() {
        assert_eq!(
            valid_code_url(Some("https://kbc.example.com/")).as_deref(),
            Some("https://kbc.example.com")
        );
        assert_eq!(
            valid_code_url(Some("http://127.0.0.1:4100")).as_deref(),
            Some("http://127.0.0.1:4100")
        );
        assert_eq!(
            valid_code_url(Some("  https://kbc.example.com  ")).as_deref(),
            Some("https://kbc.example.com")
        );
    }

    #[test]
    fn valid_code_url_refuses_junk() {
        for raw in [
            "",
            "   ",
            "kbc.example.com",
            "ftp://kbc.example.com",
            "javascript:alert(1)",
            "https://",
            "https:// kbc.example.com",
            "https://kbc.example.com?x=1",
            "https://kbc.example.com#frag",
            "https://kbc.example.com\nX",
        ] {
            assert!(valid_code_url(Some(raw)).is_none(), "must refuse {raw:?}");
        }
        assert!(valid_code_url(None).is_none());
    }

    #[test]
    fn session_diff_url_is_the_web_code_route() {
        assert_eq!(
            session_diff_url(Some("https://kbc.example.com"), "abc-123").as_deref(),
            Some("https://kbc.example.com/session/abc-123/diff")
        );
    }

    #[test]
    fn session_diff_url_is_none_without_a_code_url() {
        assert!(session_diff_url(None, "abc-123").is_none());
        assert!(session_diff_url(Some(""), "abc-123").is_none());
    }

    #[test]
    fn session_diff_url_refuses_an_unsafe_session_id() {
        for sid in ["", "a/b", "a b", "a?b", "a#b", "../etc", "a%2f"] {
            assert!(
                session_diff_url(Some("https://kbc.example.com"), sid).is_none(),
                "must refuse sid {sid:?}"
            );
        }
    }

    // ---- description goldens --------------------------------------------

    fn stamp(id: &str, ts: i64) -> SessionStamp {
        SessionStamp {
            session_id: id.to_string(),
            started_at: ts,
        }
    }

    /// 2026-05-24T10:00:00Z.
    const T: i64 = 1_779_616_800;

    #[test]
    fn description_with_code_url_carries_the_session_diff_link() {
        let got = narrative_description(&[stamp("sid-1", T)], Some("https://kbc.example.com/"), 0);
        assert_eq!(
            got,
            "Narrative order: capture → files touched → memories produced → memories recalled.\n\
             Session sid-1 · captured 2026-05-24\n\
             Session diff: https://kbc.example.com/session/sid-1/diff"
        );
    }

    #[test]
    fn description_without_code_url_omits_the_link_cleanly() {
        let got = narrative_description(&[stamp("sid-1", T)], None, 0);
        assert_eq!(
            got,
            "Narrative order: capture → files touched → memories produced → memories recalled.\n\
             Session sid-1 · captured 2026-05-24"
        );
        assert!(!got.contains("diff"));
    }

    #[test]
    fn description_with_an_invalid_code_url_omits_the_link_cleanly() {
        let got = narrative_description(&[stamp("sid-1", T)], Some("not a url"), 0);
        assert!(!got.contains("Session diff"), "never a dead URL: {got}");
    }

    #[test]
    fn description_omits_an_unrepresentable_capture_date() {
        let got = narrative_description(&[stamp("sid-1", 0)], None, 0);
        assert_eq!(
            got,
            "Narrative order: capture → files touched → memories produced → memories recalled.\n\
             Session sid-1"
        );
        assert!(!got.contains("1970"));
    }

    #[test]
    fn description_lists_every_session_of_a_thread_in_order() {
        let got = narrative_description(
            &[stamp("s1", T), stamp("s2", T + 86_400)],
            Some("https://kbc.example.com"),
            0,
        );
        let s1 = got.find("Session s1").unwrap();
        let s2 = got.find("Session s2").unwrap();
        assert!(s1 < s2);
        assert!(got.contains("https://kbc.example.com/session/s1/diff"));
        assert!(got.contains("https://kbc.example.com/session/s2/diff"));
    }

    #[test]
    fn description_collapses_past_the_session_cap() {
        let stamps: Vec<SessionStamp> = (0..DESC_SESSION_CAP + 3)
            .map(|i| stamp(&format!("s{i}"), T))
            .collect();
        let got = narrative_description(&stamps, None, 0);
        assert!(got.contains("… and 3 more sessions."));
        assert!(!got.contains(&format!("Session s{}", DESC_SESSION_CAP)));
    }

    #[test]
    fn description_reports_skipped_items_honestly() {
        let got = narrative_description(&[stamp("s1", T)], None, 1);
        assert!(got.ends_with("1 story item omitted (other corpora, or past the per-session cap)."));
        let got = narrative_description(&[stamp("s1", T)], None, 4);
        assert!(got.contains("4 story items omitted"));
    }

    #[test]
    fn description_says_nothing_about_skips_when_there_are_none() {
        let got = narrative_description(&[stamp("s1", T)], None, 0);
        assert!(!got.contains("omitted"));
    }

    #[test]
    fn description_of_no_sessions_is_just_the_order_line() {
        assert_eq!(narrative_description(&[], None, 0), NARRATIVE_ORDER_LINE);
    }

    // The description rides the kb-list/1 grammar as a blockquote; every
    // line must survive `md::to_markdown` → `md::parse_markdown` unchanged.
    #[test]
    fn description_round_trips_through_the_kb_list_grammar() {
        let desc = narrative_description(&[stamp("sid-1", T)], Some("https://kbc.example.com"), 2);
        let doc = crate::lists::ListExport {
            schema: crate::lists::EXPORT_SCHEMA.to_string(),
            kb: Some("mem".to_string()),
            list_id: Some("l_000000000000".to_string()),
            title: "Story".to_string(),
            description: Some(desc.clone()),
            pinned: false,
            archived: false,
            entries: Vec::new(),
        };
        let md = crate::lists::md::to_markdown(&doc);
        let back = crate::lists::md::parse_markdown(&md).expect("round trip parses");
        assert_eq!(back.description.as_deref(), Some(desc.as_str()));
    }
}
