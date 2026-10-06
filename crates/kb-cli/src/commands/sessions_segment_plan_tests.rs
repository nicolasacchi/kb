//! Tests for `kb sessions segment-plan`. Synthetic sessions are built by `Gen`
//! with EXACT line sizes so every cut-rule test states its arithmetic.

use super::*;
use std::process::Command;

// --- synthetic sessions ---------------------------------------------------------

struct Gen {
    lines: Vec<String>,
    next: u64,
    leaf: Option<String>,
    /// Ids of the entries that ARE on the leaf chain, in order.
    main: Vec<String>,
}

fn pad(n: usize) -> String {
    "x".repeat(n)
}

impl Gen {
    fn new() -> Gen {
        let mut g = Gen {
            lines: Vec::new(),
            next: 1,
            leaf: None,
            main: Vec::new(),
        };
        g.lines.push(
            json!({"type": "session", "version": 3, "id": "sess-1", "cwd": "/c",
                   "timestamp": "2026-01-01T00:00:00Z", "title": "t"})
            .to_string(),
        );
        g.lines
            .push(json!({"type": "title", "title": "  Slot title  "}).to_string());
        g
    }

    /// Append a record whose whole line (with newline) is exactly `total`
    /// bytes. `mk(pad_len)` builds the record around a pad of that length.
    fn put(&mut self, mk: &dyn Fn(usize) -> Value, parent: Option<String>, total: usize) -> String {
        let id = format!("e{:06}", self.next);
        self.next += 1;
        let stamp = |mut rec: Value| {
            rec["id"] = json!(id.clone());
            rec["parentId"] = json!(parent.clone());
            rec
        };
        let base = stamp(mk(0)).to_string().len() + 1;
        assert!(total >= base, "line of {total} bytes cannot hold {base}");
        self.lines.push(stamp(mk(total - base)).to_string());
        id
    }

    fn chain(&mut self, mk: &dyn Fn(usize) -> Value, total: usize) -> String {
        let p = self.leaf.clone();
        let id = self.put(mk, p, total);
        self.leaf = Some(id.clone());
        self.main.push(id.clone());
        id
    }

    fn user(&mut self, total: usize) -> String {
        self.chain(
            &|n| {
                json!({"type": "message", "message": {"role": "user",
                    "content": [{"type": "text", "text": format!("u{}", pad(n))}]}})
            },
            total,
        )
    }

    fn assistant(&mut self, total: usize) -> String {
        self.chain(
            &|n| {
                json!({"type": "message", "message": {"role": "assistant", "model": "m",
                    "content": [{"type": "text", "text": format!("a{}", pad(n))}]}})
            },
            total,
        )
    }

    fn call(&mut self, call: &str, total: usize) -> String {
        let call = call.to_string();
        self.chain(
            &move |n| {
                json!({"type": "message", "message": {"role": "assistant", "model": "m",
                    "content": [{"type": "text", "text": format!("c{}", pad(n))},
                                {"type": "toolCall", "id": call.clone(), "name": "bash",
                                 "arguments": {"command": "ls"}}]}})
            },
            total,
        )
    }

    fn result(&mut self, call: &str, total: usize) -> String {
        let call = call.to_string();
        self.chain(
            &move |n| {
                json!({"type": "message", "message": {"role": "toolResult",
                    "toolCallId": call.clone(), "toolName": "bash", "isError": false,
                    "content": [{"type": "text", "text": format!("r{}", pad(n))}]}})
            },
            total,
        )
    }

    fn compaction(&mut self, total: usize) -> String {
        self.chain(
            &|n| json!({"type": "compaction", "summary": format!("s{}", pad(n)), "shortSummary": "h"}),
            total,
        )
    }

    fn reset(&mut self) -> String {
        self.chain(&|_| json!({"type": "reset_boundary"}), 100)
    }

    /// `k` abandoned entries hanging off the current leaf; the leaf and the
    /// chain do not move.
    fn side_branch(&mut self, k: usize) {
        let mut parent = self.leaf.clone();
        for _ in 0..k {
            let id = self.put(
                &|n| {
                    json!({"type": "message", "message": {"role": "assistant", "model": "m",
                        "content": [{"type": "text", "text": format!("ABANDONED{}", pad(n))}]}})
                },
                parent.clone(),
                200,
            );
            parent = Some(id);
        }
    }

    fn text(&self) -> String {
        let mut s = self.lines.join("\n");
        s.push('\n');
        s
    }
}

fn opts(target: u64) -> Opts {
    Opts {
        target_bytes: target,
        adapter_ver: "t1".to_string(),
        print_chain: true,
        write_state: true,
    }
}

struct Fx {
    dir: tempfile::TempDir,
}

impl Fx {
    fn new() -> Fx {
        Fx {
            dir: tempfile::tempdir().unwrap(),
        }
    }
    fn src(&self) -> PathBuf {
        self.dir.path().join("session.jsonl")
    }
    fn state(&self) -> PathBuf {
        self.dir.path().join("state").join("plan.state")
    }
    fn write(&self, g: &Gen) {
        std::fs::write(self.src(), g.text()).unwrap();
    }
    fn plan(&self, target: u64) -> PlanResult {
        plan_file(&self.src(), &self.state(), &opts(target)).unwrap()
    }
    /// A plan from an empty checkpoint, never persisted.
    fn fresh(&self, target: u64) -> PlanResult {
        let mut o = opts(target);
        o.write_state = false;
        plan_file(&self.src(), &self.dir.path().join("nonexistent.state"), &o).unwrap()
    }
}

