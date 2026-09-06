//! Session-transcript renderer — Claude Code Stop-hook JSONL → readable HTML.
//!
//! Sessions are captured raw by `plugins/kb-memory/hooks/kb-capture.sh`: the
//! entire JSONL transcript wrapped in a `<pre>`, plus additive tail blocks
//! (commits/subagents/sidecar-text). That's the storage format (lossless,
//! re-renderable). This module is the *display* layer.
//!
//! **W1 (sessions-rethink) migration**: the renderer no longer parses the
//! transcript itself — it calls [`sessions::view::session_view`] (the
//! `session-view/1` engine, `sessions/view.rs`) ONCE and renders the
//! resulting [`SessionView`] IR. Every interpretation rule (requestId
//! merging, tool-result folding, wrapper-envelope classification, task
//! threading, closure extraction, turn identity) lives in the engine now;
//! this module is pure presentation over that tree — the same IR a future
//! CLI presenter (`kb sessions read`, W4) and wire route
//! (`GET /api/sessions/{sid}/view`, W2) will walk.
//!
//! Called from `kb-server::routes::artifact::serve` at request time (NOT at
//! index time): the on-disk file stays raw, and a renderer upgrade is picked
//! up on the next page load with no reindex / re-capture. Same lifecycle as
//! `iframe::inject_script` and `scrub::scrub_export` — a pure HTML transform
//! applied on serve.
//!
//! The renderer is intentionally tolerant: the engine's own tolerance floor
//! (a malformed line becomes an [`Item::Raw`]) means `render_if_session`
//! never panics on adversarial input either — the transcript must always
//! render *something*.
//!
//! **Renderer versioning**: the output stamps
//! `<meta name="kb-renderer" content="session-view/1">` (reader.md Proposal
//! 10) so a bug report can name the exact interpretation grammar that
//! produced a given page.
//!
//! **Deep-link grammar change (accepted break, documented — memo R2/D3)**:
//! turn anchors are now the stable `id="t-<uuid12>"` (`data-turn="<ordinal>"`
//! carries the old numeric position as DATA only). A bookmarked `#turn-N`
//! link from a renderer built before this wave will no longer resolve to an
//! element — renumbering across a join-pass rewrite made ordinal-only anchors
//! renderer-version-scoped from the start; the stable `t-<uuid12>` form is
//! the fix going forward.

use crate::sessions::view::{self, Item, MinimapKind, Role, SessionView, TailBlocks, ViewOptions};

const STYLE: &str = include_str!("session_render.css");
const RUNTIME_JS: &str = include_str!("session_render_runtime.js");

/// True when the html looks like a captured session transcript — i.e.
/// has `<meta name="kb-category" content="memory-session">`. Reads the
/// actual meta VALUE (first `name="kb-category"` wins, same scan as
/// `extract_head_meta`) — a document that merely *mentions*
/// memory-session in its body while declaring its own kb-category (e.g.
/// a design doc about the sessions system) must never be mistaken for a
/// transcript and hijacked by this renderer.
pub fn is_session_transcript(html: &str) -> bool {
    pick_meta(html, "kb-category").as_deref() == Some("memory-session")
}

/// Render the transcript to a full HTML document. Returns `None` if the
/// input isn't a session transcript or the body has no `<pre>` to parse.
///
/// `kb_name` is stamped onto `<body data-kb="…">` so the runtime JS can
/// target the correct kb when posting `open-artifact` to the SPA parent
/// from clicked file-path links.
pub fn render_if_session(html: &str, kb_name: &str) -> Option<String> {
    if !is_session_transcript(html) {
        return None;
    }
    let pre = extract_pre_body(html)?;
    let jsonl = html_unescape(pre);
    let head = extract_head_meta(html);
    let tail = TailBlocks::from_html(html);
    let sv = view::session_view(&jsonl, &tail, &ViewOptions::default());
    Some(build_document(head, &sv, kb_name))
}

// ─── extraction helpers ────────────────────────────────────────────────

fn extract_pre_body(html: &str) -> Option<&str> {
    let start = html.find("<pre>")? + "<pre>".len();
    let end = html[start..].find("</pre>")? + start;
    Some(&html[start..end])
}

/// Reverse of kb-capture.sh's escape chain, PLUS `&quot;`/`&#39;` (a
/// superset of the minimal inverse `sessions::recover_jsonl_from_capture`
/// uses for the byte-identical export round-trip — this fn is display-only,
/// never used for export). One left-to-right pass; entity match order
/// mirrors the historical chained replace (`&amp;` last among the three
/// capture entities, then the two display-only extras).
fn html_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'&' {
            let rest = &s[i..];
            if rest.starts_with("&lt;") {
                out.push('<');
                i += 4;
                continue;
            }
            if rest.starts_with("&gt;") {
                out.push('>');
                i += 4;
                continue;
            }
            if rest.starts_with("&quot;") {
                out.push('"');
                i += 6;
                continue;
            }
            if rest.starts_with("&#39;") {
                out.push('\'');
                i += 5;
                continue;
            }
            if rest.starts_with("&amp;") {
                out.push('&');
                i += 5;
                continue;
            }
        }
        let ch = s[i..].chars().next().expect("i < len");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

#[derive(Default)]
struct HeadMeta {
    title: String,
    session_id: Option<String>,
    category: Option<String>,
    decay: Option<String>,
}

fn extract_head_meta(html: &str) -> HeadMeta {
    let mut h = HeadMeta::default();
    if let (Some(s), Some(e)) = (html.find("<title>"), html.find("</title>")) {
        h.title = html[s + 7..e].to_string();
    }
    h.session_id = pick_meta(html, "kb-session");
    h.category = pick_meta(html, "kb-category");
    h.decay = pick_meta(html, "kb-decay");
    h
}

