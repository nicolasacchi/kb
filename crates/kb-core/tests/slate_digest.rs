//! SL1 goldens for the `kb-slate/1` projection (design §19
//! "Verification" → "Board-amendment goldens").
//!
//! Every fixture in `tests/slate_fixtures/` is a hand-authored ledger of
//! INVENTED strings — no real path, person or session id appears here, the
//! same rule `tests/session_fixtures/` follows. Each is projected at ONE
//! fixed clock ([`NOW`]) so the rendered digest is reproducible from the
//! bytes alone: `slate::project` takes `now_unix`, the [`LivePolicy`] and
//! the presence slice as ARGUMENTS and reads no clock of its own (§7).
//!
//! What is pinned here:
//!
//! - `render()` for every fixture (insta snapshot), which is byte-identical
//!   to what `kb slate open` prints and to `DigestResponse.text`;
//! - `render(&digest) == digest.text`, so the two can never drift;
//! - the chunk-equivalence property: folding `project_append` over ANY
//!   chunking of a ledger yields the same shown set and the same final
//!   digest as one `project` call over the whole thing — including drop
//!   chains, edit chains, marks, pins and done;
//! - the `displaced` before/after diff.

use kb_core::sessions::live::{LivePolicy, StateSource};
use kb_core::slate::{
    self, parse_ledger, CursorRow, Post, Presence, ProjectOpts, ProjectState, SlateDigest,
};
use std::collections::BTreeMap;

/// 2026-01-01T00:00:00Z — the fixed clock every fixture's ages are
/// measured back from (`tests/../gen`-style synthetic offsets, never
/// wall-clock).
const NOW: i64 = 1_767_225_600;

const FIXTURES: &[&str] = &[
    "plain",
    "drop-chain",
    "edit-chain",
    "marks-pins",
    "contested-takes",
    "stale-takes",
    "hands-acked",
    "asks-answers",
    "topics",
    "over-budget",
    "seen-cursors",
];

fn load(name: &str) -> Vec<Post> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/slate_fixtures")
        .join(format!("{name}.jsonl"));
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    parse_ledger(&raw).unwrap_or_else(|e| panic!("{path:?}: {e}"))
}

/// The beat slice the ROUTE would extract from `LiveRegistry::snapshot`.
/// Only `stale-takes` needs one: it pins the rule that a beat BEATS the
/// post timeline (a session that posted 70 minutes ago but beat 3 minutes
/// ago is Working, not Stalled).
fn presence_for(name: &str) -> Vec<Presence> {
    match name {
        "stale-takes" => vec![Presence {
            session_id: "8f2a5b7c99".to_string(),
            last_activity_unix: NOW - 180,
            source: StateSource::Hook,
        }],
        _ => Vec::new(),
    }
}

/// The `meta.json` cursor map the ROUTE would hand the projection (D27).
/// Only `seen-cursors` reports any: every other fixture keeps an EMPTY
/// map, which is what makes their goldens byte-identical to v0.41 — the
/// `seen by N` suffix is suppressed at N = 0.
///
/// The three reports are deliberately staggered so one golden shows all
/// three arithmetic rules at once: codex has been served the whole slate,
/// kimi only through its own ask, and the READING session (claude/4b7e)
/// only through the hand.
fn cursors_for(name: &str) -> BTreeMap<String, CursorRow> {
    if name != "seen-cursors" {
        return BTreeMap::new();
    }
    [
        ("8f2a5b7c99", 5u64, "codex"),
        ("3d5e01aa42", 3, "kimi"),
        ("4b7e91c2a0", 2, "claude"),
    ]
    .into_iter()
    .map(|(sid, seq, harness)| {
        (
            sid.to_string(),
            CursorRow {
                seq,
                harness: harness.to_string(),
                at: NOW - 60,
            },
        )
    })
    .collect()
}

