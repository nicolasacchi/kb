//! Golden tests for the `session-view/1`-migrated renderer (sessions-rethink
//! W1). Every test that legitimately changed behavior from the pre-IR
//! renderer says so in its doc comment — nothing here is a silent
//! regression. Coverage target: ≥32 (the pre-migration file's count),
//! deliberately exceeded.

use super::*;

fn wrap(jsonl: &str) -> String {
    format!(
        r#"<html><head><meta name="kb-category" content="memory-session"><meta name="kb-session" content="abc"><title>T</title></head><body><pre>{}</pre></body></html>"#,
        jsonl
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    )
}

fn render(jsonl: &str) -> String {
    render_if_session(&wrap(jsonl), "sessions").expect("renders")
}

// ─── detection ───────────────────────────────────────────────────────────

#[test]
fn detects_session_transcript() {
    let html = r#"<html><head><meta name="kb-category" content="memory-session"></head><body></body></html>"#;
    assert!(is_session_transcript(html));
    let not_session = r#"<html><head><meta name="kb-category" content="notes"></head></html>"#;
    assert!(!is_session_transcript(not_session));
}

#[test]
fn doc_about_sessions_is_not_a_transcript() {
    let html = r#"<html><head><title>plan</title>
<meta name="kb-category" content="research">
<meta name="kb-tags" content="sessions, plan"></head>
<body><p>captures carry <code>kb-category</code> = memory-session</p>
<pre>W0 -&gt; W7 pipeline diagram, not JSONL</pre></body></html>"#;
    assert!(!is_session_transcript(html));
    assert!(render_if_session(html, "kb-docs").is_none());
}

#[test]
fn returns_none_for_non_session() {
    let html =
        r#"<html><head><meta name="kb-category" content="notes"></head><body></body></html>"#;
    assert!(render_if_session(html, "sessions").is_none());
}

// ─── renderer versioning + basic shell ────────────────────────────────────