fn pick_meta(html: &str, name: &str) -> Option<String> {
    let needle = format!(r#"name="{name}""#);
    let i = html.find(&needle)?;
    let tail = &html[i..];
    let c = tail.find(r#"content=""#)?;
    let after = &tail[c + 9..];
    let end = after.find('"')?;
    Some(after[..end].to_string())
}

// ─── document assembly ────────────────────────────────────────────────

// A4/A7 — render-scoped session cwd, so `render_tool_call` can resolve a
// relative file path against the session's working dir without threading cwd
// through every render signature. Set once per document (render is
// synchronous), read while rendering tool calls, cleared at the end of
// `build_document`. Thread-local because the daemon renders concurrently.
thread_local! {
    static RENDER_CWD: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

fn build_document(head: HeadMeta, sv: &SessionView, kb_name: &str) -> String {
    RENDER_CWD.with(|c| *c.borrow_mut() = sv.header.cwd.clone());
    let flow_html = render_flow(sv);
    RENDER_CWD.with(|c| *c.borrow_mut() = None);

    let mut out = String::with_capacity(64 * 1024);

    out.push_str("<!doctype html>\n<html lang=\"en\"><head>\n");
    out.push_str(r#"<meta charset="utf-8">"#);
    out.push_str(r#"<meta name="viewport" content="width=device-width,initial-scale=1">"#);
    out.push_str("<title>");
    push_esc(&mut out, &head.title);
    out.push_str("</title>\n");
    if let Some(s) = &head.session_id {
        out.push_str(r#"<meta name="kb-session" content=""#);
        push_attr(&mut out, s);
        out.push_str("\">\n");
    }
    if let Some(c) = &head.category {
        out.push_str(r#"<meta name="kb-category" content=""#);
        push_attr(&mut out, c);
        out.push_str("\">\n");
    }
    if let Some(d) = &head.decay {
        out.push_str(r#"<meta name="kb-decay" content=""#);
        push_attr(&mut out, d);
        out.push_str("\">\n");
    }
    out.push_str(&format!(
        r#"<meta name="kb-renderer" content="{}">"#,
        view::VIEW_GRAMMAR
    ));
    out.push('\n');
    out.push_str("<style>");
    out.push_str(STYLE);
    out.push_str("</style>\n");
    out.push_str("</head><body data-kb=\"");
    push_attr(&mut out, kb_name);
    out.push_str("\">\n");

    out.push_str(&render_minimap(sv));
    out.push_str(&render_header(&head, sv));
    out.push_str(&render_filter_toolbar());

    out.push_str(r#"<main class="ses-flow">"#);
    out.push_str(&flow_html);
    out.push_str("</main>\n");

    out.push_str(&render_outcome_footer(sv));

    // reader.md Proposal 3 — the ↧ result FAB: a fixed, always-visible jump
    // affordance that survives the ≤1280px iframe reality where the vertical
    // jump rail (see `.ses-jump` — retired this wave in favor of the minimap,
    // #turn-N deep links replaced by `t-<uuid12>`) was hidden. Runtime JS
    // toggles `.is-visible` once the reader has scrolled more than one
    // viewport away from the bottom.
    if !sv.turns.is_empty() {
        out.push_str(&format!(
            r##"<a class="ses-endfab" href="#{}" title="jump to the end">↧ result</a>"##,
            crate::sessions::SES_OUTCOME_ANCHOR
        ));
    }
    out.push_str(&render_find_bar(sv));
    out.push_str("<script>");
    out.push_str(RUNTIME_JS);
    out.push_str("</script>\n");

    out.push_str("</body></html>");
    out
}

// ─── header ─────────────────────────────────────────────────────────────

fn render_header(head: &HeadMeta, sv: &SessionView) -> String {
    let h = &sv.header;
    let mut out = String::new();
    out.push_str(r#"<header class="ses-head">"#);
    out.push_str(r#"<div class="ses-head__title"><h1>"#);
    match &h.title {
        Some(t) if !t.trim().is_empty() => push_esc(&mut out, t),
        _ => push_esc(&mut out, &head.title),
    }
    out.push_str("</h1>");
    if let Some(sid) = &head.session_id {
        out.push_str(r#"<code class="ses-head__sid">"#);
        push_esc(&mut out, sid);
        out.push_str("</code>");
    }
    // A5 — escape hatch to the raw JSONL transcript (skips this render pass,
    // never the outbound scrub). Relative `?raw=1` resolves against this
    // artifact's URL.
    out.push_str(r#"<a class="ses-head__raw" href="?raw=1" title="view the raw JSONL transcript">view raw</a>"#);
    if h.outcome.is_some() {
        out.push_str(&format!(
            r##"<a class="ses-head__end" href="#{}" title="jump to the end">jump to the end ↓</a>"##,
            crate::sessions::SES_OUTCOME_ANCHOR
        ));
    }
    out.push_str("</div>");

    out.push_str(r#"<dl class="ses-head__meta">"#);
    if let Some(m) = &h.model {
        out.push_str("<dt>model</dt><dd>");
        push_esc(&mut out, m);
        out.push_str("</dd>");
    }
    if let Some(c) = &h.cwd {
        out.push_str("<dt>cwd</dt><dd><code>");
        push_esc(&mut out, c);
        out.push_str("</code></dd>");
    }
    if let Some(b) = &h.git_branch {
        out.push_str("<dt>branch</dt><dd><code>");
        push_esc(&mut out, b);
        out.push_str("</code></dd>");
    }
    out.push_str("<dt>harness</dt><dd>");
    push_esc(&mut out, &h.harness);
    out.push_str("</dd>");
    // R6/P11 — honest activity line: exchanges (real prompts) · events
    // (everything the parse pass saw) · active time (clamped-delta sum) ·
    // wall span.
    out.push_str(&format!(
        "<dt>activity</dt><dd>{} exchange{} · {} event{} · active {} · span {}</dd>",
        sv.outline.len(),
        if sv.outline.len() == 1 { "" } else { "s" },
        sv.stats.events,
        if sv.stats.events == 1 { "" } else { "s" },
        duration_human_secs(h.active_secs),
        h.span_secs
            .map(duration_human_secs)
            .unwrap_or_else(|| "?".to_string()),
    ));
    out.push_str(&format!(
        "<dt>tools</dt><dd>{} call{}{}</dd>",
        h.tool_calls,
        if h.tool_calls == 1 { "" } else { "s" },
        if h.error_count > 0 {
            format!(
                " · {} error{}",
                h.error_count,
                if h.error_count == 1 { "" } else { "s" }
            )
        } else {
            String::new()
        }
    ));
    if h.tokens.input > 0 || h.tokens.output > 0 || h.tokens.cache_read > 0 {
        out.push_str("<dt>tokens</dt><dd>");
        out.push_str(&format_tokens(&h.tokens));
        out.push_str("</dd>");
    }
    out.push_str("</dl>");

    // R3 — asked/closed peer blockquotes.
    if let Some(opening) = &h.opening {
        out.push_str(r#"<blockquote class="ses-head__tldr ses-head__tldr--asked">"#);
        out.push_str(r#"<span class="ses-head__tldr-label">asked</span>"#);
        push_esc(&mut out, &truncate(&opening.text, 280));
        out.push_str("</blockquote>");
    }
    if let Some(outcome) = &h.outcome {
        out.push_str(r#"<blockquote class="ses-head__tldr ses-head__tldr--closed">"#);
        out.push_str(r#"<span class="ses-head__tldr-label">closed</span>"#);
        push_esc(
            &mut out,
            &truncate_chars(&outcome.text, crate::sessions::OUTCOME_WIRE_MAX_CHARS),
        );
        out.push_str("</blockquote>");
    }
    out.push_str("</header>\n");
    out
}

/// `in (uncached) · out · cache write · cache read`, lead figure relabelled
/// `effective in` (R6/P11 — `input + cache_read`, what the model actually
/// attended to). Cache columns omitted when zero.
fn format_tokens(t: &view::TokenLine) -> String {
    let mut parts = vec![format!(
        "{} effective in",
        human_tokens(t.effective_input())
    )];
    parts.push(format!("{} out", human_tokens(t.output)));
    if t.cache_write > 0 {
        parts.push(format!("{} cache write", human_tokens(t.cache_write)));
    }
    if t.cache_read > 0 {
        parts.push(format!("{} cache hit", human_tokens(t.cache_read)));
    }
    parts.join(" · ")
}

fn human_tokens(n: u64) -> String {
    if n < 1_000 {
        return n.to_string();
    }
    if n < 1_000_000 {
        let k = n as f64 / 1_000.0;
        if k < 10.0 {
            return format!("{k:.1}k");
        }
        return format!("{}k", k.round() as u64);
    }
    let m = n as f64 / 1_000_000.0;
    if m < 10.0 {
        format!("{m:.1}M")
    } else {
        format!("{}M", m.round() as u64)
    }
}

/// `Xs` / `Xm Ys` / `Xh Ym` / `Xd` — used for both active-time and span.
/// Days appear once the value clears 24h, at which point minutes are noise.
fn duration_human_secs(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 60 {
        return format!("{secs}s");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m {}s", secs % 60);
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h {}m", mins % 60);
    }
    let days = hours / 24;
    format!("{days}d {}h", hours % 24)
}

// ─── minimap (reader.md Proposal 8) ─────────────────────────────────────

/// Deterministic integer-math SVG strip — one `<rect>` per notable turn,
/// `x = pos_1000` (already 0..1000, matching the `viewBox` width exactly, no
/// floating point anywhere). Degrades to nothing when `sv.minimap` is empty
/// (a husk capture). Click-to-jump + cursor tracking is runtime JS, reusing
/// the existing IntersectionObserver wiring.
fn render_minimap(sv: &SessionView) -> String {
    if sv.minimap.is_empty() {
        return String::new();
    }
    let mut marks = String::with_capacity(sv.minimap.len() * 80);
    for p in &sv.minimap {
        let x = p.pos_1000.min(998);
        let (cls, y, h) = match p.kind {
            MinimapKind::Human => ("ses-mm-human", 2, 12),
            MinimapKind::Error => ("ses-mm-error", 16, 10),
            MinimapKind::Commit => ("ses-mm-commit", 16, 10),
            MinimapKind::TaskDone => ("ses-mm-task", 28, 10),
            MinimapKind::Sidechain => ("ses-mm-side", 40, 6),
        };
        marks.push_str(&format!(
            r##"<a href="#{}" class="{cls}" data-turn="{}"><rect x="{x}" y="{y}" width="2" height="{h}"/></a>"##,
            turn_href(sv, p.ordinal),
            p.ordinal
        ));
    }
    format!(
        r#"<div class="ses-minimap"><svg viewBox="0 0 1000 48" preserveAspectRatio="none" aria-hidden="false" role="img" aria-label="activity density">{marks}</svg></div>"#
    )
}

fn turn_href(sv: &SessionView, ordinal: u32) -> String {
    sv.turns
        .iter()
        .find(|t| t.ordinal == ordinal)
        .map(|t| t.id.clone())
        .unwrap_or_default()
}

// ─── filter toolbar ─────────────────────────────────────────────────────

fn render_filter_toolbar() -> String {
    // Defaults: user/assistant/tools/system visible; thinking + hooks + debug
    // hidden by default. Runtime JS hydrates from localStorage on load.
    String::from(
        r#"<nav class="ses-bar" aria-label="filters">
  <label class="ses-bar__pill" title="shift-click to solo"><input type="checkbox" data-filter="user" checked> user</label>
  <label class="ses-bar__pill" title="shift-click to solo"><input type="checkbox" data-filter="assistant" checked> assistant</label>
  <label class="ses-bar__pill" title="shift-click to solo"><input type="checkbox" data-filter="tools" checked> tools</label>
  <label class="ses-bar__pill" title="shift-click to solo"><input type="checkbox" data-filter="thinking"> thinking</label>
  <label class="ses-bar__pill" title="shift-click to solo"><input type="checkbox" data-filter="system" checked> system</label>
  <label class="ses-bar__pill" title="shift-click to solo"><input type="checkbox" data-filter="hooks"> hooks</label>
  <label class="ses-bar__pill" title="shift-click to solo"><input type="checkbox" data-filter="debug"> debug</label>
  <span class="ses-bar__sep" aria-hidden="true"></span>
  <button class="ses-bar__chip" type="button" data-action="filter-all" title="show all categories">all</button>
  <button class="ses-bar__chip" type="button" data-action="filter-none" title="hide all categories">none</button>
  <input class="ses-bar__search" type="search" placeholder="filter tools…" data-search aria-label="search tools">
  <button class="ses-bar__btn" type="button" data-action="expand-all" title="expand all">⇅</button>
  <button class="ses-bar__btn" type="button" data-action="collapse-all" title="collapse all">⇆</button>
</nav>
"#,
    )
}

// ─── find-in-transcript (W6 — moonshots M7) ────────────────────────────

/// The find bar's STATIC markup — hidden by default (`hidden` attribute),
/// toggled by `session_render_runtime.js` (`/` when not typing elsewhere, or
/// a `#find=<term>` hash on load). Same discipline as
/// [`render_filter_toolbar`]: the server emits inert, golden-tested HTML;
/// the runtime script only ever queries these EXACT ids/classes, never
/// constructs the bar itself — so the visible chrome is renderer-golden-
/// pinned the same way every other toolbar is, and there is nothing here
/// for the runtime to inject user-controlled text INTO (no `innerHTML` of a
/// search term anywhere — count/highlight are class toggles + `textContent`
/// reads, never markup built from the query). Emitted only once turns exist
/// — a husk capture has nothing to find.
fn render_find_bar(sv: &SessionView) -> String {
    if sv.turns.is_empty() {
        return String::new();
    }
    String::from(
        r#"<div class="ses-find" id="ses-find" hidden>
  <input class="ses-find__input" id="ses-find-input" type="text" aria-label="find in transcript" placeholder="find in transcript…" autocomplete="off">
  <span class="ses-find__count" id="ses-find-count" aria-live="polite">0/0</span>
  <button type="button" class="ses-find__btn" data-find-action="prev" title="previous match (Shift+Enter)">↑</button>
  <button type="button" class="ses-find__btn" data-find-action="next" title="next match (Enter)">↓</button>
  <button type="button" class="ses-find__btn ses-find__close" data-find-action="close" title="close (Esc)">✕</button>
</div>
"#,
    )
}

// ─── conversation flow ────────────────────────────────────────────────

/// Renders every turn card, wrapping consecutive-sidechain runs
/// (`sv.side_lanes`) in a collapsible lane, and inserting a time-gap chip
/// between two turns whose timestamps are >30s apart (mirrors the pre-IR
/// renderer's threshold, computed here from `Turn::ts` rather than at parse
/// time — a render-time concern now that turns already carry timestamps).
fn render_flow(sv: &SessionView) -> String {
    let mut lane_of: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (i, lane) in sv.side_lanes.iter().enumerate() {
        for id in &lane.turn_ids {
            lane_of.insert(id.as_str(), i);
        }
    }

    let mut out = String::new();
    let mut open_lane: Option<usize> = None;
    let mut last_ts: Option<&str> = None;

    for t in &sv.turns {
        let this_lane = lane_of.get(t.id.as_str()).copied();
        if open_lane != this_lane {
            if open_lane.is_some() {
                out.push_str("</div></details>\n");
            }
            if let Some(li) = this_lane {
                let lane = &sv.side_lanes[li];
                let label = lane
                    .agent
                    .clone()
                    .unwrap_or_else(|| format!("sidechain #{}", li + 1));
                out.push_str(r#"<details class="ses-side-lane" open><summary>↳ "#);
                push_esc(&mut out, &label);
                out.push_str(&format!(
                    " — {} turns</summary><div class=\"ses-side-lane__body\">",
                    lane.turn_ids.len()
                ));
            }
            open_lane = this_lane;
        }

        if let (Some(prev), Some(curr)) = (last_ts, t.ts.as_deref()) {
            if let Some(label) = time_gap_label(prev, curr) {
                out.push_str(&format!(
                    r#"<aside class="ses-time-gap"><span>⌚ {}</span></aside>"#,
                    esc(&label)
                ));
            }
        }
        if t.ts.is_some() {
            last_ts = t.ts.as_deref();
        }

        out.push_str(&render_turn(t));
    }
    if open_lane.is_some() {
        out.push_str("</div></details>\n");
    }
    out
}

fn time_gap_label(prev_iso: &str, curr_iso: &str) -> Option<String> {
    const THRESHOLD_SECS: i64 = 30;
    let prev = crate::timeparse::parse_iso_utc(prev_iso)?;
    let curr = crate::timeparse::parse_iso_utc(curr_iso)?;
    let secs = curr - prev;
    if secs < THRESHOLD_SECS {
        return None;
    }
    Some(format!("{} later", duration_human_secs(secs)))
}

/// One turn card. `id="t-<uuid12>"` is the stable anchor; `data-turn`
/// carries the render-order ordinal (R2 — visible label, not a live anchor).
/// Ghost elimination already happened in the engine (every `Turn` here has
/// ≥1 item) — nothing to filter here. Empty-thinking fragments collapse to a
/// header glyph (`◆×n`) instead of a per-block accordion (hygiene bundle).
fn render_turn(t: &Turn) -> String {
    let role_label = match t.role {
        Role::Human => "you",
        Role::Assistant => "assistant",
    };
    let role_class = match t.role {
        Role::Human => "ses-turn--user",
        Role::Assistant => "ses-turn--assistant",
    };
    let side_class = if t.sidechain { " ses-turn--side" } else { "" };

    let empty_thinking = t
        .items
        .iter()
        .filter(|i| matches!(i, Item::Thinking { empty: true, .. }))
        .count();
    let glyph = if empty_thinking > 0 {
        format!(
            r#"<span class="ses-turn__glyph" title="{empty_thinking} redacted thinking block{}">{}</span>"#,
            if empty_thinking == 1 { "" } else { "s" },
            "◆".repeat(empty_thinking.min(5))
        )
    } else {
        String::new()
    };

    // W6 (moonshots M2) — the timestamp is now the "per-turn permalink
    // anchor" the CSS has carried since W1 (`.ses-turn__permalink`,
    // session_render.css) but never had a link to style: a plain in-page
    // `href="#t-<uuid12>"` jump, no JS required. `esc(&t.id)` twice is
    // deliberate (href + the sibling comment button below) — `t.id` is a
    // hex-and-dash uuid fragment so escaping is a no-op today, but the
    // convention is "every emitted id goes through esc()", not "this one
    // happens to be safe".
    let ts_html = t.ts.as_deref().map(short_time).map(|s| {
        format!(
            r##"<a class="ses-turn__permalink" href="#{}">{}</a>"##,
            esc(&t.id),
            esc(&s)
        )
    });
    let ts_html = ts_html.unwrap_or_default();

    // W6 (moonshots M2) — "comment on this turn": posts the SAME `cm:
    // compose` message annotate.ts's click-to-compose path already sends
    // (Section-scope anchor, no new anchor kind, no new wire — see
    // session_render_runtime.js's delegated handler), so a reader can flag
    // one turn for review without first toggling full annotate mode. Always
    // emitted (comment-affordance doesn't require a timestamp); harmless —
    // and silently inert — when the artifact's kb has comments disabled, the
    // same posture as the `.ses-path` open-artifact link above.
    // "❝" mirrors the glyph annotate.ts's own in-page comment icon uses
    // (web/src/scripts/annotate.ts `addIcon`) — one comment-affordance
    // symbol across both the always-present runtime chrome and the
    // annotate-mode overlay.
    let comment_btn = format!(
        r#"<button type="button" class="ses-turn__comment" data-kb-turn-comment="{}" title="comment on this turn">❝</button>"#,
        esc(&t.id)
    );

    let body_bytes: usize = t.items.iter().map(item_byte_estimate).sum();
    let est = (body_bytes / 6).clamp(120, 4000);

    let mut body = String::new();
    for item in &t.items {
        if matches!(item, Item::Thinking { empty: true, .. }) {
            continue; // folded into the header glyph
        }
        body.push_str(&render_item(item));
    }

    let debug = render_debug_details(t);

    format!(
        r#"<article class="ses-turn {role_class}{side_class}" id="{}" data-turn="{}" style="--ses-turn-est: {est}px"><header class="ses-turn__head"><span class="ses-turn__role">{role_label}</span>{ts_html}{comment_btn}{glyph}{debug}</header><div class="ses-turn__body">{body}</div></article>"#,
        esc(&t.id),
        t.ordinal,
    )
}

/// The per-turn "raw" accordion (P13/hygiene bundle) — id/ordinal/sidechain/
/// agent, everything the [`Turn`] carries beyond its rendered content.
/// Gated behind the `debug` filter pill (CSS `body[data-hide-debug]`), never
/// part of expand-all (the runtime JS's `:not(.ses-debug)` selector already
/// excludes this class). A trimmed successor of the pre-IR accordion (which
/// also showed `parentUuid`/`requestId`/`sessionId` — those lived on the raw
/// JSONL record, not on the merged [`Turn`]; `id` and `data-turn` above
/// already expose the two identifiers that matter for grep'ing the raw
/// transcript or building a deep link).
fn render_debug_details(t: &Turn) -> String {
    let mut rows = vec![format!("id: {}", t.id), format!("ordinal: {}", t.ordinal)];
    if t.sidechain {
        rows.push("sidechain: true".to_string());
    }
    if let Some(a) = &t.agent_id {
        rows.push(format!("agent: {a}"));
    }
    format!(
        r#"<details class="ses-debug"><summary>raw</summary><pre>{}</pre></details>"#,
        esc(&rows.join("\n"))
    )
}

fn item_byte_estimate(i: &Item) -> usize {
    match i {
        Item::Prose { text } => text.len(),
        Item::Thinking { text, .. } => text.as_deref().map(str::len).unwrap_or(0),
        Item::ToolCall { headline, .. } => headline.len() + 200,
        Item::Command { stdout, .. } => 60 + stdout.as_deref().map(str::len).unwrap_or(0),
        Item::TaskEvent { .. } | Item::ModeChange { .. } | Item::TimeGap { .. } => 40,
        Item::WorkflowCard { description, .. } => {
            200 + description.as_deref().map(str::len).unwrap_or(0)
        }
        Item::KbCommand { result_view, .. } => {
            80 + result_view.as_deref().map(str::len).unwrap_or(0)
        }
        Item::MemoryInjection { items } => items.iter().map(String::len).sum::<usize>() + 40,
        Item::SystemReminder { preview } => preview.len(),
        Item::Decision { prompt, answer } => {
            prompt.len() + answer.as_deref().map(str::len).unwrap_or(0) + 40
        }
        Item::Raw { .. } => 80,
    }
}

use crate::sessions::view::Turn;

fn render_item(item: &Item) -> String {
    match item {
        Item::Prose { text } => render_prose(text),
        Item::Thinking { empty, len, text } => {
            if *empty {
                String::new() // header glyph handles this
            } else {
                let body = text.as_deref().unwrap_or("");
                let preview = truncate(body.trim(), 80);
                format!(
                    r#"<details class="ses-think"><summary>◆ thinking · {} · {len} chars</summary><div class="ses-think__body">{}</div></details>"#,
                    esc(&preview),
                    render_prose(body)
                )
            }
        }
        Item::ToolCall {
            name,
            kind,
            headline,
            input_view,
            result,
            unpaired,
            path_for_link,
        } => render_tool_call(
            name,
            *kind,
            headline,
            input_view,
            result.as_ref(),
            *unpaired,
            path_for_link.as_deref(),
        ),
        Item::Command { name, args, stdout } => {
            render_command(name, args.as_deref(), stdout.as_deref())
        }
        Item::TaskEvent {
            id,
            subject,
            transition,
        } => render_task_event(id, subject, transition),
        Item::WorkflowCard {
            name,
            description,
            phases,
            task_id,
        } => render_workflow_card(
            name.as_deref(),
            description.as_deref(),
            phases,
            task_id.as_deref(),
        ),
        Item::KbCommand {
            verb,
            args,
            result_view,
        } => render_kb_command(verb, args, result_view.as_deref()),
        Item::MemoryInjection { items } => render_memory_injection(items),
        Item::SystemReminder { preview } => render_chip(&format!("system: {preview}")),
        Item::Decision { prompt, answer } => render_decision(prompt, answer.as_deref()),
        Item::ModeChange { mode } => render_chip(&format!("mode → {mode}")),
        Item::TimeGap { secs } => render_chip(&format!("⌚ {}", duration_human_secs(*secs))),
        Item::Raw { reason } => format!(
            r#"<details class="ses-raw"><summary>{}</summary></details>"#,
            esc(reason)
        ),
    }
}

fn render_chip(label: &str) -> String {
    format!(r#"<aside class="ses-chip">{}</aside>"#, esc(label))
}

fn render_prose(text: &str) -> String {
    let mut out = String::new();
    let mut buf = String::new();
    let mut in_fence = false;
    let mut fence_buf = String::new();
    for line in text.split('\n') {
        let trimmed = line.trim_end();
        if trimmed.starts_with("```") {
            if in_fence {
                out.push_str(r#"<pre class="ses-code"><code>"#);
                push_esc(&mut out, &fence_buf);
                out.push_str("</code></pre>");
                fence_buf.clear();
                in_fence = false;
            } else {
                flush_paragraph(&mut out, &mut buf);
                in_fence = true;
            }
            continue;
        }
        if in_fence {
            fence_buf.push_str(line);
            fence_buf.push('\n');
            continue;
        }
        if trimmed.is_empty() {
            flush_paragraph(&mut out, &mut buf);
        } else {
            if !buf.is_empty() {
                buf.push('\n');
            }
            buf.push_str(line);
        }
    }
    if in_fence {
        out.push_str(r#"<pre class="ses-code"><code>"#);
        push_esc(&mut out, &fence_buf);
        out.push_str("</code></pre>");
    }
    flush_paragraph(&mut out, &mut buf);
    out
}

fn flush_paragraph(out: &mut String, buf: &mut String) {
    if buf.is_empty() {
        return;
    }
    out.push_str("<p>");
    let escaped = esc(buf);
    out.push_str(&escaped.replace('\n', "<br>"));
    out.push_str("</p>");
    buf.clear();
}

// ─── tool calls ─────────────────────────────────────────────────────────

fn render_tool_call(
    name: &str,
    kind: view::ToolClass,
    headline: &str,
    input_view: &view::InputView,
    result: Option<&view::ResultView>,
    unpaired: bool,
    path_for_link: Option<&str>,
) -> String {
    let kind_cls = tool_kind_class(kind);
    let head_html = match path_for_link {
        Some(p) => render_file_path(p),
        None => esc(headline),
    };
    let input_html = match input_view {
        view::InputView::Small { pretty } => esc(pretty),
        view::InputView::Summary { keys, bytes } => {
            format!(
                "{{ {} keys, {} bytes — full payload behind raw }}",
                keys.len(),
                bytes
            )
        }
    };
    let mut out = String::new();
    if let Some(r) = result {
        if r.is_error {
            out.push_str(&format!(
                r#"<aside class="ses-error-callout"><span class="ses-error-callout__head">✗ failed</span><span class="ses-error-callout__msg">{}</span></aside>"#,
                esc(&truncate(r.preview.trim(), 140))
            ));
        }
    }
    out.push_str(&format!(
        r#"<details class="ses-tool ses-tool--{kind_cls}"><summary><span class="ses-tool__name">{}</span><span class="ses-tool__head">{head_html}</span></summary><pre class="ses-tool__input">{input_html}</pre>"#,
        esc(name)
    ));
    match result {
        Some(r) => {
            let cls = if r.is_error {
                "ses-result ses-result--err"
            } else {
                "ses-result"
            };
            out.push_str(&format!(
                r#"<details class="{cls}"><summary>{}{}</summary><pre>{}</pre></details>"#,
                if r.is_error {
                    "✗ result · "
                } else {
                    "↳ result · "
                },
                esc(&truncate(r.preview.trim(), 80)),
                esc(&r.raw)
            ));
        }
        None if unpaired => {
            out.push_str(
                r#"<aside class="ses-chip ses-chip--unpaired">no result captured</aside>"#,
            );
        }
        None => {}
    }
    out.push_str("</details>");
    out
}

fn tool_kind_class(kind: view::ToolClass) -> &'static str {
    match kind {
        view::ToolClass::File => "file",
        view::ToolClass::Shell => "shell",
        view::ToolClass::Delegate => "delegate",
        view::ToolClass::Net => "net",
        view::ToolClass::Plan => "plan",
        view::ToolClass::Mcp => "mcp",
        view::ToolClass::Other => "other",
    }
}

/// Render a tool-call's `file_path`: an in-corpus link (resolved against the
/// daemon mount table + the render-scoped session cwd) or plain text when the
/// path is out-of-corpus. Carries `data-kb`/`data-rel`, which the runtime
/// posts so the SPA navigates to the right artifact.
fn render_file_path(p: &str) -> String {
    let cwd = RENDER_CWD.with(|c| c.borrow().clone());
    let mounts = crate::sessions::corpus_mounts();
    let (in_corpus, kb, _id) = crate::sessions::resolve_corpus_path(p, cwd.as_deref(), &mounts);
    if in_corpus {
        if let Some(kb) = kb {
            let rel = resolve_source_relative(p, cwd.as_deref(), &kb, &mounts);
            return format!(
                r##"<a class="ses-path" href="#" data-kb="{}" data-rel="{}">{}</a>"##,
                esc(&kb).replace('"', "&quot;"),
                esc(&rel).replace('"', "&quot;"),
                esc(&truncate(p, 90))
            );
        }
    }
    format!(
        r#"<span class="ses-path ses-path--plain">{}</span>"#,
        esc(&truncate(p, 90))
    )
}

fn resolve_source_relative(
    p: &str,
    cwd: Option<&str>,
    kb: &str,
    mounts: &[crate::sessions::CorpusMount],
) -> String {
    use std::path::Path;
    let abs = {
        let pp = Path::new(p);
        if pp.is_absolute() {
            pp.to_path_buf()
        } else if let Some(c) = cwd.filter(|c| !c.is_empty()) {
            Path::new(c).join(pp)
        } else {
            return String::new();
        }
    };
    mounts
        .iter()
        .find(|m| m.kb == kb)
        .map(|m| crate::paths::doc_rel_path(&abs.to_string_lossy(), &m.source_root))
        .unwrap_or_default()
}

// ─── the interpretation catalog's other card kinds ──────────────────────

fn render_command(name: &str, args: Option<&str>, stdout: Option<&str>) -> String {
    let label = match args {
        Some(a) if !a.is_empty() => format!("⌁ {name} {a}"),
        _ => format!("⌁ {name}"),
    };
    match stdout {
        Some(out) if !out.trim().is_empty() => format!(
            r#"<aside class="ses-chip ses-cmd"><span>{}</span><span class="ses-cmd__out"> → {}</span></aside>"#,
            esc(&label),
            esc(&truncate(out.trim(), 160))
        ),
        _ => format!(r#"<aside class="ses-chip ses-cmd">{}</aside>"#, esc(&label)),
    }
}

fn render_task_event(id: &str, subject: &str, transition: &str) -> String {
    let glyph = match transition {
        "completed" | "success" => "✓",
        "in_progress" => "◐",
        _ => "○",
    };
    let subject_html = if subject.is_empty() {
        String::new()
    } else {
        format!(" {}", esc(&truncate(subject, 90)))
    };
    format!(
        r#"<aside class="ses-chip ses-task">{glyph} #{}{} → {}</aside>"#,
        esc(id),
        subject_html,
        esc(transition)
    )
}

fn render_workflow_card(
    name: Option<&str>,
    description: Option<&str>,
    phases: &[String],
    task_id: Option<&str>,
) -> String {
    let title = name.unwrap_or("workflow");
    let desc_html = description
        .map(|d| format!("<p>{}</p>", esc(&truncate(d, 200))))
        .unwrap_or_default();
    let phases_html = if phases.is_empty() {
        String::new()
    } else {
        let items: String = phases
            .iter()
            .map(|p| format!("<li>{}</li>", esc(p)))
            .collect();
        format!("<ol class=\"ses-workflow__phases\">{items}</ol>")
    };
    let task_html = task_id
        .map(|t| format!(r#"<span class="ses-workflow__task">task {}</span>"#, esc(t)))
        .unwrap_or_default();
    format!(
        r#"<div class="ses-workflow"><header>⛭ {}{}</header>{desc_html}{phases_html}</div>"#,
        esc(title),
        task_html
    )
}

fn render_kb_command(verb: &str, args: &str, result_view: Option<&str>) -> String {
    let result_html = match result_view {
        Some(r) if !r.is_empty() => format!(" → {}", esc(r)),
        _ => String::new(),
    };
    format!(
        r#"<aside class="ses-chip ses-tool--kb">kb {} {}{}</aside>"#,
        esc(verb),
        esc(&truncate(args, 100)),
        result_html
    )
}

fn render_memory_injection(items: &[String]) -> String {
    let count = items.len();
    let rows: String = items
        .iter()
        .map(|i| format!("<li>{}</li>", esc(&truncate(i, 120))))
        .collect();
    format!(
        r#"<details class="ses-hook ses-memory-injection"><summary>◈ {count} memor{} recalled</summary><ul>{rows}</ul></details>"#,
        if count == 1 { "y" } else { "ies" }
    )
}

fn render_decision(prompt: &str, answer: Option<&str>) -> String {
    let variant = match prompt {
        "plan approved" => "approved",
        "tool use rejected" => "rejected",
        "permission denied" => "denied",
        _ => "answered",
    };
    let head = match variant {
        "approved" => "✓ you approved the plan",
        "rejected" => "✗ you rejected the tool use",
        "denied" => "⊘ permission denied",
        _ => "◆ you answered",
    };
    let body = match variant {
        "answered" => format!(
            "<dl><dt>{}</dt><dd>{}</dd></dl>",
            esc(prompt),
            esc(answer.unwrap_or(""))
        ),
        "denied" => answer
            .filter(|a| !a.is_empty())
            .map(|a| format!(r#"<p class="ses-decision__reason">{}</p>"#, esc(a)))
            .unwrap_or_default(),
        _ => String::new(),
    };
    format!(
        r#"<aside class="ses-decision ses-decision--{variant}"><span class="ses-decision__head">{}</span>{body}</aside>"#,
        head
    )
}

// ─── outcome footer (R3) ──────────────────────────────────────────────

/// `id="ses-outcome"` — the closing prose (uncapped), the final task board,
/// the commits footer (from the `kb-session-commits` tail block, dropped by
/// the pre-IR renderer entirely), and the subagents section (from
/// `kb-session-subagents`, also never rendered before). Absent when the
/// session has no [`Outcome`](view::Outcome) (a husk capture).
fn render_outcome_footer(sv: &SessionView) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        r#"<section class="ses-outcome" id="{}">"#,
        crate::sessions::SES_OUTCOME_ANCHOR
    ));
    out.push_str(r#"<h2 class="ses-outcome__head">outcome</h2>"#);

    if let Some(outcome) = &sv.header.outcome {
        out.push_str(r#"<div class="ses-outcome__closing">"#);
        out.push_str(&render_prose(&outcome.text));
        out.push_str("</div>");
        if let Some(r) = &outcome.stop_reason {
            out.push_str(&render_chip(&format!("stop · {r}")));
        }
        if !outcome.commits.is_empty() {
            out.push_str(r#"<div class="ses-outcome__commits"><h3>commits</h3><ul>"#);
            for c in &outcome.commits {
                let sha = c.sha.as_deref().unwrap_or("?");
                let subj = c.subject.as_deref().unwrap_or("(no subject recovered)");
                let status = if c.resolved {
                    "✓resolved"
                } else {
                    "unresolved"
                };
                out.push_str(&format!(
                    "<li><code>{}</code> {} <span class=\"ses-outcome__commit-status\">{}</span></li>",
                    esc(sha),
                    esc(&truncate(subj, 100)),
                    esc(status)
                ));
            }
            out.push_str("</ul></div>");
        }
    } else {
        out.push_str(r#"<p class="ses-outcome__none">∅ no assistant closure captured — this session may have ended mid-tool-call or on an empty capture.</p>"#);
    }

    if !sv.tasks_final.is_empty() {
        out.push_str(r#"<div class="ses-outcome__tasks"><h3>final task board</h3><ul>"#);
        for row in &sv.tasks_final {
            out.push_str(&render_task_event(&row.id, &row.subject, &row.status));
        }
        out.push_str("</ul></div>");
    }

    if !sv.subagents.is_empty() {
        out.push_str(r#"<div class="ses-outcome__subagents"><h3>subagents</h3><ul>"#);
        for a in &sv.subagents {
            let trunc = if a.truncated {
                r#" <span class="ses-outcome__truncated" title="sidecar text was capped">(truncated)</span>"#
            } else {
                ""
            };
            // R14 — the un-hide affordance's target: a deep link into the
            // raw page's hidden sidecar-text section, keyed on this agent.
            out.push_str(&format!(
                "<li><code>{}</code> — {} files · {} tokens · {} tool calls · {} errors{} · <a href=\"?raw=1#{}\">raw sidecar</a></li>",
                esc(&a.agent_id),
                a.files,
                a.tokens,
                a.tool_calls,
                a.errors,
                trunc,
                crate::sessions::SIDECAR_TEXT_BLOCK_ID,
            ));
        }
        out.push_str("</ul></div>");
    }

    out.push_str("<p class=\"ses-outcome__caveat\">captured at the last Stop — the session may have continued afterward.</p>");
    out.push_str("</section>\n");
    out
}

// ─── escape utilities ────────────────────────────────────────────────

fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

fn push_esc(out: &mut String, s: &str) {
    out.push_str(&esc(s));
}

fn push_attr(out: &mut String, s: &str) {
    out.push_str(&esc(s).replace('"', "&quot;"));
}

fn short_time(iso: &str) -> String {
    if iso.len() < 19 {
        return iso.to_string();
    }
    iso[11..19].to_string()
}

fn truncate(s: &str, max: usize) -> String {
    let trimmed = s.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max).collect();
    out.push('…');
    out
}

fn truncate_chars(s: &str, max: usize) -> String {
    truncate(s, max)
}

#[cfg(test)]
#[path = "session_render_tests.rs"]
mod tests;