fn opts_for(name: &str) -> ProjectOpts {
    ProjectOpts {
        slug: "orchard".to_string(),
        header_context: Some("main /srv/orchard".to_string()),
        who: Some("claude/4b7e".to_string()),
        since: Some(1),
        session_id: Some("4b7e91c2a0".to_string()),
        // The over-budget fixture is the one that must actually truncate;
        // the rest read at the shipped default.
        budget: if name == "over-budget" {
            1_400
        } else {
            slate::BUDGET_OPEN
        },
        cursors: cursors_for(name),
        ..ProjectOpts::default()
    }
}

fn project_fixture(name: &str) -> SlateDigest {
    let posts = load(name);
    slate::project(
        &posts,
        NOW,
        &LivePolicy::default(),
        &presence_for(name),
        &opts_for(name),
    )
}

#[test]
fn slate_digest_goldens() {
    for name in FIXTURES {
        let d = project_fixture(name);
        insta::assert_snapshot!(format!("digest_{}", name.replace('-', "_")), d.text);
    }
}

/// The projection fills `text` by calling `render` on itself; a second
/// call must return the same bytes, or the CLI and the wire would drift.
#[test]
fn slate_render_is_idempotent_over_the_projection() {
    for name in FIXTURES {
        let d = project_fixture(name);
        assert_eq!(slate::render(&d), d.text, "{name}");
    }
}

/// Determinism: the same bytes and the same clock project to the same
/// text, twice (no HashMap iteration order, no clock, no randomness).
#[test]
fn slate_projection_is_deterministic() {
    for name in FIXTURES {
        assert_eq!(
            project_fixture(name).text,
            project_fixture(name).text,
            "{name}"
        );
    }
}

/// §19's chunk-equivalence golden: `project_append` folds to `project`
/// for EVERY chunk size, on every fixture — drop chains, edit chains,
/// marks, pins and done included. The delta is a diff of two full
/// projections, so this holds by construction; the test is what keeps a
/// future "optimisation" from making it an assumption.
#[test]
fn slate_folding_deltas_equals_one_projection() {
    for name in FIXTURES {
        let posts = load(name);
        let policy = LivePolicy::default();
        let presence = presence_for(name);
        let opts = opts_for(name);
        let whole = slate::project(&posts, NOW, &policy, &presence, &opts);
        // A delta reports BOARD membership. What the budget pushes off the
        // digest is a rendering fact reported separately as `displaced`
        // (§5), so the fold is compared against the untruncated set.
        let membership = slate::project(
            &posts,
            NOW,
            &policy,
            &presence,
            &ProjectOpts {
                all: true,
                ..opts.clone()
            },
        );

        for chunk in 1..=posts.len().max(1) {
            let mut state = ProjectState::new(NOW, &policy, &presence, &opts);
            // The fold a delta consumer performs: add what entered, remove
            // what the batch hid.
            let mut folded: std::collections::BTreeSet<u64> = Default::default();
            for batch in posts.chunks(chunk) {
                let delta = slate::project_append(&mut state, batch);
                for p in &delta.posts {
                    folded.insert(p.seq);
                }
                for h in &delta.hides {
                    folded.remove(&h.hide);
                }
            }
            assert_eq!(
                folded,
                membership.shown_seqs(),
                "{name}: folded delta shown-set diverged at chunk size {chunk}"
            );
            let finished = slate::project_finish(state);
            assert_eq!(
                finished.text, whole.text,
                "{name}: project_finish diverged at chunk size {chunk}"
            );
        }
    }
}