#[test]
fn renderer_stamps_its_grammar_version() {
    let rendered = render(
        r#"{"type":"user","timestamp":"2026-05-27T08:00:00Z","message":{"role":"user","content":"hi"}}"#,
    );
    assert!(rendered.contains(r#"<meta name="kb-renderer" content="session-view/1">"#));
}

#[test]
fn renders_filter_toolbar_with_debug_pill_and_runtime() {
    let rendered = render_if_session(&wrap(""), "platform").expect("renders");
    assert!(rendered.contains(r#"<body data-kb="platform">"#));
    assert!(rendered.contains(r#"data-filter="user""#));
    assert!(rendered.contains(r#"data-filter="thinking""#));
    // P13/hygiene bundle — the NEW debug pill (pre-IR always showed
    // .ses-debug; it's filterable now).
    assert!(rendered.contains(r#"data-filter="debug""#));
    assert!(rendered.contains(r#"data-search"#));
    assert!(rendered.contains(r#"data-action="expand-all""#));
    assert!(rendered.contains("STORAGE_KEY"));
    assert!(rendered.contains(r#""debug""#)); // FILTER_FLAGS includes it
}

#[test]
fn toolbar_includes_all_none_buttons() {
    let rendered = render_if_session(&wrap(""), "sessions").expect("renders");
    assert!(rendered.contains(r#"data-action="filter-all""#));
    assert!(rendered.contains(r#"data-action="filter-none""#));
    assert!(rendered.contains(r#"title="shift-click to solo""#));
}

// ─── turns: rendering + identity (R2) ─────────────────────────────────────

#[test]
fn renders_minimal_user_turn_with_stable_turn_id() {
    let rendered = render(
        r#"{"type":"user","timestamp":"2026-05-27T08:46:05.507Z","uuid":"aaaaaaaa-bbbb-4000-8000-000000000001","message":{"role":"user","content":"hello world"}}"#,
    );
    assert!(rendered.contains("ses-turn--user"));
    assert!(rendered.contains("hello world"));
    assert!(rendered.contains("08:46:05"));
    // R2 — t-<uuid12> stable id, data-turn carries the ordinal.
    assert!(rendered.contains(r#"id="t-aaaaaaaabbbb""#), "{rendered}");
    assert!(rendered.contains(r#"data-turn="1""#));
    // The old #turn-N anchor form is GONE (accepted break, R2/D3).
    assert!(!rendered.contains(r#"id="turn-1""#));
}

#[test]
fn renders_tool_use_with_headline() {
    let rendered = render(
        r#"{"type":"assistant","timestamp":"2026-05-27T08:46:05.507Z","message":{"role":"assistant","model":"claude-opus-4-7","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls -la /tmp"}}]}}"#,
    );
    assert!(rendered.contains("ses-tool--shell"));
    assert!(rendered.contains("Bash"));
    assert!(rendered.contains("ls -la /tmp"));
}

#[test]
fn malformed_lines_become_raw_fragments_the_tolerance_floor() {
    let html = wrap(
        "not json at all\n{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}",
    );
    let rendered = render_if_session(&html, "sessions").expect("renders");
    assert!(rendered.contains("ses-raw"));
    assert!(rendered.contains("hi"));
}

#[test]
fn html_unescape_roundtrip() {
    assert_eq!(
        html_unescape("&lt;a href=&quot;x&quot;&gt;&amp;"),
        "<a href=\"x\">&"
    );
}

/// Entity-order pin for the display-only 5-entity unescape (mirrors
/// sessions.rs's 3-entity pin): `&amp;lt;` stays `&lt;`, not `<`.
#[test]
fn html_unescape_preserves_amp_then_entity_order() {
    assert_eq!(html_unescape("&amp;lt;"), "&lt;");
    assert_eq!(html_unescape("&amp;amp;"), "&amp;");
    assert_eq!(html_unescape("&amp;quot;"), "&quot;");
    // One pass, no re-decode: `&amp;gt;` yields the literal `&gt;`.
    assert_eq!(html_unescape("&lt;&amp;gt;&#39;"), "<&gt;'");
}

#[test]
fn esc_is_single_pass_amp_first() {
    assert_eq!(esc("a < b & c >"), "a &lt; b &amp; c &gt;");
    assert_eq!(esc("&lt;"), "&amp;lt;");
    assert_eq!(html_unescape(&esc("x < y & z >")), "x < y & z >");
}

// ─── in-corpus file-path links ─────────────────────────────────────────────

#[test]
fn file_path_link_resolves_in_corpus_and_stays_plain_out_of_corpus() {
    let _g = crate::sessions::MOUNT_TEST_GUARD
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let read = |file: &str| {
        let jsonl = format!(
            r#"{{"type":"assistant","timestamp":"2026-05-27T08:00:00Z","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"t1","name":"Read","input":{{"file_path":"{file}"}}}}]}}}}"#
        );
        render_if_session(&wrap(&jsonl), "platform").expect("renders")
    };

    crate::sessions::set_corpus_mounts(vec![]);
    let out = read("/home/user/project/kb/CLAUDE.md");
    assert!(
        !out.contains(r##"<a class="ses-path" href="#" data-kb="##),
        "no link out-of-corpus"
    );
    assert!(out.contains("ses-path--plain"));
    assert!(out.contains("CLAUDE.md"));

    let tmp = std::env::temp_dir().join(format!("kbtest-render-v1-{}", std::process::id()));
    let root = tmp.join("docs");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("guide.html"), b"x").unwrap();
    let file = root.join("guide.html");
    crate::sessions::set_corpus_mounts(vec![crate::sessions::CorpusMount {
        kb: "docs".into(),
        source_root: root.clone(),
    }]);
    let inc = read(&file.to_string_lossy());
    assert!(inc.contains(r#"data-kb="docs""#));
    assert!(inc.contains(r#"data-rel="guide.html""#));

    crate::sessions::set_corpus_mounts(vec![]);
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn bash_arg_is_never_linkified() {
    let rendered = render(
        r#"{"type":"assistant","timestamp":"2026-05-27T08:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"ls /tmp/foo.html"}}]}}"#,
    );
    assert!(!rendered.contains(r#"data-rel="/tmp/foo.html""#));
    assert!(!rendered.contains(r#"data-path="/tmp/foo.html""#));
}

// ─── tool results + errors ─────────────────────────────────────────────────

#[test]
fn error_tool_result_emits_callout_before_details() {
    let jsonl = [
        r#"{"type":"assistant","timestamp":"2026-05-27T08:00:00Z","uuid":"11111111-0000-4000-8000-000000000001","requestId":"r1","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"false"}}]}}"#,
        r#"{"type":"user","timestamp":"2026-05-27T08:00:05Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"Permission denied"}]}}"#,
    ].join("\n");
    let rendered = render(&jsonl);
    assert!(rendered.contains(r#"<aside class="ses-error-callout">"#));
    assert!(rendered.contains("Permission denied"));
    let cb = rendered
        .find(r#"<aside class="ses-error-callout">"#)
        .unwrap();
    let cd = rendered
        .find(r#"<details class="ses-result ses-result--err">"#)
        .unwrap();
    assert!(cb < cd, "callout should be before the details");
}

#[test]
fn unpaired_tool_call_shows_no_result_captured_chip() {
    let jsonl = r#"{"type":"assistant","timestamp":"2026-05-27T08:00:00Z","uuid":"11111111-0000-4000-8000-000000000001","requestId":"r1","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"echo hi"}}]}}"#;
    let rendered = render(jsonl);
    assert!(rendered.contains("no result captured"));
}

#[test]
fn tool_input_renders_as_valid_reparseable_json() {
    // v2 CHANGE (reader.md Proposal 6): the pre-IR renderer's `pretty_input`
    // was display-only (multiline strings broke into real newlines, NOT
    // valid JSON). The IR's InputView::Small uses
    // `serde_json::to_string_pretty` — always valid, re-parseable — so the
    // raw view is genuinely the raw payload, not a lossy display transform.
    let jsonl = r#"{"type":"assistant","timestamp":"2026-05-27T08:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Edit","input":{"file_path":"x.rs","old_string":"a","new_string":"b"}}]}}"#;
    let rendered = render(jsonl);
    let start =
        rendered.find(r#"class="ses-tool__input">"#).unwrap() + r#"class="ses-tool__input">"#.len();
    let end = rendered[start..].find("</pre>").unwrap() + start;
    let inner = html_unescape(&rendered[start..end]).replace("&#39;", "'"); // esc() doesn't touch quotes; fine either way
    assert!(
        serde_json::from_str::<serde_json::Value>(&inner).is_ok(),
        "{inner}"
    );
}

// ─── time gaps ──────────────────────────────────────────────────────────

#[test]
fn time_gap_label_thresholds() {
    assert_eq!(
        time_gap_label("2026-05-27T08:00:00Z", "2026-05-27T08:00:20Z"),
        None
    );
    // v2 CHANGE: the pre-IR renderer had a bespoke "<90s stays seconds"
    // bucket in its own `time_gap_label`. This wave consolidates on ONE
    // `duration_human_secs` formatter shared by time-gaps AND the header's
    // active-time/span figures (R6) — its bucket boundary is a flat 60s, so
    // exactly-60s now reads "1m 0s" rather than "60s". Documented, not a
    // silent regression.
    assert_eq!(
        time_gap_label("2026-05-27T08:00:00Z", "2026-05-27T08:01:00Z"),
        Some("1m 0s later".into())
    );
    assert_eq!(
        time_gap_label("2026-05-27T08:00:00Z", "2026-05-27T11:00:00Z"),
        Some("3h 0m later".into())
    );
}

#[test]
fn time_gap_chip_inserted_between_distant_turns() {
    let jsonl = [
        r#"{"type":"user","timestamp":"2026-05-27T08:00:00Z","message":{"role":"user","content":"first"}}"#,
        r#"{"type":"user","timestamp":"2026-05-27T08:05:00Z","message":{"role":"user","content":"second"}}"#,
    ].join("\n");
    let rendered = render(&jsonl);
    assert!(rendered.contains(r#"<aside class="ses-time-gap">"#));
    assert!(rendered.contains("5m 0s later"));
}

// ─── tokens ──────────────────────────────────────────────────────────────

#[test]
fn human_tokens_buckets() {
    assert_eq!(human_tokens(0), "0");
    assert_eq!(human_tokens(999), "999");
    assert_eq!(human_tokens(1_000), "1.0k");
    assert_eq!(human_tokens(2_456), "2.5k");
    assert_eq!(human_tokens(1_234_000), "1.2M");
}

#[test]
fn token_line_relabels_effective_input_r6() {
    let jsonl = r#"{"type":"assistant","timestamp":"2026-05-27T08:00:00Z","message":{"role":"assistant","model":"opus","content":[{"type":"text","text":"a"}],"usage":{"input_tokens":1200,"output_tokens":340,"cache_creation_input_tokens":500,"cache_read_input_tokens":10000}}}"#;
    let rendered = render(jsonl);
    assert!(rendered.contains("effective in"));
    // effective_input = input(1200) + cache_read(10000) = 11200 -> "11k"
    assert!(rendered.contains("11k effective in"), "{rendered}");
}

// ─── AskUserQuestion / plan / permission decisions ─────────────────────────

#[test]
fn structured_answers_promoted_to_decision_card_with_raw_toggle() {
    let jsonl = [
        r#"{"type":"assistant","timestamp":"2026-05-27T08:00:00Z","uuid":"11111111-0000-4000-8000-000000000001","requestId":"r1","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"AskUserQuestion","input":{}}]}}"#,
        r#"{"type":"user","timestamp":"2026-05-27T08:00:10Z","toolUseResult":{"answers":{"Pick one":"Option B","Pick many":"X, Y"},"questions":[{"question":"Pick one"},{"question":"Pick many"}]},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"Your questions have been answered: \"Pick one\"=\"Option B\" selected preview:\nsome preview, \"Pick many\"=\"X, Y\"."}]}}"#,
    ].join("\n");
    let rendered = render(&jsonl);
    assert!(rendered.contains(r#"ses-decision--answered"#));
    assert!(rendered.contains("Pick one"));
    assert!(rendered.contains("Option B"));
    assert!(rendered.contains("Pick many"));
    assert!(rendered.contains("X, Y"));
    // No leftover generic tool card for the same call.
    assert!(!rendered.contains(">AskUserQuestion<"));
}

#[test]
fn plan_approval_renders_milestone_chip() {
    let jsonl = [
        r#"{"type":"assistant","timestamp":"2026-05-27T08:00:00Z","uuid":"11111111-0000-4000-8000-000000000001","requestId":"r1","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"ExitPlanMode","input":{}}]}}"#,
        r#"{"type":"user","timestamp":"2026-05-27T08:00:10Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"User has approved your plan. You can now start coding."}]}}"#,
    ].join("\n");
    let rendered = render(&jsonl);
    assert!(rendered.contains("ses-decision--approved"));
    assert!(rendered.contains("you approved the plan"));
}

#[test]
fn user_tool_rejection_renders_distinctly() {
    let jsonl = [
        r#"{"type":"assistant","timestamp":"2026-05-27T08:00:00Z","uuid":"11111111-0000-4000-8000-000000000001","requestId":"r1","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"rm x"}}]}}"#,
        r#"{"type":"user","timestamp":"2026-05-27T08:00:10Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"The user doesn't want to proceed with this tool use. The tool use was rejected."}]}}"#,
    ].join("\n");
    let rendered = render(&jsonl);
    assert!(rendered.contains("ses-decision--rejected"));
    assert!(rendered.contains("you rejected the tool use"));
}

#[test]
fn permission_denied_renders_with_reason() {
    let jsonl = [
        r#"{"type":"assistant","timestamp":"2026-05-27T08:00:00Z","uuid":"11111111-0000-4000-8000-000000000001","requestId":"r1","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"python3 -m http.server"}}]}}"#,
        r#"{"type":"user","timestamp":"2026-05-27T08:00:10Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"Permission for this action was denied by the Claude Code auto mode classifier. Reason: Starting python3 -m http.server binds to all interfaces by default."}]}}"#,
    ].join("\n");
    let rendered = render(&jsonl);
    assert!(rendered.contains("ses-decision--denied"));
    assert!(rendered.contains("permission denied"));
    assert!(rendered.contains("binds to all interfaces"));
}

// ─── the interpretation catalog (R14) ──────────────────────────────────────

#[test]
fn caveat_wrapper_is_suppressed_entirely() {
    let jsonl = [
        r#"{"type":"user","timestamp":"2026-05-27T08:00:00Z","message":{"role":"user","content":"<local-command-caveat>Caveat: not from the operator.</local-command-caveat>"},"isMeta":true}"#,
        r#"{"type":"user","timestamp":"2026-05-27T08:00:10Z","message":{"role":"user","content":"real prompt here","promptSource":"typed"}}"#,
    ].join("\n");
    let rendered = render(&jsonl);
    assert!(!rendered.contains("Caveat:"));
    assert!(rendered.contains("real prompt here"));
}

#[test]
fn command_wrapper_becomes_a_chip_and_stdout_folds_in() {
    let jsonl = [
        r#"{"type":"user","timestamp":"2026-05-27T08:00:00Z","message":{"role":"user","content":"<command-name>/model</command-name>\n<command-args>fable</command-args>"}}"#,
        r#"{"type":"user","timestamp":"2026-05-27T08:00:05Z","message":{"role":"user","content":"<local-command-stdout>Set model to Fable 5</local-command-stdout>"}}"#,
    ].join("\n");
    let rendered = render(&jsonl);
    assert!(rendered.contains("ses-cmd"));
    assert!(rendered.contains("/model"));
    assert!(rendered.contains("Set model to Fable 5"));
    // ONE command turn, not two separate cards (the fold).
    assert_eq!(rendered.matches("ses-cmd\"").count(), 1);
}

// ─── W6 — bare SGR remnant strip (W4 builder report, P12 tail) ─────────────

#[test]
fn command_stdout_sgr_remnants_are_stripped_but_prose_brackets_survive() {
    let jsonl = [
        r#"{"type":"user","timestamp":"2026-05-27T08:00:00Z","message":{"role":"user","content":"<command-name>/status</command-name>"}}"#,
        r#"{"type":"user","timestamp":"2026-05-27T08:00:05Z","message":{"role":"user","content":"<local-command-stdout>[1m[32mAll green[0m — see [note] and item [1][22m</local-command-stdout>"}}"#,
    ].join("\n");
    let rendered = render(&jsonl);
    assert!(rendered.contains("All green"));
    // The remnants are gone from the visible stdout body...
    assert!(!rendered.contains("[1m"));
    assert!(!rendered.contains("[32m"));
    assert!(!rendered.contains("[0m"));
    assert!(!rendered.contains("[22m"));
    // ...but ordinary prose brackets that merely look bracket-like (no
    // trailing digits+`m` shape) round-trip untouched.
    assert!(rendered.contains("[note]"));
    assert!(rendered.contains("item [1]"));
}

#[test]
fn task_notification_becomes_a_task_event_not_a_prompt_card() {
    let jsonl = r#"{"type":"user","timestamp":"2026-05-27T08:00:00Z","message":{"role":"user","content":"<task-notification>\n<task-id>wf1</task-id>\n<status>success</status>\n<summary>Recon done</summary>\n</task-notification>"}}"#;
    let rendered = render(jsonl);
    assert!(rendered.contains("ses-task"));
    assert!(rendered.contains("Recon done"));
    assert!(!rendered.contains("task-notification"));
}

#[test]
fn recall_memory_injection_becomes_a_chip_with_titles() {
    let jsonl = r#"{"type":"attachment","attachment":{"type":"hook_additional_context","hookName":"UserPromptSubmit","content":["Relevant memories from kb (recall - these persist across sessions):\n- ingest retry cap (2026-05-02): three attempts"]},"timestamp":"2026-05-27T08:00:00Z"}
{"type":"user","timestamp":"2026-05-27T08:00:05Z","message":{"role":"user","content":"go"},"promptSource":"typed"}"#;
    let rendered = render(jsonl);
    assert!(rendered.contains("ses-memory-injection"));
    assert!(rendered.contains("memory recalled")); // singular: 1 item in this fixture
    assert!(rendered.contains("ingest retry cap"));
}

#[test]
fn kb_command_bash_call_renders_a_kb_branded_card() {
    let jsonl = [
        r#"{"type":"assistant","timestamp":"2026-05-27T08:00:00Z","uuid":"11111111-0000-4000-8000-000000000001","requestId":"r1","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"kb remember \"x\" --tags y"}}]}}"#,
        r#"{"type":"user","timestamp":"2026-05-27T08:00:10Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"remembered abc123  (slug)"}]}}"#,
    ].join("\n");
    let rendered = render(&jsonl);
    assert!(rendered.contains("ses-tool--kb"));
    assert!(rendered.contains("kb remember"));
    assert!(rendered.contains("remembered abc123"));
}

#[test]
fn workflow_launch_renders_a_workflow_card() {
    let jsonl = r#"{"type":"assistant","timestamp":"2026-05-27T08:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Workflow","input":{"script":"export const meta = {\n  name: 'atlas-fit',\n  description: 'recon and build',\n};\n"}}]}}"#;
    let rendered = render(jsonl);
    assert!(rendered.contains("ses-workflow"));
    assert!(rendered.contains("atlas-fit"));
    assert!(rendered.contains("recon and build"));
}

// ─── hygiene bundle: empty thinking / ghost elimination ────────────────────

#[test]
fn empty_thinking_collapses_to_a_header_glyph_not_an_empty_accordion() {
    let jsonl = r#"{"type":"assistant","timestamp":"2026-05-27T08:00:00Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":""},{"type":"text","text":"hi"}]}}"#;
    let rendered = render(jsonl);
    assert!(rendered.contains("ses-turn__glyph"));
    assert!(rendered.contains("redacted thinking block"));
    // No empty <details class="ses-think"> — the glyph replaces it entirely.
    assert!(!rendered.contains(r#"<details class="ses-think">"#));
}

#[test]
fn non_empty_thinking_still_renders_a_preview_and_body() {
    let jsonl = r#"{"type":"assistant","timestamp":"2026-05-27T08:00:00Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"considering the options carefully"},{"type":"text","text":"hi"}]}}"#;
    let rendered = render(jsonl);
    assert!(rendered.contains(r#"<details class="ses-think">"#));
    assert!(rendered.contains("considering the options"));
}

#[test]
fn ghost_turns_never_render_no_empty_cards() {
    // A caveat-only + isMeta-only line, then a real assistant reply — must
    // not leave a blank card behind for the suppressed line.
    let jsonl = [
        r#"{"type":"user","timestamp":"2026-05-27T08:00:00Z","message":{"role":"user","content":"<local-command-caveat>x</local-command-caveat>"},"isMeta":true}"#,
        r#"{"type":"assistant","timestamp":"2026-05-27T08:00:10Z","message":{"role":"assistant","content":[{"type":"text","text":"ok"}]}}"#,
    ].join("\n");
    let rendered = render(&jsonl);
    // Exactly one turn card (`data-turn="1"`) — the caveat consumed zero
    // ordinals and rendered no card.
    assert!(rendered.contains(r#"data-turn="1""#));
    assert!(!rendered.contains(r#"data-turn="2""#));
}

// ─── outcome footer (R3) ────────────────────────────────────────────────

#[test]
fn outcome_footer_present_with_stable_anchor_and_closing_prose() {
    let jsonl = [
        r#"{"type":"user","timestamp":"2026-05-27T08:00:00Z","message":{"role":"user","content":"build it"},"promptSource":"typed"}"#,
        r#"{"type":"assistant","timestamp":"2026-05-27T08:00:10Z","message":{"role":"assistant","content":[{"type":"text","text":"Done. All good, shipped and verified in production."}]}}"#,
    ].join("\n");
    let rendered = render(&jsonl);
    assert!(rendered.contains(&format!(r#"id="{}""#, crate::sessions::SES_OUTCOME_ANCHOR)));
    assert!(rendered.contains("Done. All good, shipped"));
    // header jump-to-end affordance present too.
    assert!(rendered.contains("jump to the end"));
    assert!(rendered.contains("ses-head__tldr--asked"));
    assert!(rendered.contains("ses-head__tldr--closed"));
}

#[test]
fn outcome_footer_absent_message_for_a_husk() {
    let rendered = render_if_session(&wrap(""), "sessions").expect("renders");
    assert!(rendered.contains("no assistant closure captured"));
}

#[test]
fn outcome_footer_final_task_board_and_commits_render() {
    let jsonl = [
        r#"{"type":"assistant","timestamp":"2026-05-27T08:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"TaskCreate","input":{"subject":"Recon"}}]}}"#,
        r#"{"type":"user","timestamp":"2026-05-27T08:00:05Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"Created task 1"}]}}"#,
        r#"{"type":"assistant","timestamp":"2026-05-27T08:00:10Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"t2","name":"TaskUpdate","input":{"taskId":"1","status":"completed"}}]}}"#,
        r#"{"type":"user","timestamp":"2026-05-27T08:00:15Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t2","content":"Updated task 1"}]}}"#,
        r#"{"type":"assistant","timestamp":"2026-05-27T08:00:20Z","message":{"role":"assistant","content":[{"type":"text","text":"Done shipping the recon lane end to end."}]}}"#,
    ].join("\n");
    let html = wrap(&jsonl);
    // Splice a commits tail block (mirrors what `kb sessions capture` writes).
    let commit = crate::sessions::CapturedCommit {
        kind: "commit".to_string(),
        sha: Some("4f1c2ab".to_string()),
        subject: Some("atlas: clamp the fit scale".to_string()),
        resolved: true,
        ..Default::default()
    };
    let with_commits = html.replacen(
        "</body>",
        &format!(
            "{}\n</body>",
            crate::sessions::render_commits_block(&[commit])
        ),
        1,
    );
    let rendered = render_if_session(&with_commits, "sessions").expect("renders");
    assert!(rendered.contains("final task board"));
    assert!(rendered.contains("completed"));
    assert!(rendered.contains("4f1c2ab"));
    assert!(rendered.contains("atlas: clamp the fit scale"));
    assert!(rendered.contains("✓resolved"));
}

// ─── minimap determinism (Proposal 8) ──────────────────────────────────────

#[test]
fn minimap_omitted_when_no_turns() {
    let rendered = render_if_session(&wrap(""), "sessions").expect("renders");
    assert!(!rendered.contains(r#"<div class="ses-minimap">"#));
}

#[test]
fn minimap_is_byte_deterministic_across_repeated_renders() {
    let jsonl = [
        r#"{"type":"user","timestamp":"2026-05-27T08:00:00Z","message":{"role":"user","content":"first"},"promptSource":"typed"}"#,
        r#"{"type":"assistant","timestamp":"2026-05-27T08:00:10Z","message":{"role":"assistant","content":[{"type":"text","text":"ok"}]}}"#,
    ].join("\n");
    let a = render(&jsonl);
    let b = render(&jsonl);
    let extract_mm = |s: &str| -> String {
        let start = s.find(r#"<div class="ses-minimap">"#).unwrap();
        let end = s[start..].find("</div>").unwrap() + start;
        s[start..end].to_string()
    };
    assert_eq!(extract_mm(&a), extract_mm(&b));
}

// ─── XSS discipline ─────────────────────────────────────────────────────

#[test]
fn script_tag_in_task_subject_stays_escaped() {
    let jsonl = r#"{"type":"assistant","timestamp":"2026-05-27T08:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"TaskCreate","input":{"subject":"<script>alert(1)</script>"}}]}}"#;
    let rendered = render(jsonl);
    assert!(!rendered.contains("<script>alert(1)</script>"));
    assert!(rendered.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
}

#[test]
fn script_tag_in_prose_stays_escaped() {
    let jsonl = r#"{"type":"user","timestamp":"2026-05-27T08:00:00Z","message":{"role":"user","content":"<script>alert(1)</script>"},"promptSource":"typed"}"#;
    let rendered = render(jsonl);
    assert!(!rendered.contains("<script>alert(1)</script>"));
}

#[test]
fn script_tag_in_command_args_stays_escaped() {
    let jsonl = r#"{"type":"user","timestamp":"2026-05-27T08:00:00Z","message":{"role":"user","content":"<command-name>/x</command-name>\n<command-args><script>alert(1)</script></command-args>"}}"#;
    let rendered = render(jsonl);
    assert!(!rendered.contains("<script>alert(1)</script>"));
}

// ─── digest ↔ render lock-step (R14, classify_user_text shared) ───────────

fn read_shape_fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/session_fixtures")
        .join(format!("{name}.jsonl"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read fixture {name}: {e}"))
}

#[test]
fn header_asked_agrees_with_digest_first_user_prompt() {
    let jsonl = read_shape_fixture("synthetic-workflow-heavy");
    let activity = crate::sessions::parse_session_activity(&jsonl);
    let expected = activity
        .first_user_prompt
        .expect("digest has a first prompt");

    let rendered = render(&jsonl);
    // The header's asked: blockquote must contain the SAME text the digest's
    // first-prompt pass picked — both now route through the shared
    // classify_user_text/is_wrapper_text classification (R14).
    let start = rendered.find("ses-head__tldr--asked").unwrap();
    let window = &rendered[start..(start + 400).min(rendered.len())];
    let head_of_expected: String = expected.chars().take(40).collect();
    assert!(
        window.contains(&head_of_expected),
        "asked blockquote should agree with the digest first prompt: {window}"
    );
}

// ─── W6 — turn permalink + "comment on this turn" (moonshots M2) ──────────

#[test]
fn turn_timestamp_is_wrapped_in_a_permalink_anchor() {
    let rendered = render(
        r#"{"type":"user","timestamp":"2026-05-27T08:46:05.507Z","uuid":"aaaaaaaa-bbbb-4000-8000-000000000001","message":{"role":"user","content":"hello world"}}"#,
    );
    // The CSS class (`.ses-turn__permalink`) predates this wave (W1) but was
    // never actually wired to an `<a>` — this pins the wiring.
    assert!(
        rendered
            .contains(r##"<a class="ses-turn__permalink" href="#t-aaaaaaaabbbb">08:46:05</a>"##),
        "{rendered}"
    );
}

#[test]
fn turn_carries_a_comment_on_this_turn_affordance() {
    let rendered = render(
        r#"{"type":"user","timestamp":"2026-05-27T08:46:05.507Z","uuid":"aaaaaaaa-bbbb-4000-8000-000000000001","message":{"role":"user","content":"hello world"}}"#,
    );
    // No new anchor kind, no new wire (M2) — the button just carries the
    // turn's OWN stable id; the runtime script builds the Section anchor at
    // click time (session_render_runtime.js).
    assert!(
        rendered.contains(r#"data-kb-turn-comment="t-aaaaaaaabbbb""#),
        "{rendered}"
    );
    assert!(rendered.contains(r#"title="comment on this turn""#));
}

// ─── W6 — find-in-transcript (moonshots M7) ────────────────────────────────

#[test]
fn find_bar_renders_when_turns_exist() {
    let rendered = render(
        r#"{"type":"user","timestamp":"2026-05-27T08:00:00Z","message":{"role":"user","content":"hi"}}"#,
    );
    assert!(rendered.contains(r#"<div class="ses-find" id="ses-find" hidden>"#));
    assert!(rendered.contains(r#"id="ses-find-input""#));
    assert!(rendered.contains(r#"id="ses-find-count""#));
    assert!(rendered.contains(r#"data-find-action="prev""#));
    assert!(rendered.contains(r#"data-find-action="next""#));
    assert!(rendered.contains(r#"data-find-action="close""#));
}

#[test]
fn find_bar_absent_for_a_husk() {
    // A husk (zero turns) has nothing to find — mirrors `minimap_omitted_
    // when_no_turns` just above.
    let rendered = render_if_session(&wrap(""), "sessions").expect("renders");
    assert!(!rendered.contains(r#"id="ses-find""#));
}

#[test]
fn runtime_js_ships_the_find_in_transcript_wiring() {
    // Pins that the runtime-JS injection actually carries the M7 feature —
    // the `#find=` hash grammar and the turn-level match walk — not just
    // that the static bar markup exists.
    let rendered = render(
        r#"{"type":"user","timestamp":"2026-05-27T08:00:00Z","message":{"role":"user","content":"hi"}}"#,
    );
    assert!(rendered.contains("#find="));
    assert!(rendered.contains("ses-find-current"));
    assert!(rendered.contains("data-find-action"));
}