/// `(first_id, last_id, n_entries, start_offset, end_offset, bytes, frozen, cut)`.
type Shape = (String, String, u64, u64, u64, u64, bool, &'static str);

fn shapes(r: &PlanResult) -> Vec<Shape> {
    r.parts
        .iter()
        .map(|p| {
            (
                p.first_id.clone(),
                p.last_id.clone(),
                p.n_entries,
                p.start_offset,
                p.end_offset,
                p.bytes,
                p.frozen,
                p.cut,
            )
        })
        .collect()
}

fn jid(s: &str) -> String {
    Value::String(s.to_string()).to_string()
}

fn chain_ids(r: &PlanResult) -> Vec<String> {
    r.json["chain_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

// --- chain rules ------------------------------------------------------------------

#[test]
fn leaf_chain_ignores_abandoned_branches_and_title_slot() {
    let mut g = Gen::new();
    g.user(200);
    g.assistant(200);
    g.side_branch(3);
    g.user(200);
    g.side_branch(1);
    g.assistant(200);
    let fx = Fx::new();
    fx.write(&g);
    let r = fx.plan(1 << 20);
    assert_eq!(chain_ids(&r), g.main);
    assert_eq!(r.parts.len(), 1);
    assert!(!r.parts[0].frozen);
    assert_eq!(r.json["session_id"], "sess-1");
    assert_eq!(r.json["cwd"], "/c");
    assert_eq!(r.json["title"], "Slot title");
    assert_eq!(r.json["dmodel"], "omp");
}

#[test]
fn reset_boundary_cuts_everything_at_or_before_it() {
    let mut g = Gen::new();
    g.user(200);
    g.assistant(200);
    g.reset();
    g.main.clear(); // the chain the planner reports starts AFTER the reset
    let u = g.user(200);
    let a = g.assistant(200);
    let fx = Fx::new();
    fx.write(&g);
    let r = fx.plan(1 << 20);
    assert_eq!(chain_ids(&r), vec![u, a]);

    // a trailing reset leaves nothing to capture
    let mut g2 = Gen::new();
    g2.user(200);
    g2.reset();
    let fx2 = Fx::new();
    fx2.write(&g2);
    let r2 = fx2.plan(1 << 20);
    assert!(chain_ids(&r2).is_empty());
    assert!(r2.parts.is_empty());
}

#[test]
fn dmodel_is_the_last_model_change_on_the_live_chain() {
    let mut g = Gen::new();
    g.chain(&|_| json!({"type": "model_change", "model": "prov/a"}), 100);
    g.user(200);
    g.chain(&|_| json!({"type": "model_change", "model": "prov/b"}), 100);
    let fx = Fx::new();
    fx.write(&g);
    assert_eq!(fx.plan(1 << 20).json["dmodel"], "prov/b");
    // a falsy model on the LAST model_change falls back to "omp" like jq's `// "omp"`
    g.chain(&|_| json!({"type": "model_change", "model": null}), 100);
    fx.write(&g);
    assert_eq!(fx.plan(1 << 20).json["dmodel"], "omp");
}

// --- cut rules ---------------------------------------------------------------------

#[test]
fn a_call_result_pair_is_never_split_by_a_non_hard_cut() {
    // omp records no user message between a tool call and its result (queued
    // steering is parked until the batch ends), so cuts - which only fall
    // before user messages - cannot split a pair. T=2000, half=1000: u1@1100
    // and u2@1840 are the window candidates; the pair c1 spans 1300..1740.
    let mut g = Gen::new();
    g.user(600);
    g.assistant(500);
    g.user(200); // u1
    g.call("c1", 240);
    g.result("c1", 200);
    g.assistant(200);
    let u2 = g.user(200);
    g.assistant(800);
    let fx = Fx::new();
    fx.write(&g);
    let r = fx.plan(2000);
    assert_eq!(r.parts.len(), 2, "{:?}", shapes(&r));
    assert_eq!(r.parts[0].cut, "user");
    assert_eq!(r.parts[1].first_id, jid(&u2));
    assert_pairs_whole(&r);
}

/// Every part holds the `toolCall` ids and the `toolResult` ids of the same
/// set of calls (no call separated from its result by a part boundary).
fn assert_pairs_whole(r: &PlanResult) {
    for p in &r.parts {
        let mut calls: Vec<String> = Vec::new();
        let mut results: Vec<String> = Vec::new();
        for &i in &r.live[p.lo..p.hi] {
            let e = &r.entries[i];
            match e.kind {
                Kind::Assistant if !e.aux.is_empty() => {
                    calls.extend(serde_json::from_str::<Vec<String>>(&e.aux).unwrap());
                }
                Kind::ToolResult => results.push(serde_json::from_str::<String>(&e.aux).unwrap()),
                _ => {}
            }
        }
        calls.sort();
        results.sort();
        assert_eq!(
            calls, results,
            "part {} splits a call from its result",
            p.idx
        );
    }
}

#[test]
fn an_unanswered_call_followed_by_a_user_message_leaves_a_legal_cut() {
    // T=2000, half=1000. c1 was interrupted (no result is ever recorded) and
    // the user then typed u1 at 1100. With "pending forever" u1 and every
    // later cut were illegal and the 2400-byte chain stayed one live part.
    let mut g = Gen::new();
    g.user(600);
    g.call("c1", 500); // interrupted: never answered
    let u1 = g.user(200);
    g.assistant(1100);
    let fx = Fx::new();
    fx.write(&g);
    let r = fx.plan(2000);
    assert_eq!(r.parts.len(), 2, "{:?}", shapes(&r));
    assert_eq!(r.parts[0].cut, "user");
    assert_eq!(r.parts[0].bytes, 1100);
    assert_eq!(r.parts[1].first_id, jid(&u1));
    assert!(r.parts[0].frozen);
    // a LATER pair after the interrupted call is still kept whole
    let mut g2 = Gen::new();
    g2.user(600);
    g2.call("c1", 500);
    g2.user(200);
    g2.call("c2", 240);
    g2.result("c2", 200);
    g2.assistant(200);
    g2.user(200);
    g2.assistant(800);
    let fx2 = Fx::new();
    fx2.write(&g2);
    let r2 = fx2.plan(2000);
    assert!(r2.parts.len() >= 2, "{:?}", shapes(&r2));
    assert!(
        r2.parts.iter().all(|p| p.cut != "hard"),
        "{:?}",
        shapes(&r2)
    );
    // every recorded result sits in the same part as its call
    for p in &r2.parts {
        let mut calls: Vec<String> = Vec::new();
        let mut results: Vec<String> = Vec::new();
        for &i in &r2.live[p.lo..p.hi] {
            let e = &r2.entries[i];
            match e.kind {
                Kind::Assistant if !e.aux.is_empty() => {
                    calls.extend(serde_json::from_str::<Vec<String>>(&e.aux).unwrap());
                }
                Kind::ToolResult => results.push(serde_json::from_str::<String>(&e.aux).unwrap()),
                _ => {}
            }
        }
        for r in &results {
            assert!(calls.contains(r), "part {} orphans result {r}", p.idx);
        }
    }
}

#[test]
fn pair_heavy_sessions_cut_only_before_users_and_never_split_a_pair() {
    // every turn starts with a user message and holds one or two call/result
    // pairs, so a legal cut always exists within 2*target of any part start
    let mut g = Gen::new();
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let mut calls = 0u64;
    for _ in 0..40 {
        g.user(150 + rng.below(400) as usize);
        for _ in 0..(1 + rng.below(2)) {
            calls += 1;
            let c = format!("k{calls}");
            g.call(&c, 240 + rng.below(300) as usize);
            g.result(&c, 200 + rng.below(500) as usize);
        }
        if rng.below(2) == 0 {
            g.assistant(200 + rng.below(400) as usize);
        }
    }
    let fx = Fx::new();
    fx.write(&g);
    let r = fx.plan(2500);
    assert!(r.parts.len() >= 3, "{}", r.parts.len());
    assert!(r.parts.iter().all(|p| p.cut != "hard"), "{:?}", shapes(&r));
    assert_pairs_whole(&r);
}

#[test]
fn a_compaction_boundary_is_preferred_inside_the_window() {
    // T=2000. Window boundaries: u1@1100, u2@1550 (follows the compaction),
    // u3@1950. The LAST legal one would be u3; the compaction one wins.
    let mut g = Gen::new();
    g.user(600);
    g.assistant(500);
    g.user(200);
    g.assistant(150);
    g.compaction(100);
    let u2 = g.user(200);
    g.assistant(200);
    g.user(200);
    g.assistant(600);
    let fx = Fx::new();
    fx.write(&g);
    let r = fx.plan(2000);
    assert_eq!(r.parts[0].cut, "compaction", "{:?}", shapes(&r));
    assert_eq!(r.parts[1].first_id, jid(&u2));
    assert_eq!(r.parts[0].bytes, 1550);
}

#[test]
fn without_a_compaction_the_last_legal_cut_in_the_window_wins() {
    let mut g = Gen::new();
    g.user(600);
    g.assistant(500);
    g.user(200); // 1100
    g.assistant(200);
    let u2 = g.user(200); // 1500 (after u1 200 + a 200)
    g.assistant(600);
    let fx = Fx::new();
    fx.write(&g);
    let r = fx.plan(2000);
    assert_eq!(r.parts[0].cut, "user");
    assert_eq!(r.parts[1].first_id, jid(&u2));
}

#[test]
fn a_legal_cut_past_the_target_is_taken_within_twice_the_target() {
    // nothing legal in [1000,2000]; u1 at 2200 is within 2T
    let mut g = Gen::new();
    g.user(300);
    g.assistant(1900);
    let u1 = g.user(300);
    g.assistant(300);
    let fx = Fx::new();
    fx.write(&g);
    let r = fx.plan(2000);
    assert_eq!(r.parts[0].cut, "user-late", "{:?}", shapes(&r));
    assert_eq!(r.parts[0].bytes, 2200);
    assert_eq!(r.parts[1].first_id, jid(&u1));
}

#[test]
fn an_autonomous_loop_without_user_boundaries_is_cut_hard_past_twice_the_target() {
    let mut g = Gen::new();
    g.user(150);
    for _ in 0..12 {
        g.assistant(300);
    }
    let fx = Fx::new();
    fx.write(&g);
    let r = fx.plan(1000);
    assert_eq!(r.parts[0].cut, "hard", "{:?}", shapes(&r));
    // sizes 150,450,750,1050: the first entry boundary reaching T is after 4 entries
    assert_eq!(r.parts[0].n_entries, 4);
    assert!(r.parts.len() >= 3);
    assert!(!r.parts.last().unwrap().frozen);
}

#[test]
fn an_undecided_cut_is_not_frozen() {
    // 1550 bytes, no user boundary, T=1000 (2T=2000 not reached): one live part.
    let mut g = Gen::new();
    g.user(150);
    for _ in 0..4 {
        g.assistant(350);
    }
    let fx = Fx::new();
    fx.write(&g);
    let r = fx.plan(1000);
    assert_eq!(r.parts.len(), 1);
    assert!(!r.parts[0].frozen);
}

// --- determinism, incrementality, the property ---------------------------------------

struct Rng(u64);

impl Rng {
    fn step(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.step() % n
    }
}

/// Append one group of records; every group ends on the leaf chain, so the
/// chain after a group is exactly the chain before it plus the group's chain
/// entries (an append-only growth).
fn grow(g: &mut Gen, rng: &mut Rng, calls: &mut u64) {
    if rng.below(5) == 0 {
        g.side_branch(1 + rng.below(3) as usize);
    }
    match rng.below(7) {
        0 | 1 => {
            g.user(150 + rng.below(500) as usize);
        }
        2 | 3 => {
            *calls += 1;
            let c = format!("call{calls}");
            g.call(&c, 240 + rng.below(400) as usize);
            if rng.below(4) == 0 {
                // interrupted: the call is never answered, a user message follows
                g.user(150 + rng.below(200) as usize);
            } else {
                g.result(&c, 200 + rng.below(600) as usize);
            }
        }
        4 => {
            g.compaction(200 + rng.below(300) as usize);
        }
        _ => {
            g.assistant(150 + rng.below(700) as usize);
        }
    }
}

fn append_new_lines(path: &Path, g: &Gen, already: usize) {
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    for l in &g.lines[already..] {
        f.write_all(l.as_bytes()).unwrap();
        f.write_all(b"\n").unwrap();
    }
}

#[test]
fn the_plan_is_deterministic_and_equal_with_or_without_a_checkpoint() {
    let mut g = Gen::new();
    let mut rng = Rng(0x9e3779b97f4a7c15);
    let mut calls = 0;
    for _ in 0..80 {
        grow(&mut g, &mut rng, &mut calls);
    }
    let fx = Fx::new();
    fx.write(&g);
    let a = fx.plan(3000);
    let b = fx.plan(3000); // from the checkpoint the first run wrote
    let c = fx.fresh(3000);
    assert!(a.parts.len() > 3);
    assert_eq!(shapes(&a), shapes(&b));
    assert_eq!(shapes(&a), shapes(&c));
    assert_eq!(a.json["chain_ids"], c.json["chain_ids"]);
    assert_eq!(b.json["checkpoint"]["status"], "valid");
    assert_eq!(b.json["checkpoint"]["scanned_bytes"], 0);
}

#[test]
fn append_only_growth_never_changes_frozen_ranges() {
    for seed in 1..=24u64 {
        let target = [600u64, 1500, 4000][(seed % 3) as usize];
        let mut rng = Rng(seed.wrapping_mul(0x2545F4914F6CDD1D) | 1);
        let mut g = Gen::new();
        let mut calls = 0;
        let fx = Fx::new();
        fx.write(&g);
        let mut written = g.lines.len();
        let mut prev: Vec<Shape> = Vec::new();
        let mut max_frozen = 0;
        for step in 0..45 {
            for _ in 0..(1 + rng.below(3)) {
                grow(&mut g, &mut rng, &mut calls);
            }
            append_new_lines(&fx.src(), &g, written);
            written = g.lines.len();
            let inc = fx.plan(target);
            let fresh = fx.fresh(target);
            let now = shapes(&inc);
            assert_eq!(
                now,
                shapes(&fresh),
                "seed {seed} step {step}: incremental != from scratch"
            );
            assert_eq!(chain_ids(&inc), g.main, "seed {seed} step {step}");
            assert!(now.len() >= prev.len(), "seed {seed} step {step}");
            for (k, p) in prev.iter().enumerate() {
                if p.6 {
                    assert_eq!(&now[k], p, "seed {seed} step {step}: frozen part {k} moved");
                }
            }
            assert!(inc.json["divergence"].is_null(), "seed {seed} step {step}");
            if step > 0 {
                assert_eq!(inc.json["checkpoint"]["status"], "valid");
            }
            max_frozen = max_frozen.max(now.iter().filter(|p| p.6).count());
            prev = now;
        }
        assert!(max_frozen >= 2, "seed {seed}: the test never froze a part");
    }
}

#[test]
fn an_incremental_pass_scans_only_the_appended_bytes() {
    let mut g = Gen::new();
    g.user(300);
    g.assistant(300);
    let fx = Fx::new();
    fx.write(&g);
    let first = fx.plan(1 << 20);
    assert_eq!(first.json["checkpoint"]["status"], "absent");
    let size1 = std::fs::metadata(fx.src()).unwrap().len();
    assert_eq!(first.json["checkpoint"]["scanned_bytes"], size1);
    let before = g.lines.len();
    g.user(300);
    append_new_lines(&fx.src(), &g, before);
    let size2 = std::fs::metadata(fx.src()).unwrap().len();
    let second = fx.plan(1 << 20);
    assert_eq!(second.json["checkpoint"]["status"], "valid");
    assert_eq!(second.json["checkpoint"]["scanned_bytes"], size2 - size1);
}

#[test]
fn an_unterminated_final_line_is_planned_but_never_persisted() {
    let mut g = Gen::new();
    g.user(300);
    let a = g.assistant(300);
    let mut text = g.text();
    text.pop(); // drop the final newline: jq would still read the line
    let fx = Fx::new();
    std::fs::write(fx.src(), &text).unwrap();
    let r = fx.plan(1 << 20);
    assert_eq!(chain_ids(&r).last(), Some(&a));
    assert_eq!(r.json["source"]["lines_indexed"], 3); // header, title, user (a is volatile)
                                                      // once the newline arrives the same line is indexed
    std::fs::write(fx.src(), g.text()).unwrap();
    let r2 = fx.plan(1 << 20);
    assert_eq!(chain_ids(&r2).last(), Some(&a));
    assert_eq!(r2.json["source"]["lines_indexed"], 4);
}

// --- divergence ---------------------------------------------------------------------

fn long_session(n: usize, seed: u64) -> Gen {
    let mut g = Gen::new();
    let mut rng = Rng(seed);
    let mut calls = 0;
    for _ in 0..n {
        grow(&mut g, &mut rng, &mut calls);
    }
    g
}

#[test]
fn a_rewind_behind_a_frozen_boundary_is_reported_and_replanned() {
    let g = long_session(120, 77);
    let fx = Fx::new();
    fx.write(&g);
    let old = fx.plan(2500);
    let old_shapes = shapes(&old);
    assert!(old.parts.len() >= 5, "{}", old.parts.len());
    // branch from the LAST entry of frozen part 2: a new leaf whose chain
    // leaves everything after part 2 behind
    let rewind_parent = old.json["parts"][1]["last_id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut g2 = Gen {
        lines: g.lines.clone(),
        next: g.next,
        leaf: Some(rewind_parent),
        main: Vec::new(),
    };
    g2.user(250);
    let before = g.lines.len();
    append_new_lines(&fx.src(), &g2, before);
    let r = fx.plan(2500);
    assert_eq!(r.json["checkpoint"]["status"], "valid");
    let fresh = fx.fresh(2500);
    assert_eq!(shapes(&r), shapes(&fresh));
    // expected first mismatch: the first stored FROZEN part that differs now
    let now = shapes(&r);
    let expected = old_shapes
        .iter()
        .enumerate()
        .filter(|(_, p)| p.6)
        .find(|(k, p)| now.get(*k) != Some(*p))
        .map(|(k, _)| k as u64 + 1)
        .expect("the rewind must invalidate a frozen part");
    assert_eq!(r.json["divergence"]["first_mismatch_part"], expected);
    // everything from the mismatch on is re-planned, everything before reused
    for p in &r.parts {
        assert_eq!(p.reused, p.idx < expected && p.frozen, "part {}", p.idx);
    }
    let orphans: Vec<u64> = r.json["orphans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect();
    let want: Vec<u64> = ((r.parts.len() as u64 + 1)..=old.parts.len() as u64).collect();
    assert_eq!(orphans, want);
    assert!(!orphans.is_empty());
    // a second pass is quiet: the checkpoint now agrees with the plan
    let again = fx.plan(2500);
    assert!(again.json["divergence"].is_null());
    assert!(again.json["orphans"].as_array().unwrap().is_empty());
}

#[test]
fn clear_invalidates_every_part_and_orphans_the_rest() {
    let mut g = long_session(100, 5);
    let fx = Fx::new();
    fx.write(&g);
    let old = fx.plan(2500);
    assert!(old.parts.len() >= 4);
    let before = g.lines.len();
    g.reset();
    g.main.clear();
    let u = g.user(300);
    append_new_lines(&fx.src(), &g, before);
    let r = fx.plan(2500);
    assert_eq!(chain_ids(&r), vec![u]);
    assert_eq!(r.parts.len(), 1);
    assert_eq!(r.json["divergence"]["first_mismatch_part"], 1);
    let orphans: Vec<u64> = r.json["orphans"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect();
    assert_eq!(orphans, (2..=old.parts.len() as u64).collect::<Vec<_>>());
}

#[test]
fn a_changed_target_reports_a_divergence_instead_of_silently_moving_cuts() {
    let g = long_session(100, 11);
    let fx = Fx::new();
    fx.write(&g);
    let a = fx.plan(2500);
    assert!(a.parts.len() >= 3);
    let b = fx.plan(4000);
    assert_eq!(b.json["checkpoint"]["status"], "valid"); // the index is reusable
    assert_eq!(b.json["divergence"]["reason"], "target_bytes changed");
}

// --- checkpoint validation -------------------------------------------------------------

fn big_session() -> Gen {
    long_session(150, 3)
}

fn state_text(fx: &Fx) -> Vec<u8> {
    std::fs::read(fx.state()).unwrap()
}

fn assert_rebuilt(fx: &Fx, why: &str, target: u64) {
    let r = plan_file(&fx.src(), &fx.state(), &opts(target)).unwrap();
    assert_eq!(r.json["checkpoint"]["status"], "rebuilt", "{why}");
    let reason = r.json["checkpoint"]["reason"].as_str().unwrap_or("");
    assert!(reason.contains(why), "reason {reason:?} lacks {why:?}");
    assert_eq!(
        shapes(&r),
        shapes(&fx.fresh(target)),
        "a rebuild must equal a from-scratch plan ({why})"
    );
    // the rebuild rewrote a valid checkpoint
    let after = plan_file(&fx.src(), &fx.state(), &opts(target)).unwrap();
    assert_eq!(after.json["checkpoint"]["status"], "valid", "{why}");
}

#[test]
fn tampered_checkpoints_rebuild_instead_of_being_trusted() {
    let target = 3000;
    let g = big_session();

    // adapter_ver changed
    let fx = Fx::new();
    fx.write(&g);
    fx.plan(target);
    let mut o = opts(target);
    o.adapter_ver = "t2".to_string();
    let r = plan_file(&fx.src(), &fx.state(), &o).unwrap();
    assert_eq!(r.json["checkpoint"]["status"], "rebuilt");
    assert!(r.json["checkpoint"]["reason"]
        .as_str()
        .unwrap()
        .contains("adapter_ver"));

    // the source shrank below indexed_bytes
    let fx = Fx::new();
    fx.write(&g);
    fx.plan(target);
    let size = std::fs::metadata(fx.src()).unwrap().len();
    std::fs::OpenOptions::new()
        .write(true)
        .open(fx.src())
        .unwrap()
        .set_len(size / 2)
        .unwrap();
    assert_rebuilt(&fx, "smaller than indexed_bytes", target);

    // an in-place rewrite inside the 4 KiB window before indexed_bytes
    let fx = Fx::new();
    fx.write(&g);
    fx.plan(target);
    let mut bytes = std::fs::read(fx.src()).unwrap();
    let at = bytes.len() - 100;
    let at = bytes[..at].iter().rposition(|&b| b == b'x').unwrap();
    bytes[at] = b'y';
    std::fs::write(fx.src(), &bytes).unwrap();
    assert_rebuilt(&fx, "window hash", target);

    // the header line changed (outside the window: the session is far larger)
    let fx = Fx::new();
    fx.write(&g);
    fx.plan(target);
    let mut bytes = std::fs::read(fx.src()).unwrap();
    assert!(bytes.len() > 20_000);
    let at = bytes.windows(4).position(|w| w == b"\"/c\"").unwrap();
    bytes[at + 2] = b'd';
    std::fs::write(fx.src(), &bytes).unwrap();
    assert_rebuilt(&fx, "header line", target);

    // the index body was edited
    let fx = Fx::new();
    fx.write(&g);
    fx.plan(target);
    let mut st = state_text(&fx);
    st[0] = if st[0] == b'9' { b'8' } else { b'9' };
    std::fs::write(fx.state(), &st).unwrap();
    assert_rebuilt(&fx, "checksum", target);

    // the trailer is cut off
    let fx = Fx::new();
    fx.write(&g);
    fx.plan(target);
    let st = state_text(&fx);
    std::fs::write(fx.state(), &st[..st.len() - 7]).unwrap();
    assert_rebuilt(&fx, "state", target);

    // garbage
    let fx = Fx::new();
    fx.write(&g);
    fx.plan(target);
    std::fs::write(fx.state(), b"not a checkpoint\n").unwrap();
    assert_rebuilt(&fx, "state", target);
}

#[test]
fn a_rebuild_reports_reuse_of_frozen_parts_that_still_match() {
    let g = big_session();
    let fx = Fx::new();
    fx.write(&g);
    let a = fx.plan(3000);
    let frozen = a.parts.iter().filter(|p| p.frozen).count();
    assert!(frozen >= 2);
    let mut o = opts(3000);
    o.adapter_ver = "t2".to_string();
    let b = plan_file(&fx.src(), &fx.state(), &o).unwrap();
    assert_eq!(b.json["checkpoint"]["status"], "rebuilt");
    assert_eq!(b.parts.iter().filter(|p| p.reused).count(), frozen);
    assert!(b.json["divergence"].is_null());
}

// --- emit -------------------------------------------------------------------------------

#[test]
fn emit_streams_the_header_title_and_exactly_the_parts_chain_lines() {
    let g = long_session(60, 9);
    let fx = Fx::new();
    fx.write(&g);
    let r = fx.plan(2500);
    assert!(r.parts.len() >= 3);
    let by_id: std::collections::HashMap<String, String> = g
        .lines
        .iter()
        .filter_map(|l| {
            let v: Value = serde_json::from_str(l).ok()?;
            Some((v["id"].as_str()?.to_string(), l.clone()))
        })
        .collect();
    let mut all_chain_lines: Vec<String> = Vec::new();
    for idx in 1..=r.parts.len() {
        let mut out: Vec<u8> = Vec::new();
        emit_part(&fx.src(), &r, idx, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], g.lines[0], "header first");
        assert_eq!(lines[1], g.lines[1], "title slot second");
        let ids: Vec<String> = r.live[r.parts[idx - 1].lo..r.parts[idx - 1].hi]
            .iter()
            .map(|&i| serde_json::from_str::<String>(r.entries[i].id.as_deref().unwrap()).unwrap())
            .collect();
        let want: Vec<&str> = ids.iter().map(|i| by_id[i].as_str()).collect();
        assert_eq!(&lines[2..], &want[..], "part {idx}");
        all_chain_lines.extend(lines[2..].iter().map(|s| s.to_string()));
    }
    let want_all: Vec<String> = g.main.iter().map(|i| by_id[i].clone()).collect();
    assert_eq!(all_chain_lines, want_all, "parts concatenate to the chain");
    let mut sink: Vec<u8> = Vec::new();
    assert!(emit_part(&fx.src(), &r, r.parts.len() + 1, &mut sink).is_err());
    assert!(emit_part(&fx.src(), &r, 0, &mut sink).is_err());
}

// --- equivalence with the adapter's jq TRANSLATE ---------------------------------------------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn adapter_script() -> String {
    std::fs::read_to_string(repo_root().join("plugins/kb-memory/hooks/kb-capture-omp.sh"))
        .expect("kb-capture-omp.sh")
}

/// The adapter's TRANSLATE program, exactly as the shell script embeds it.
fn translate_program() -> String {
    let sh = adapter_script();
    let a = sh.find("TRANSLATE='").expect("TRANSLATE=") + "TRANSLATE='".len();
    let b = a + sh[a..].find("\n'\n").expect("end of TRANSLATE");
    sh[a..b].replace("'\"'\"'", "'")
}

/// The chain-resolution slice of TRANSLATE, closed to print the live chain's
/// ids and `$dmodel`. Because it is cut out of the shipped program, a change
/// to the adapter's chain rules changes this test's oracle.
fn chain_program() -> String {
    let p = translate_program();
    let s0 = p
        .find("map(select(.type != \"title\")) as $es |")
        .expect("chain start");
    let marker =
        "($live | map(select(.type == \"model_change\") | .model) | last // \"omp\") as $dmodel |";
    let m = p.find(marker).expect("dmodel line") + marker.len();
    format!(
        "{} {{ids: ($live | map(.id // null)), dmodel: $dmodel}} end",
        &p[s0..m]
    )
}

fn jq_available() -> bool {
    Command::new("jq")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `jq -R -c 'fromjson? // empty'` - the adapter's own input cleaning.
fn jq_clean(file: &Path) -> Vec<u8> {
    let o = Command::new("jq")
        .args(["-R", "-c", "fromjson? // empty"])
        .arg(file)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    o.stdout
}

fn jq_slurp(program: &str, cleaned: &[u8], file_arg: &str) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("clean.jsonl");
    std::fs::write(&p, cleaned).unwrap();
    let o = Command::new("jq")
        .args(["-c", "-s", "--arg", "file", file_arg, program])
        .arg(&p)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    o.stdout
}

fn omp_fixtures() -> Vec<PathBuf> {
    let dir = repo_root().join("plugins/kb-memory/hooks/tests/fixtures");
    let mut v: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let n = p.file_name().unwrap().to_string_lossy().into_owned();
            n.starts_with("omp-") && n.ends_with(".jsonl")
        })
        .collect();
    v.sort();
    v
}

/// What the planner says about a fixture, in the oracle's shape.
fn planner_view(fixture: &Path) -> Value {
    let dir = tempfile::tempdir().unwrap();
    let o = Opts {
        target_bytes: DEFAULT_TARGET_BYTES,
        adapter_ver: String::new(),
        print_chain: true,
        write_state: false,
    };
    let r = plan_file(fixture, &dir.path().join("s"), &o).unwrap();
    if r.live.is_empty() && r.entries.iter().all(|e| e.kind == Kind::Title) {
        return Value::Null;
    }
    json!({"ids": r.json["chain_ids"], "dmodel": r.json["dmodel"]})
}

#[test]
fn chain_matches_the_committed_jq_goldens_on_every_omp_fixture() {
    // The goldens were generated locally by running the adapter's own chain
    // slice with jq over `fromjson? // empty`-cleaned fixtures.
    let g: Value = serde_json::from_str(
        &std::fs::read_to_string(
            repo_root().join("plugins/kb-memory/hooks/tests/fixtures/omp-chain-goldens.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let fixtures = omp_fixtures();
    assert!(fixtures.len() >= 9);
    for f in &fixtures {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        assert!(g.get(&name).is_some(), "no golden for {name}");
        assert_eq!(planner_view(f), g[&name], "{name}");
    }
}

#[test]
fn chain_matches_what_jq_selects_on_every_omp_fixture() {
    if !jq_available() {
        eprintln!("jq not installed: relying on the committed goldens");
        return;
    }
    let prog = chain_program();
    for f in omp_fixtures() {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        let out = jq_slurp(&prog, &jq_clean(&f), "x");
        let oracle: Value = if out.iter().all(|b| b.is_ascii_whitespace()) {
            Value::Null
        } else {
            serde_json::from_slice(&out).unwrap()
        };
        assert_eq!(planner_view(&f), oracle, "{name}");
    }
}

#[test]
fn chain_matches_jq_on_synthetic_sessions_with_branches_and_resets() {
    if !jq_available() {
        return;
    }
    let prog = chain_program();
    for seed in [1u64, 2, 3, 4] {
        let mut g = long_session(60, seed);
        if seed % 2 == 0 {
            g.reset();
            g.main.clear();
            g.user(200);
            g.side_branch(2);
            g.assistant(200);
        }
        let fx = Fx::new();
        fx.write(&g);
        let oracle: Value =
            serde_json::from_slice(&jq_slurp(&prog, &jq_clean(&fx.src()), "x")).unwrap();
        assert_eq!(planner_view(&fx.src()), oracle, "seed {seed}");
        assert_eq!(oracle["ids"], json!(g.main), "seed {seed}");
    }
}

#[test]
fn translating_a_single_part_emit_equals_translating_the_whole_file() {
    if !jq_available() {
        return;
    }
    let prog = translate_program();
    let mut files = omp_fixtures();
    let fx = Fx::new();
    let g = long_session(40, 21);
    fx.write(&g);
    files.push(fx.src());
    for f in files {
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        let sdir = tempfile::tempdir().unwrap();
        let o = Opts {
            target_bytes: DEFAULT_TARGET_BYTES,
            adapter_ver: String::new(),
            print_chain: false,
            write_state: false,
        };
        let r = plan_file(&f, &sdir.path().join("s"), &o).unwrap();
        let whole = jq_slurp(&prog, &jq_clean(&f), "F");
        if r.parts.is_empty() {
            // nothing on the chain: the legacy program emits at most the meta line
            assert!(whole.len() < 2048, "{name}");
            continue;
        }
        assert_eq!(r.parts.len(), 1, "{name}");
        let mut emitted: Vec<u8> = Vec::new();
        emit_part(&f, &r, 1, &mut emitted).unwrap();
        let part = jq_slurp(&prog, &jq_clean_bytes(&emitted), "F");
        assert_eq!(
            String::from_utf8_lossy(&part),
            String::from_utf8_lossy(&whole),
            "{name}"
        );
    }
}

fn jq_clean_bytes(bytes: &[u8]) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("in.jsonl");
    std::fs::write(&p, bytes).unwrap();
    jq_clean(&p)
}

#[test]
fn concatenated_part_translations_equal_the_full_translation() {
    if !jq_available() {
        return;
    }
    // Single-model, no compaction/exit records: the only per-part differences
    // the adapter must carry ($dmodel, the exit record) cannot arise.
    let mut g = Gen::new();
    let mut rng = Rng(0xdead_beef_1234_5671);
    let mut calls = 0u64;
    for _ in 0..70 {
        match rng.below(4) {
            0 => {
                g.user(150 + rng.below(400) as usize);
            }
            1 | 2 => {
                calls += 1;
                let c = format!("k{calls}");
                g.call(&c, 240 + rng.below(300) as usize);
                g.result(&c, 200 + rng.below(500) as usize);
            }
            _ => {
                g.assistant(150 + rng.below(500) as usize);
            }
        }
    }
    let fx = Fx::new();
    fx.write(&g);
    let r = fx.plan(2500);
    assert!(r.parts.len() >= 3, "{}", r.parts.len());
    let prog = translate_program();
    let strip_meta = |bytes: Vec<u8>| -> Vec<String> {
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .filter(|l| !l.contains("\"type\":\"adapter-meta\""))
            .map(str::to_string)
            .collect()
    };
    let whole = strip_meta(jq_slurp(&prog, &jq_clean(&fx.src()), "F"));
    let mut joined: Vec<String> = Vec::new();
    for idx in 1..=r.parts.len() {
        let mut emitted: Vec<u8> = Vec::new();
        emit_part(&fx.src(), &r, idx, &mut emitted).unwrap();
        joined.extend(strip_meta(jq_slurp(&prog, &jq_clean_bytes(&emitted), "F")));
    }
    assert!(!whole.is_empty());
    assert_eq!(joined, whole);
}

// --- user-message parity with TRANSLATE (v0.46 SEG-PR1 polish) -----------------------------

/// `content` shapes whose TRANSLATE verdict (jq `join("\n") != ""`) is NOT
/// "an array with a non-empty text string": the planner must agree on each.
fn user_content_variants() -> Vec<(Value, bool)> {
    vec![
        (json!([{"type": "text", "text": "hello"}]), true),
        (json!([{"type": "text", "text": ""}]), false),
        (json!([{"type": "text"}]), false),
        (json!([{"type": "text", "text": null}]), false),
        // two empty parts join to "\n": TRANSLATE emits the record
        (
            json!([{"type": "text", "text": ""}, {"type": "text", "text": ""}]),
            true,
        ),
        (
            json!([{"type": "text", "text": null}, {"type": "text"}]),
            true,
        ),
        (
            json!([{"type": "text", "text": ""}, {"type": "image"}]),
            false,
        ),
        (json!([{"type": "text", "text": false}]), true),
        (json!([{"type": "text", "text": {}}]), true),
        (json!({"a": {"type": "text", "text": "x"}}), true),
        (json!("plain string"), false),
        (json!([]), false),
    ]
}

#[test]
fn user_classification_matches_translate_on_empty_text_part_variants() {
    for (content, expect) in user_content_variants() {
        let rec = json!({"type": "message", "id": "u", "parentId": null,
            "message": {"role": "user", "content": content}});
        let (kind, ..) = classify(&rec).unwrap();
        assert_eq!(kind == Kind::User, expect, "{content}");
    }
}

#[test]
fn user_classification_matches_the_translate_program_itself() {
    if !jq_available() {
        return;
    }
    // One session whose chain holds every variant; TRANSLATE emits a
    // `type:"user"` text record for exactly the planner's `Kind::User` ones.
    let mut g = Gen::new();
    let variants = user_content_variants();
    for (content, _) in &variants {
        let content = content.clone();
        g.chain(
            &move |_| json!({"type": "message", "message": {"role": "user", "content": content.clone()}}),
            400,
        );
    }
    let fx = Fx::new();
    fx.write(&g);
    let r = fx.plan(1 << 20);
    let planner_users: Vec<bool> = r
        .live
        .iter()
        .map(|&i| r.entries[i].kind == Kind::User)
        .collect();
    let out = jq_slurp(&translate_program(), &jq_clean(&fx.src()), "F");
    let emitted: Vec<Value> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|v| v["type"] == "user")
        .collect();
    assert_eq!(
        emitted.len(),
        planner_users.iter().filter(|b| **b).count(),
        "TRANSLATE and the planner disagree on which user messages exist"
    );
    assert_eq!(
        planner_users,
        variants.iter().map(|(_, e)| *e).collect::<Vec<_>>()
    );
}

// --- per-part dmodel ------------------------------------------------------------------------

#[test]
fn each_part_carries_the_model_in_force_at_its_start_not_the_final_one() {
    // T=2000. user600+asst400+modelA100 = 1100, then u1 (legal cut), then a
    // later model change C inside part 2.
    let mut g = Gen::new();
    g.user(600);
    g.assistant(400);
    g.chain(&|_| json!({"type": "model_change", "model": "prov/a"}), 100);
    let u1 = g.user(200);
    g.chain(&|_| json!({"type": "model_change", "model": "prov/c"}), 100);
    g.assistant(900);
    let fx = Fx::new();
    fx.write(&g);
    let r = fx.plan(2000);
    assert_eq!(r.parts.len(), 2, "{:?}", shapes(&r));
    assert_eq!(r.parts[1].first_id, jid(&u1));
    assert_eq!(r.json["dmodel"], "prov/c", "the document value is the LAST");
    let pj = &r.json["parts"];
    assert_eq!(
        pj[0]["dmodel"], "omp",
        "no model change yet at part 1's start"
    );
    assert_eq!(
        pj[1]["dmodel"], "prov/a",
        "model C comes after part 2's start"
    );
    // appending a further model change moves the document value only
    let before = r.json["parts"][0]["dmodel"].clone();
    let n = g.lines.len();
    g.chain(&|_| json!({"type": "model_change", "model": "prov/d"}), 100);
    append_new_lines(&fx.src(), &g, n);
    let r2 = fx.plan(2000);
    assert_eq!(r2.json["dmodel"], "prov/d");
    assert_eq!(r2.json["parts"][0]["dmodel"], before);
    assert_eq!(r2.json["parts"][1]["dmodel"], "prov/a");
    assert_eq!(r2.parts[0].dmodel, json!("omp"));
}

#[test]
fn a_falsy_last_model_at_a_part_start_falls_back_to_omp_like_jq() {
    let mut g = Gen::new();
    g.chain(&|_| json!({"type": "model_change", "model": "prov/a"}), 100);
    g.user(600);
    g.assistant(400);
    g.chain(&|_| json!({"type": "model_change", "model": null}), 100);
    g.user(200);
    g.assistant(1200);
    let fx = Fx::new();
    fx.write(&g);
    let r = fx.plan(2000);
    assert_eq!(r.parts.len(), 2, "{:?}", shapes(&r));
    // part 1 starts AT the first model change (inclusive); part 2's last model
    // change before its start is null, so jq's `last // "omp"` gives "omp" -
    // the earlier prov/a is not consulted.
    assert_eq!(r.parts[0].dmodel, json!("prov/a"));
    assert_eq!(r.parts[1].dmodel, json!("omp"));
}