/// Every `hide` a delta emits names a real reason and a real hiding post
/// — an honest tombstone, never a silent disappearance (D17).
#[test]
fn slate_delta_hides_name_the_actor_and_the_reason() {
    let posts = load("drop-chain");
    let policy = LivePolicy::default();
    let opts = opts_for("drop-chain");
    let mut state = ProjectState::new(NOW, &policy, &[], &opts);
    let mut hides = Vec::new();
    for batch in posts.chunks(1) {
        hides.extend(slate::project_append(&mut state, batch).hides);
    }
    assert!(!hides.is_empty(), "the drop-chain fixture must hide posts");
    for h in &hides {
        assert!(h.by > h.hide, "the hiding post is always later: {h:?}");
        assert!(!h.who.is_empty(), "every hide names who: {h:?}");
    }
    insta::assert_debug_snapshot!("hides_drop_chain", hides);
}

/// `displaced` is the poster's only feedback loop with the finite surface
/// (§5): project at the default budget before and after the append inside
/// the same lock, and diff the shown set.
#[test]
fn slate_displaced_golden() {
    let posts = load("over-budget");
    let policy = LivePolicy::default();
    let opts = opts_for("over-budget");
    // Split inside the found/idea run: only a post in the SAME section can
    // displace another (unspent share flows forward, never back).
    let split = 10;
    let before = slate::project(&posts[..split], NOW, &policy, &[], &opts);
    let after = slate::project(&posts, NOW, &policy, &[], &opts);
    let (displaced, total) = slate::displaced(&before, &after);
    assert!(total > 0, "the over-budget fixture must displace something");
    assert!(displaced.len() <= slate::DISPLACED_CAP);
    for d in &displaced {
        assert!(
            !matches!(
                d.kind,
                kb_core::slate::Kind::Now | kb_core::slate::Kind::Warn
            ),
            "NOW and WARN are never displaced: {d:?}"
        );
    }
    insta::assert_debug_snapshot!("displaced_over_budget", (displaced, total));
}

/// D7's hybrid block: now, warn, unacknowledged hands and answers to the
/// session's OWN asks in full; counts for the rest; the echo line stays.
#[test]
fn slate_hybrid_block_golden() {
    let posts = load("plain");
    let opts = ProjectOpts {
        mode: slate::Mode::Hybrid,
        budget: slate::BUDGET_HYBRID,
        ..opts_for("plain")
    };
    let d = slate::project(&posts, NOW, &LivePolicy::default(), &[], &opts);
    assert!(d.sections.take.is_empty() && d.sections.found_idea.is_empty());
    assert!(!d.echo.is_empty(), "the hybrid block carries the echo line");
    insta::assert_snapshot!("digest_plain_hybrid", d.text);
}

/// `--all` never truncates and never displaces (D18: the board is finite,
/// the ledger is not).
#[test]
fn slate_all_mode_shows_everything() {
    let posts = load("over-budget");
    let opts = ProjectOpts {
        all: true,
        ..opts_for("over-budget")
    };
    let d = slate::project(&posts, NOW, &LivePolicy::default(), &[], &opts);
    assert_eq!(d.sections.found_idea.len(), d.found_idea_total);
    assert_eq!(d.sections.tried.len(), d.tried_total);
    assert!(!d.found_idea_truncated && !d.tried_truncated);
}

// ---------------------------------------------------------------------------
// D27 — seen is a reported cursor (v0.42)
// ---------------------------------------------------------------------------

/// The derivation, spelled out on the fixture the golden renders: a
/// session counts for a post when its REPORTED cursor has reached that
/// post's seq, and the post's own author is never counted.
#[test]
fn slate_seen_by_counts_every_other_session_whose_cursor_reached_the_post() {
    let d = project_fixture("seen-cursors");
    let now = &d.sections.now[0];
    assert_eq!(now.seq, 1);
    // The operator's NOW carries no session id, so nobody is the author
    // and all three reporters count.
    assert_eq!(now.seen_by, vec!["3d5e", "4b7e", "8f2a"], "{now:?}");

    let hand = &d.sections.hand[0];
    assert_eq!(hand.seq, 2);
    assert_eq!(
        hand.seen_by,
        vec!["3d5e", "4b7e"],
        "codex authored #2 and must not count itself"
    );

    let ask = &d.sections.ask[0];
    assert_eq!(ask.seq, 3);
    assert_eq!(
        ask.seen_by,
        vec!["8f2a"],
        "claude's cursor is #2 and has not reached #3; kimi authored it"
    );

    // Derived on EVERY kind — the three-kind rule is about RENDERING, not
    // about the derivation. Codex's cursor reached #5 and claude authored
    // it, so the fact exists; the FOUND line just never prints it.
    let found = &d.sections.found_idea[0];
    assert_eq!(found.seq, 5);
    assert_eq!(found.seen_by, vec!["8f2a"], "{found:?}");
}

/// The rendering rule: `seen by N` on WHOLE-tier NOW/HAND/ASK lines and
/// nowhere else. The `found` at #5 has a reader (codex, cursor #5) but is
/// a knowledge line, so it never carries the suffix.
#[test]
fn slate_seen_by_renders_only_on_whole_tier_now_hand_and_ask() {
    let text = project_fixture("seen-cursors").text;
    assert!(text.contains("seen by 3"), "{text}");
    assert!(text.contains("UNACKNOWLEDGED · seen by 2"), "{text}");
    assert!(text.contains("seen by 1"), "{text}");
    let found_line = text
        .lines()
        .find(|l| l.contains("the guard is skipped"))
        .unwrap_or_default();
    assert!(
        !found_line.contains("seen by"),
        "a FOUND line never carries the suffix: {found_line:?}"
    );
}

/// No cursor reported ⇒ not one byte changes, and `seen_by` is the ONLY
/// thing a cursor map touches. This is the compatibility contract the
/// other ten fixtures' goldens already encode; asserting it directly is
/// what stops a future "always render seen by 0" — or a cursor map that
/// quietly starts reordering.
#[test]
fn slate_cursor_map_changes_seen_by_and_nothing_else() {
    let posts = load("seen-cursors");
    let with_cursors = opts_for("seen-cursors");
    let without = ProjectOpts {
        cursors: BTreeMap::new(),
        ..with_cursors.clone()
    };
    let mut a = slate::project(&posts, NOW, &LivePolicy::default(), &[], &with_cursors);
    let b = slate::project(&posts, NOW, &LivePolicy::default(), &[], &without);
    assert!(a.text.contains("seen by"));
    assert!(!b.text.contains("seen by"));

    let s = &mut a.sections;
    for lane in [
        &mut s.now,
        &mut s.warn,
        &mut s.hand,
        &mut s.ask,
        &mut s.take,
        &mut s.found_idea,
        &mut s.tried,
    ] {
        for p in lane.iter_mut() {
            p.seen_by.clear();
        }
    }
    for t in a.header.topics.iter_mut() {
        if let Some(p) = t.now.as_mut() {
            p.seen_by.clear();
        }
    }
    a.text = slate::render(&a);
    assert_eq!(a, b, "seen_by is the only field a cursor map writes");
}

/// A cursor is presence-INDEPENDENT (D27): the seen set is identical with
/// an empty presence slice and with a live one, because a report is not a
/// beat. The fixture's beat would otherwise flip take liveness, which is
/// exactly the axis `seen_by` must not ride.
#[test]
fn slate_seen_by_is_independent_of_presence() {
    let posts = load("seen-cursors");
    let opts = opts_for("seen-cursors");
    let live = vec![Presence {
        session_id: "3d5e01aa42".to_string(),
        last_activity_unix: NOW - 30,
        source: StateSource::Hook,
    }];
    let a = slate::project(&posts, NOW, &LivePolicy::default(), &[], &opts);
    let b = slate::project(&posts, NOW, &LivePolicy::default(), &live, &opts);
    let seen = |d: &SlateDigest| -> Vec<Vec<String>> {
        d.shown().into_iter().map(|p| p.seen_by.clone()).collect()
    };
    assert_eq!(seen(&a), seen(&b));
}
