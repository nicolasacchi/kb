//! Artifact subdomain serving — `<id>.artifacts.localhost:4000`.
//!
//! Resolves the requested file under the kb's source folder, injects the
//! probe script via `kb_core::iframe::inject_script` if it's the entry-
//! point HTML, and serves with the appropriate Content-Type.
//!
//! v0.0.1 serves only the canon corpus path verbatim — multi-page artifacts
//! work because the parent HTML has relative links to siblings that resolve
//! under the same subdomain.

use crate::middleware::error_to_problem_json;
use crate::state::KbHandles;
use axum::{
    body::Body,
    extract::{ConnectInfo, State},
    http::{header, HeaderMap, HeaderValue, Response, StatusCode, Uri},
    response::IntoResponse,
};
use kb_core::iframe::{
    inject_annotator, inject_inline_head_script, inject_script, parse_artifact_host_id,
    ArtifactHostId,
};
use kb_core::review::{self, ReviewFile};
use kb_core::types::KbName;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const PROBE_PATH: &str = "/_kb/probe.js";
const ANNOTATE_PATH: &str = "/_kb/annotate.js";
/// v0.6+ H3 — inline runtime injected on every artifact serve.
/// Posts debounced scroll updates to the parent SPA and listens for
/// `kb:scroll-to` resume messages. See `runtime_js` for the source.
const RUNTIME_PATH: &str = "/_kb/runtime.js";

/// Default-dev `parent_origin` (`http://localhost:4000`). Treated as
/// the "not configured for production" sentinel — the postMessage
/// targetOrigin stays `'*'` and no `frame-ancestors` CSP is sent so
/// the common dev flows (SPA at 127.0.0.1, vite at :4738 proxying)
/// keep working. Self-hosters set `[server] parent_origin` to a real
/// origin to opt into the lockdown.
fn postmessage_target(origin_cfg: &crate::state::OriginConfig) -> &str {
    let p = origin_cfg.parent_origin.as_str();
    if p.is_empty() || p == kb_core::config::ServerSection::DEFAULT_PARENT_ORIGIN {
        "*"
    } else {
        p
    }
}

/// SPA origin a *top-level* artifact load should bounce to — used by the
/// per-page bounce in `serve` and the top-level branch of
/// `trampoline_response`. Returns the configured `parent_origin` in
/// production; an empty string in default-dev (where `postmessage_target`
/// is `'*'`), signalling the injected script to derive the origin from
/// `window.location` instead (the dev SPA port/host varies — 127.0.0.1,
/// vite proxy — so a hardcoded default would bounce to the wrong origin).
fn spa_origin_lit(origin_cfg: &crate::state::OriginConfig) -> &str {
    if postmessage_target(origin_cfg) == "*" {
        ""
    } else {
        origin_cfg.parent_origin.as_str()
    }
}

/// `Content-Security-Policy: frame-ancestors` value for artifact
/// subdomain responses. Returns `None` (header omitted) for the
/// default-dev `parent_origin` — see `postmessage_target` rationale.
/// When set: `frame-ancestors <parent_origin>;` so only the SPA the
/// daemon was configured for can iframe artifacts; hostile embedders
/// on other origins are refused by the browser.
fn artifact_csp_header(origin_cfg: &crate::state::OriginConfig) -> Option<HeaderValue> {
    let target = postmessage_target(origin_cfg);
    if target == "*" {
        return None;
    }
    HeaderValue::from_str(&format!("frame-ancestors {target};")).ok()
}

/// Attach the `Content-Security-Policy: frame-ancestors` header (if any)
/// to an artifact-subdomain response — kept as a one-liner so every
/// return path in `serve` and `trampoline_response` is uniformly
/// hardened. A `None` from `artifact_csp_header` (default-dev) leaves
/// the response unmodified.
fn with_artifact_csp(
    mut resp: Response<Body>,
    origin_cfg: &crate::state::OriginConfig,
) -> Response<Body> {
    if let Some(value) = artifact_csp_header(origin_cfg) {
        resp.headers_mut()
            .insert(header::CONTENT_SECURITY_POLICY, value);
    }
    resp
}

/// Tiny inline probe script — the Playwright iframe smoke test (phase 18)
/// loads this and `postMessage`s its results back to the parent. The exact
/// body matches what spike-iframe used; the targetOrigin is templated in
/// (`'*'` in default dev, the configured `parent_origin` in production).
fn probe_js(target_origin: &str) -> String {
    // JSON-encode so quotes/backslashes in a real origin can't break out.
    let target_lit = serde_json::to_string(target_origin).unwrap_or_else(|_| "\"*\"".to_string());
    format!(
        r#"// kb iframe probe — runs inside artifact subdomain
(function() {{
  const TARGET = {target_lit};
  const origin = window.location.origin;
  function report(check, ok, detail) {{
    try {{
      window.parent.postMessage({{ kind: 'kb-probe', origin, check, ok, detail }}, TARGET);
    }} catch (e) {{}}
  }}
  // 1. localStorage write
  try {{
    const key = 'kb-probe-' + Date.now();
    localStorage.setItem(key, 'hello');
    report('localStorage', true, key);
  }} catch (e) {{
    report('localStorage', false, String(e));
  }}
  // 2. parent.document access (must throw SecurityError)
  try {{
    void window.parent.document.body;
    report('cross_origin_parent_doc_access', false, 'unexpectedly readable');
  }} catch (e) {{
    report('cross_origin_parent_doc_access', true, 'SecurityError as expected');
  }}
  // 3. Notify the parent SPA of the current sub-page so multi-page
  // artifacts reflect internal navigation in the outer URL. Skip '/'
  // so the entrypoint never clobbers a path the parent already trusts.
  try {{
    var path = window.location.pathname;
    if (path && path !== '/') {{
      window.parent.postMessage({{
        kind: 'pm:page',
        label: path.replace(/^\//, ''),
        src: path
      }}, TARGET);
    }}
  }} catch (e) {{}}
}})();
"#
    )
}

/// v0.6+ H3 — scroll-reporter runtime. Inlined like `probe_js` so it
/// works without an SPA build. Two responsibilities:
///
/// 1. Listen for window `scroll`, debounce 500ms, post `kb:scroll` to
///    parent with the current `y` and the document's `scrollMax`. The
///    SPA throttles its server POSTs separately (debounced 1s) so
///    we're not aiming for low-latency here — just "captures the
///    settled position after the user stops".
///
/// 2. Listen for `message` from the parent; on `{kind:'kb:scroll-to', y}`,
///    call `scrollTo` inside a `requestAnimationFrame` so layout has
///    settled before the jump. The parent waits for the existing
///    `kb-probe` ready ping before posting, so DOMContentLoaded has
///    already fired by the time we land here.
///
/// `target_origin` mirrors `probe_js`: `'*'` in default-dev,
/// `parent_origin` in production. Cross-origin enforcement also
/// happens on the parent (origin allow-listing in detail.tsx).
fn runtime_js(target_origin: &str) -> String {
    let target_lit = serde_json::to_string(target_origin).unwrap_or_else(|_| "\"*\"".to_string());
    format!(
        r#"// kb iframe runtime — scroll capture + resume + TOC mini-spy (P3)
(function() {{
  const TARGET = {target_lit};
  let timer = null;
  function snapshot() {{
    const y = window.scrollY || document.documentElement.scrollTop || 0;
    const max = Math.max(
      document.documentElement.scrollHeight || 0,
      (document.body && document.body.scrollHeight) || 0
    );
    try {{
      window.parent.postMessage({{ kind: 'kb:scroll', y: y, max: max }}, TARGET);
    }} catch (e) {{}}
  }}
  // v0.12 P3 — emit a TOC of the artifact's headings + an active-
  // section indicator as the user scrolls. The SPA renders a tiny
  // mini-spy on the iframe overlay; click → posts 'kb:scroll-to-id'
  // back here to scroll the iframe.
  let toc = [];
  function buildToc() {{
    const out = [];
    const hs = document.querySelectorAll('h1, h2, h3');
    for (const h of hs) {{
      const text = (h.textContent || '').trim();
      if (!text) continue;
      // Ensure each heading has an id so kb:scroll-to-id works.
      if (!h.id) {{
        const slug = text.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/(^-|-$)/g, '');
        h.id = 'kb-h-' + (slug || ('h' + out.length));
      }}
      out.push({{
        id: h.id,
        text: text.length > 60 ? text.slice(0, 57) + '…' : text,
        level: Number(h.tagName.slice(1)) || 1,
        top: h.offsetTop,
      }});
    }}
    toc = out;
    buildMeta();
    try {{
      window.parent.postMessage({{ kind: 'kb:toc', toc: out }}, TARGET);
    }} catch (e) {{}}
  }}
  function activeSectionId() {{
    if (toc.length === 0) return null;
    const y = (window.scrollY || 0) + 80;
    let cur = toc[0].id;
    for (const h of toc) {{
      if (h.top <= y) cur = h.id;
      else break;
    }}
    return cur;
  }}
  let lastActive = null;
  function emitActive() {{
    const id = activeSectionId();
    if (id && id !== lastActive) {{
      lastActive = id;
      onSectionChange(id);
      try {{
        window.parent.postMessage({{ kind: 'kb:section', id: id }}, TARGET);
      }} catch (e) {{}}
    }}
  }}
  function onScroll() {{
    if (hoverA) clearHover();
    rResume();
    if (timer) clearTimeout(timer);
    timer = setTimeout(function() {{ snapshot(); emitActive(); }}, 500);
  }}
  window.addEventListener('scroll', onScroll, {{ passive: true }});
  // The 500ms debounce above can leave the parent's last-known offset
  // stale by up to that much when the page goes away; flush one final
  // `kb:scroll` (and drop any live peek) on pagehide, mirroring the
  // reading tracker's own pagehide flush below.
  window.addEventListener('pagehide', function() {{ snapshot(); clearHover(); }});

  // --- RP-track reading capture: per-section dwell + active reading time.
  // Stopwatch-with-pause — credit elapsed active time to the CURRENT section
  // on every boundary (section change, tab hidden, idle, pagehide), never
  // while paused. Values are CUMULATIVE per visit; the parent POSTs them to
  // /history/reading and the server max-merges. On a 30-min resume the parent
  // seeds us via 'kb:reading-seed' so the cumulative counters never regress.
  // Cross-origin: we can only postMessage the parent (it owns the POST and
  // the reliable final flush on iframe unmount). The full TOC is reported
  // every beacon (untouched sections at dwell 0 / enters 0) so the summary
  // can mark sections Unseen, not just rank the ones that were touched.
  var R_IDLE_MS = 30000, R_FLUSH_MS = 5000;
  var rDwell = Object.create(null);   // section_id -> cumulative active ms
  var rEnters = Object.create(null);  // section_id -> enter count
  var rMeta = Object.create(null);    // section_id -> {{idx,text,level,words,content_px}}
  var rActiveMs = 0;                  // cumulative active ms this visit
  var rCur = null;                    // current section id (the stop-point)
  var rTick = null;                   // ms at last accrual; null = paused
  var rIdleTimer = null, rSeeded = false;
  function rAccrue() {{
    if (rTick === null || document.hidden) return;
    var dt = Date.now() - rTick;
    if (dt > 0) {{
      if (rCur) rDwell[rCur] = (rDwell[rCur] || 0) + dt;
      rActiveMs += dt;
    }}
    rTick = Date.now();
  }}
  function rPause() {{ rAccrue(); rTick = null; }}
  function rResume() {{
    if (rTick === null && !document.hidden) rTick = Date.now();
    if (rIdleTimer) clearTimeout(rIdleTimer);
    rIdleTimer = setTimeout(rPause, R_IDLE_MS);
  }}
  function onSectionChange(id) {{
    rAccrue();                         // credit elapsed to the OLD section first
    rCur = id;
    if (id) rEnters[id] = (rEnters[id] || 0) + 1;
    if (rTick === null && !document.hidden) rTick = Date.now();
  }}
  function rWordsBetween(id, nextId) {{
    var el = document.getElementById(id);
    if (!el) return 0;
    var stop = nextId ? document.getElementById(nextId) : null;
    var words = 0, node = el.nextSibling, guard = 0;
    while (node && node !== stop && guard < 4000) {{
      guard++;
      var t = (node.textContent || '').trim();
      if (t) words += t.split(/\s+/).length;
      node = node.nextSibling;
    }}
    return words;
  }}
  function buildMeta() {{               // recomputed each buildToc (content_px settles on `load`)
    var docMax = Math.max(
      document.documentElement.scrollHeight || 0,
      (document.body && document.body.scrollHeight) || 0
    );
    for (var i = 0; i < toc.length; i++) {{
      var h = toc[i], next = toc[i + 1];
      var px = (next ? next.top : docMax) - h.top;
      var prev = rMeta[h.id];
      rMeta[h.id] = {{
        idx: i, text: h.text, level: h.level,
        words: (prev && prev.words) || rWordsBetween(h.id, next && next.id),
        content_px: px > 0 ? px : 0
      }};
    }}
  }}
  function snapshotReading() {{
    rAccrue();                         // settle the current section before reporting
    var sections = [];
    for (var id in rMeta) {{
      var m = rMeta[id];
      sections.push({{
        id: id, idx: m.idx, text: m.text, level: m.level,
        words: m.words || 0, content_px: m.content_px || 0,
        dwell_ms: Math.round(rDwell[id] || 0), enters: rEnters[id] || 0
      }});
    }}
    try {{
      window.parent.postMessage({{
        kind: 'kb:reading', sections: sections,
        active_ms: Math.round(rActiveMs), last_section: rCur
      }}, TARGET);
    }} catch (e) {{}}
  }}
  ['mousemove', 'keydown', 'pointerdown', 'wheel', 'touchstart'].forEach(function(t) {{
    window.addEventListener(t, rResume, {{ passive: true }});
  }});
  document.addEventListener('visibilitychange', function() {{
    if (document.hidden) {{ rPause(); snapshotReading(); }}   // primary flush
    else rResume();
  }});
  window.addEventListener('pagehide', function() {{ rPause(); snapshotReading(); }}); // best-effort
  setInterval(function() {{
    if (rTick !== null && !document.hidden) snapshotReading();
  }}, R_FLUSH_MS);
  // Build the TOC after a frame so most styles + image-driven offsets
  // have settled. A second emit on `load` covers late layout.
  if (document.readyState === 'loading') {{
    document.addEventListener('DOMContentLoaded', function() {{
      requestAnimationFrame(function() {{ buildToc(); emitActive(); }});
    }});
  }} else {{
    requestAnimationFrame(function() {{ buildToc(); emitActive(); }});
  }}
  window.addEventListener('load', function() {{ buildToc(); emitActive(); }});

  // Listen for parent's scroll-to resume message. requestAnimationFrame
  // defers one frame so layout has settled before scrollTo (the iframe
  // may not have hit its final scrollHeight at message-arrival time).
  window.addEventListener('message', function(ev) {{
    const data = ev && ev.data;
    if (!data) return;
    if (data.kind === 'kb:scroll-to') {{
      const y = Number(data.y) || 0;
      if (y <= 0) return;
      requestAnimationFrame(function() {{
        try {{ window.scrollTo({{ top: y }}); }} catch (e) {{ window.scrollTo(0, y); }}
      }});
      return;
    }}
    if (data.kind === 'kb:scroll-to-id') {{
      const id = String(data.id || '');
      if (!id) return;
      const el = document.getElementById(id);
      if (!el) return;
      requestAnimationFrame(function() {{
        try {{
          el.scrollIntoView({{ behavior: 'smooth', block: 'start' }});
        }} catch (e) {{
          window.scrollTo(0, el.offsetTop || 0);
        }}
        // RLs1 — section-permalink arrival flash (`?sec=` deep links and
        // reading-list trail hops set `flash: true`): one ~1.8s amber
        // wash + left-edge bar that holds ~600ms then fades. Web
        // Animations API only — no CSS injection, no persistent DOM
        // state, one flash per message.
        if (data.flash && el.animate) {{
          try {{
            el.animate(
              [
                {{ backgroundColor: 'rgba(255, 200, 87, 0.30)',
                   boxShadow: '-4px 0 0 0 rgba(255, 200, 87, 0.9)' }},
                {{ backgroundColor: 'rgba(255, 200, 87, 0.30)',
                   boxShadow: '-4px 0 0 0 rgba(255, 200, 87, 0.9)',
                   offset: 0.35 }},
                {{ backgroundColor: 'transparent', boxShadow: 'none' }}
              ],
              {{ duration: 1800, easing: 'ease-out' }}
            );
          }} catch (e) {{ /* Web Animations unavailable — scroll alone is fine */ }}
        }}
      }});
      return;
    }}
    // RP-track seed-on-open (F6): restore the visit's prior cumulative
    // reading state once, so beacons stay monotonic across a 30-min resume.
    if (data.kind === 'kb:reading-seed') {{
      if (rSeeded) return;
      rSeeded = true;
      rActiveMs = Number(data.active_ms) || 0;
      if (data.last_section) rCur = String(data.last_section);
      var ss = data.sections || [];
      for (var i = 0; i < ss.length; i++) {{
        var s = ss[i];
        if (!s || !s.id) continue;
        rDwell[s.id] = Number(s.dwell_ms) || 0;
        rEnters[s.id] = Number(s.enters) || 0;
      }}
      return;
    }}
  }});

  // Link-peek relay — a delegated mouseover/mouseout pair reports the
  // anchor under the pointer to the parent, which owns every decision
  // about what is peekable (this side stays dumb: no href-shape
  // filtering). `rect` is in iframe-viewport coords; the parent offsets
  // it by the frame's own box.
  var hoverA = null;
  function clearHover() {{
    if (!hoverA) return;
    hoverA = null;
    try {{
      window.parent.postMessage({{ kind: 'kb:link-clear' }}, TARGET);
    }} catch (e) {{}}
  }}
  document.addEventListener('mouseover', function(e) {{
    try {{
      var a = e.target && e.target.closest && e.target.closest('a[href]');
      if (!a || a === hoverA) return;
      var url;
      try {{ url = new URL(a.href, location.href); }} catch (err) {{ return; }}
      var r = a.getBoundingClientRect();
      var text = (a.textContent || '').trim();
      if (text.length > 120) text = text.slice(0, 120);
      hoverA = a;
      window.parent.postMessage({{
        kind: 'kb:link-hover',
        href: url.href,
        rect: {{ x: r.left, y: r.top, w: r.width, h: r.height }},
        text: text
      }}, TARGET);
    }} catch (err) {{}}
  }});
  document.addEventListener('mouseout', function(e) {{
    try {{
      if (!hoverA) return;
      var t = e.target;
      if (t !== hoverA && !(t && hoverA.contains && hoverA.contains(t))) return;
      var to = e.relatedTarget;
      if (to && hoverA.contains && hoverA.contains(to)) return;   // still inside
      clearHover();
    }} catch (err) {{}}
  }});

  // Artifact-shaped CROSS-origin links: a sibling artifact's own
  // subdomain (same host remainder, different first label — derived from
  // `location.host` at runtime so the configured `artifact_host_suffix`
  // is never hardcoded here, #7) or an SPA permalink `/a/<kb>/<rel>`.
  // Left to the browser these navigate the iframe out from under the
  // parent, desyncing every piece of SPA chrome; relayed as
  // `kb:link-open` the parent routes them like any other artifact open.
  var HOST_REM = (function() {{
    try {{
      var d = location.host.indexOf('.');
      return d > 0 ? location.host.slice(d) : '';
    }} catch (e) {{ return ''; }}
  }})();
  function isArtifactSubdomain(url) {{
    if (!HOST_REM || url.host === location.host) return false;
    var d = url.host.indexOf('.');
    return d > 0 && url.host.slice(d) === HOST_REM;
  }}
  function isSpaPermalink(url) {{
    if (!/^\/a\/[^/]+\/.+/.test(url.pathname)) return false;
    if (url.origin === location.origin) return true;
    return TARGET !== '*' && url.origin === TARGET;   // dev '*' -> native
  }}

  // kb back-button fix (A1) — a cross-artifact link makes THIS iframe do a
  // real PUSH navigation to the server trampoline, which pollutes the
  // browser's joint session history (the classic iframe back-trap: pressing
  // Back then walks the iframe's internal stack instead of the parent's
  // per-artifact route entries, often landing on the gallery). The sandbox
  // has no `allow-top-navigation`, so a SAME-FRAME `location.replace` is
  // allowed and REPLACES the iframe entry instead of pushing — the
  // trampoline -> `open-artifact` -> parent push (detail.tsx) then yields
  // exactly one clean browser-history entry per artifact, and the iframe is
  // re-keyed per id so no stale trampoline is restored. Bubble phase (not
  // capture) so an artifact's own click handler can preventDefault first
  // and keep custom navigation; hash-only links scroll natively;
  // modified / _blank / cross-origin clicks are left untouched (the
  // allow-popups-to-escape-sandbox + top-level bounce path is unchanged).
  document.addEventListener('click', function(e) {{
    if (e.defaultPrevented || e.button !== 0) return;
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    var a = e.target && e.target.closest && e.target.closest('a[href]');
    if (!a) return;
    var tgt = a.getAttribute('target');
    if (tgt && tgt !== '_self') return;
    if (a.hasAttribute('download')) return;
    var url;
    try {{ url = new URL(a.href, location.href); }} catch (err) {{ return; }}
    // A link to THIS very document (a bare '#hash', or the page itself) is
    // never a cross-document open — checked BEFORE the artifact-shape relay
    // below, not only in the same-origin block further down. An artifact
    // whose own served path happens to match the SPA permalink shape
    // (`/a/<kb>/<rel>` — reachable once the same-origin branch below has
    // taken the frame to a nested page) would otherwise have its internal
    // anchors relayed out as `kb:link-open` and lose native hash scrolling.
    var samePage = url.origin === location.origin
      && url.pathname === location.pathname
      && url.search === location.search;
    if (!samePage && (isArtifactSubdomain(url) || isSpaPermalink(url))) {{
      e.preventDefault();
      snapshot();                                      // true leave-point
      try {{
        window.parent.postMessage({{ kind: 'kb:link-open', href: url.href }}, TARGET);
      }} catch (err) {{}}
      return;
    }}
    if (url.origin !== location.origin) return;        // external -> native
    if (url.href === location.href) return;            // self / empty href
    // pure same-page #hash -> let the browser scroll natively
    if (samePage && url.hash) return;
    e.preventDefault();
    snapshot();                                        // true leave-point
    try {{ location.replace(url.href); }} catch (err) {{ location.href = url.href; }}
  }});
}})();
"#
    )
}

pub async fn serve(
    State(state): State<Arc<KbHandles>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    uri: Uri,
) -> Response<Body> {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let Some(host_id) = parse_artifact_host_id(host, &state.origin.artifact_host_suffix) else {
        return (StatusCode::NOT_FOUND, "not an artifact subdomain").into_response();
    };

    let raw_path = uri.path();
    if raw_path == PROBE_PATH {
        let target = postmessage_target(&state.origin);
        let mut resp = (StatusCode::OK, probe_js(target)).into_response();
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/javascript; charset=utf-8"),
        );
        return with_artifact_csp(resp, &state.origin);
    }
    if raw_path == RUNTIME_PATH {
        let target = postmessage_target(&state.origin);
        let mut resp = (StatusCode::OK, runtime_js(target)).into_response();
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/javascript; charset=utf-8"),
        );
        return with_artifact_csp(resp, &state.origin);
    }
    if raw_path == ANNOTATE_PATH {
        return with_artifact_csp(serve_annotate_js(&state), &state.origin);
    }

    // Resolve the file. For `/`, use `<id>.html`. For other paths, use the
    // path relative to the kb source folder. Path traversal is blocked by
    // canonicalising against the source root.
    //
    // Multi-kb dispatch: a BARE artifact subdomain doesn't encode the kb
    // name, only the id — `resolve_artifact_kb` walks the BTreeMap of kbs
    // (deterministic alphabetical order) and returns the first one whose
    // storage owns the (path-based) artifact id, OR — for legacy
    // file-stem ids — whose source tree contains `<id>.html`. Without
    // this, picking an arbitrary kb via `kbs.values().next()`
    // mis-resolves every artifact owned by the other kb. Pre-v0.7 bug. A
    // QUALIFIED subdomain (`{kb_enc}--{id}`, ARTIFACT HOST GRAMMAR v2)
    // names its kb explicitly — this disambiguates the case two kbs share
    // a source-relative path (and so hash to the SAME id): the bare-id
    // walk above would always pick the alphabetically-first owner for
    // every colliding id, silently mis-serving every other kb's copy.
    let Some((ctx, root_resolved, artifact_id)) = resolve_artifact_kb(&state, &host_id).await
    else {
        return (StatusCode::NOT_FOUND, "artifact not found").into_response();
    };
    let artifact_id = artifact_id.as_str();

    // `ctx.source_path` is already canonicalised once at daemon bring-up
    // (invariant #27; lib.rs canonicalises `kb_section.path`), so it IS the
    // canonical source root. No per-request `canonicalize()` syscall on this
    // hot path (every iframe + sibling-asset GET).
    let source_root = ctx.source_path.clone();

    // For path-id subdomains, `resolve_artifact_kb` returns the
    // resolved file path; use its parent as `asset_base` so sibling-asset
    // requests on the same subdomain resolve relative to the entrypoint's
    // own directory rather than the kb root. Without this, an artifact at
    // `<root>/foo/index.html` referencing `_assets/x.css` would 404 (the
    // daemon would look under `<root>/_assets/x.css`).
    let asset_base: PathBuf = match root_resolved.as_ref() {
        Some(p) => p
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| source_root.clone()),
        None => source_root.clone(),
    };

    let resolved = if raw_path == "/" {
        match root_resolved {
            Some(p) => p,
            None => {
                let candidate = asset_base.join(format!("{artifact_id}.html"));
                match candidate.canonicalize() {
                    Ok(p) => p,
                    Err(_) => {
                        return (StatusCode::NOT_FOUND, "artifact not found").into_response();
                    }
                }
            }
        }
    } else {
        let rel = raw_path.trim_start_matches('/');
        // Walk up from asset_base toward source_root, probing each
        // level for `rel`. First hit wins so a nearer `_assets/`
        // shadows a further-up one — matches author intent for
        // shared assets in the nearest ancestor folder, and lets a
        // chapter at depth ≥ 2 (e.g. `foo/steps/INDEX.html` with
        // `<link href="_assets/x.css">`) pick up `foo/_assets/x.css`.
        // Ok-but-escape at any level is rejected as path traversal
        // (matches the `escape.txt` regression test at every depth);
        // Err (no file at this level) walks to the parent. The
        // `starts_with(&source_root)` guard stays the trust boundary
        // — walk-up only changes WHERE the resolver looks, never
        // WHAT is reachable beyond source_root.
        let mut probe = asset_base.clone();
        let canon = loop {
            match probe.join(rel).canonicalize() {
                Ok(p) if p.starts_with(&source_root) => break p,
                Ok(_) => {
                    let err = kb_core::Error::BadRequest("path traversal".into());
                    return error_to_problem_json(&err);
                }
                Err(_) => {}
            }
            if probe == source_root {
                return (StatusCode::NOT_FOUND, "artifact not found").into_response();
            }
            match probe.parent() {
                Some(p) => probe = p.to_path_buf(),
                None => {
                    return (StatusCode::NOT_FOUND, "artifact not found").into_response();
                }
            }
        };
        // Whichever branch produced `canon`, check whether the file is
        // itself a DIFFERENT indexed artifact. If so, bubble navigation
        // up to the parent SPA via a trampoline that postMessages
        // `open-artifact` — the iframe re-mounts at the target's own
        // subdomain so origin isolation is preserved. Skipping this
        // check on the asset_base branch was the cohabiting-siblings
        // bug: a same-folder link to another indexed artifact would be
        // served normally, the probe would fire `pm:page`, and the
        // parent SPA would mis-route the URL to `?p=<sibling>` of the
        // *current* artifact instead of switching subdomains.
        // Errors from `get_by_source_path` fall through to serve the
        // file (matches pre-fix behaviour on the fallback branch — a
        // storage hiccup shouldn't 5xx a static asset).
        let path_str = canon.to_string_lossy().to_string();
        if let Ok(Some(target)) = ctx.storage.get_by_source_path(path_str).await {
            if target.id != artifact_id {
                let target_rel = kb_core::paths::doc_rel_path(&target.path, &source_root);
                return trampoline_response(&ctx.kb_name, &target.id, &target_rel, &state.origin);
            }
        }
        canon
    };
    if !resolved.starts_with(&source_root) {
        let err = kb_core::Error::BadRequest("path traversal".into());
        return error_to_problem_json(&err);
    }

    let bytes = match std::fs::read(&resolved) {
        Ok(b) => b,
        Err(e) => {
            return (StatusCode::NOT_FOUND, format!("read failed: {e}")).into_response();
        }
    };

    let content_type = super::guess_content_type(&resolved);
    // Markdown artifacts render to a full HTML page HERE — BEFORE the scrub —
    // so the inline `<template id="kb-prompt">` strip and the probe/runtime/
    // annotator injections all run over the SAME DOM the indexer saw. Markdown
    // is served ONLY from this CSP-wrapped, scrubbed `text/html` branch; it
    // never falls through to the raw-bytes branch below (we don't route it via
    // `guess_content_type`, which would label a mapped extension octet-stream).
    // SC5 — reads `ctx.ext_map` (the SAME per-kb resolved map `index_file` used
    // to pick the parse pipeline), not the hardcoded `.md`/`.markdown` extension
    // check: a `[indexer.indexable_extensions]` mapping (e.g. `txt = "markdown"`)
    // now renders here instead of falling through to raw bytes.
    let is_md = ctx.ext_map.is_markdown(&resolved);
    if is_md || content_type == "text/html; charset=utf-8" {
        let src = match std::str::from_utf8(&bytes) {
            Ok(s) => s,
            Err(_) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, "non-utf8 artifact").into_response();
            }
        };
        let rendered_md: String;
        let html: &str = if is_md {
            rendered_md = kb_core::markdown::render_page(src);
            &rendered_md
        } else {
            src
        };

        // v0.3 G3 — outbound scrubbing. Runs FIRST so probe/annotator
        // injection doesn't accidentally leak through to the scrubbed
        // bytes (the probe is dev infrastructure; we don't want it
        // stripped, but we DO want kb-prompt + PII gone before any
        // post-processing). Triggered only on non-loopback origins,
        // gated by the per-kb outbound config.
        let scrubbed_owned: String;
        let scrubbed_html: &str = match (
            &ctx.outbound,
            crate::scrub::looks_non_loopback(
                Some(peer.ip()),
                &headers,
                &state.origin.trusted_proxies,
            ),
        ) {
            (Some(cache), true) => {
                scrubbed_owned = crate::scrub::scrub(html, cache);
                &scrubbed_owned
            }
            _ => html,
        };

        // v0.14+ — captured Stop-hook transcripts (kb-category=memory-session)
        // are stored as raw JSONL wrapped in <pre> for losslessness. Apply
        // the session_render transform here so the reader gets structured
        // HTML (user/assistant cards, collapsible tool cards) instead of a
        // wall of JSON. Pure HTML→HTML; downstream probe/runtime/annotator
        // injections still apply.
        // A5 — `?raw=1` skips ONLY the session_render transform, serving the
        // (still-scrubbed) raw <pre> JSONL transcript. The scrub above already
        // ran, so raw NEVER bypasses the outbound kb-prompt strip (#5/#9).
        let rendered_owned: String;
        let working_html: &str = if wants_raw(&uri) {
            scrubbed_html
        } else {
            match kb_core::session_render::render_if_session(scrubbed_html, ctx.kb_name.as_str()) {
                Some(rendered) => {
                    rendered_owned = rendered;
                    &rendered_owned
                }
                None => scrubbed_html,
            }
        };

        // First inject the probe (preserves v0.0.1 behaviour). Then
        // the v0.6+ H3 runtime (scroll capture + resume — unconditional,
        // same shape as the probe). Then, when the SPA toggled annotate
        // mode, inject the v0.2 annotator (data block + deferred script).
        // Three passes keeps each helper single-purpose; defer ordering
        // preserves run order (probe → runtime → annotator).
        let probed = match inject_script(working_html, PROBE_PATH) {
            Ok(s) => s,
            Err(e) => return error_to_problem_json(&e),
        };
        let injected = match inject_script(&probed, RUNTIME_PATH) {
            Ok(s) => s,
            Err(e) => return error_to_problem_json(&e),
        };
        let cm_on = wants_annotator(&uri);
        let with_annotator = if cm_on {
            match build_comments_payload(&state, ctx, artifact_id) {
                Ok(payload) => match inject_annotator(
                    &injected,
                    &payload,
                    ANNOTATE_PATH,
                    postmessage_target(&state.origin),
                ) {
                    Ok(s) => s,
                    Err(e) => return error_to_problem_json(&e),
                },
                Err(e) => return error_to_problem_json(&e),
            }
        } else {
            injected
        };

        // Top-level bounce: when this page is opened as its own tab (e.g.
        // ctrl/⌘/middle-click on a link inside an artifact iframe) rather
        // than embedded in the kb SPA, redirect to the kb wrapper so the
        // new tab gets chrome/comments/history. `window.top !== self`
        // skips it when framed; the `.artifacts.` referrer gate keeps
        // directly-typed / externally-shared raw artifact links raw. KB +
        // REL are templated raw and encoded in JS exactly like the SPA's
        // `artifactHref`, so the permalink matches what the SPA resolves.
        let rel = kb_core::paths::doc_rel_path(&resolved.to_string_lossy(), &source_root);
        let kb_json = serde_json::to_string(ctx.kb_name.as_str()).unwrap_or_else(|_| "\"\"".into());
        let rel_json = serde_json::to_string(&rel).unwrap_or_else(|_| "\"\"".into());
        let spa_json =
            serde_json::to_string(spa_origin_lit(&state.origin)).unwrap_or_else(|_| "\"\"".into());
        let bounce_js = format!(
            "(function(){{\
               if(window.top!==window.self)return;\
               try{{var r=document.referrer;if(!r||new URL(r).hostname.indexOf('.artifacts.')<0)return;}}catch(e){{return;}}\
               var SPA={spa_json}||(location.protocol+'//'+location.host.replace(/^[^.]+\\.artifacts\\./,''));\
               var KB={kb_json},REL={rel_json};\
               location.replace(SPA+'/a/'+encodeURIComponent(KB)+'/'+REL.split('/').map(encodeURIComponent).join('/'));\
             }})();"
        );
        let with_bounce = match inject_inline_head_script(&with_annotator, &bounce_js) {
            Ok(s) => s,
            Err(e) => return error_to_problem_json(&e),
        };

        let mut resp = (StatusCode::OK, with_bounce).into_response();
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        resp.headers_mut().insert(
            "X-Kb-Artifact-Id",
            HeaderValue::from_str(artifact_id).unwrap_or(HeaderValue::from_static("?")),
        );
        // Cache headers: plain bytes vary on Accept/Cookie; ?cm=on
        // bytes additionally vary on Authorization (v0.4 B2 — prevents
        // a reverse proxy from leaking one user's comments to another)
        // and force private, no-store so no proxy ever caches the
        // user-specific comment payload.
        if cm_on {
            resp.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, no-store"),
            );
            resp.headers_mut().insert(
                header::VARY,
                HeaderValue::from_static("Authorization, Accept, Cookie"),
            );
        } else {
            resp.headers_mut()
                .insert(header::VARY, HeaderValue::from_static("Accept, Cookie"));
        }
        with_artifact_csp(resp, &state.origin)
    } else {
        let mut resp = (StatusCode::OK, bytes).into_response();
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_str(content_type)
                .unwrap_or(HeaderValue::from_static("application/octet-stream")),
        );
        with_artifact_csp(resp, &state.origin)
    }
}

/// Serve `/_kb/annotate.js` from the SPA bundle. The annotator is built
/// by Vite as a separate entry; we resolve it dynamically so the alias
/// path stays stable across content-hashed bundle filenames.
///
/// Resolution order (first hit wins):
///   1. `<spa_dist>/annotate.js` — non-hashed direct entry
///   2. `<spa_dist>/assets/annotate-*.js` — hashed Vite output
///
/// Returns 404 problem+json when no SPA bundle is configured (cargo
/// test runs without `npm run build` — fine; the iframe just won't
/// have annotations until the SPA is built).
fn serve_annotate_js(state: &Arc<KbHandles>) -> Response<Body> {
    let Some(spa_dist) = state.spa_dist.as_deref() else {
        return (
            StatusCode::NOT_FOUND,
            "annotator script unavailable — SPA not built",
        )
            .into_response();
    };
    let direct = spa_dist.join("annotate.js");
    let path = if direct.is_file() {
        direct
    } else if let Some(p) = find_hashed_annotator(spa_dist) {
        p
    } else {
        return (StatusCode::NOT_FOUND, "annotate.js not found in SPA dist").into_response();
    };
    match std::fs::read(&path) {
        Ok(bytes) => {
            let mut resp = (StatusCode::OK, bytes).into_response();
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/javascript; charset=utf-8"),
            );
            // Short cache: the alias /_kb/annotate.js may swap to a new
            // bundle on rebuild, but iframes hit it for hours of a single
            // session. 1h max-age trades freshness for throughput.
            resp.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=3600"),
            );
            resp
        }
        Err(e) => (StatusCode::NOT_FOUND, format!("read annotate.js: {e}")).into_response(),
    }
}

fn find_hashed_annotator(spa_dist: &Path) -> Option<PathBuf> {
    let assets = spa_dist.join("assets");
    // `read_dir` order is OS-defined; if multiple `annotate-*.js` linger
    // (a stale bundle beside a fresh one) pick deterministically — the
    // lexicographically-greatest name — not whatever the FS yielded last.
    std::fs::read_dir(&assets)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("annotate-") && n.ends_with(".js"))
        })
        .max()
}

/// True iff the URL has `?cm=on` (or any other truthy `cm=`) in the query string.
fn wants_annotator(uri: &Uri) -> bool {
    let Some(q) = uri.query() else {
        return false;
    };
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .any(|(k, v)| k == "cm" && matches!(v, "on" | "1" | "true"))
}

/// A5 — `?raw=1` (or `raw=true`) opts out of the session_render transform so
/// the reader can see / download the raw JSONL transcript. Scrub still applies.
fn wants_raw(uri: &Uri) -> bool {
    let Some(q) = uri.query() else {
        return false;
    };
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .any(|(k, v)| k == "raw" && matches!(v, "1" | "true" | "on"))
}

/// Build the kb-comments/1 payload to inline. If a review file already
/// exists for this artifact, load it; otherwise emit an empty skeleton
/// so the annotator always sees a well-formed `window.__KB_COMMENTS`.
fn build_comments_payload(
    state: &Arc<KbHandles>,
    ctx: &crate::state::KbContext,
    artifact_id: &str,
) -> kb_core::Result<serde_json::Value> {
    let kb_name: KbName = ctx.kb_name.clone();
    let path = state.paths.kb_review_file(&kb_name, artifact_id);
    let etag = review::etag_for(&path)?;
    let file = review::load(&path)?
        .unwrap_or_else(|| ReviewFile::empty_skeleton(&kb_name, artifact_id, ""));
    Ok(serde_json::json!({
        "v": 1,
        "etag": etag,
        "file": file,
    }))
}

/// Helper retained for future code that may need a typed PathBuf result.
#[allow(dead_code)]
fn artifact_path(source: &Path, id: &str) -> PathBuf {
    source.join(format!("{id}.html"))
}

/// Cross-artifact navigation trampoline. When a relative link inside an
/// artifact resolves (under the kb's source root) to a file that's itself
/// an indexed artifact, this body postMessages `open-artifact` to the
/// parent SPA so the outer route changes to `/a/<kb>/<target_id>` — the
/// iframe re-mounts at the target's own subdomain, preserving origin
/// isolation (cookies, localStorage, comments keyed on the target id).
///
/// A 302 redirect won't do the job: redirecting to `<target>.artifacts.localhost/`
/// swaps the iframe content but leaves the parent SPA URL on the wrong
/// artifact (wrong floating-pill, wrong comments). Redirecting to the
/// parent's SPA URL renders the SPA *inside* the iframe. PostMessage out
/// is the only clean way to bubble navigation up.
///
/// Returns plain `text/html`; no probe/annotator/scrub passes — the body
/// is one script, none of the v0.3 G3 leakage vectors apply.
///
/// The link's `#fragment` never reaches the server (the runtime's
/// `location.replace` carries it onto the trampoline URL client-side), so
/// this is the only place it can be recovered: the script reads
/// `location.hash` and forwards it as the OPTIONAL `sec` field on the
/// `open-artifact` payload — omitted when there is no fragment.
fn trampoline_response(
    kb_name: &KbName,
    target_id: &str,
    target_rel: &str,
    origin_cfg: &crate::state::OriginConfig,
) -> Response<Body> {
    let body = trampoline_body(kb_name, target_id, target_rel, origin_cfg);
    let mut resp = (StatusCode::OK, body).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp.headers_mut().insert(
        "X-Kb-Trampoline",
        HeaderValue::from_str(target_id).unwrap_or(HeaderValue::from_static("?")),
    );
    with_artifact_csp(resp, origin_cfg)
}

/// The trampoline's HTML body — split out from `trampoline_response` so the
/// inline tests can assert on the script the way `runtime_js` is asserted on.
fn trampoline_body(
    kb_name: &KbName,
    target_id: &str,
    target_rel: &str,
    origin_cfg: &crate::state::OriginConfig,
) -> String {
    let kb_json = serde_json::to_string(kb_name.as_str()).unwrap_or_else(|_| "\"\"".into());
    let id_json = serde_json::to_string(target_id).unwrap_or_else(|_| "\"\"".into());
    // Track U — the parent SPA navigates to the path-based permalink
    // `/a/<kb>/<path>`, so carry the target's source-relative path too.
    let path_json = serde_json::to_string(target_rel).unwrap_or_else(|_| "\"\"".into());
    // postMessage targetOrigin: `'*'` for default-dev `parent_origin`,
    // the configured origin in production. JSON-encoded so a real
    // origin's quotes/backslashes can't break out of the script.
    let target_json =
        serde_json::to_string(postmessage_target(origin_cfg)).unwrap_or_else(|_| "\"*\"".into());
    // SPA origin for the top-level branch (empty in dev → derive from
    // location). Shared with the per-page bounce in `serve`.
    let spa_json =
        serde_json::to_string(spa_origin_lit(origin_cfg)).unwrap_or_else(|_| "\"\"".into());
    // Match the parent SPA's neutral background so the iframe doesn't
    // flash white during the parent-route re-mount. When loaded as a
    // top-level tab (ctrl/⌘/middle-click on a cross-folder link) there is
    // no parent SPA to receive the postMessage, so redirect to the kb
    // wrapper instead; when framed, postMessage out as before.
    format!(
        "<!doctype html><meta charset=\"utf-8\"><title>opening…</title>\
         <style>html,body{{background:#0d0d10;margin:0;height:100%;}}</style>\
         <script>(function(){{\
           var KB={kb_json},REL={path_json};\
           if(window.top===window.self){{\
             var SPA={spa_json}||(location.protocol+'//'+location.host.replace(/^[^.]+\\.artifacts\\./,''));\
             location.replace(SPA+'/a/'+encodeURIComponent(KB)+'/'+REL.split('/').map(encodeURIComponent).join('/'));\
           }}else{{\
             var M={{kind:\"open-artifact\",kb:KB,id:{id_json},path:REL}};\
             var SEC=(location.hash||'').slice(1);\
             if(SEC)M.sec=SEC;\
             parent.postMessage(M,{target_json});\
           }}\
         }})();</script>"
    )
}

/// Heuristic: a real artifact id is 12 lowercase-hex chars
/// (sha256-prefix-12 per `kb_core::ids` — v0.7.1 note: a hash of the
/// source-relative *path*, not the content). Anything else (e.g. the
/// test corpus's `kitchen-sink`) is treated as a legacy file stem.
fn looks_like_artifact_id(id: &str) -> bool {
    id.len() == 12 && id.chars().all(|c| c.is_ascii_hexdigit())
}

/// Resolve an artifact id to the file path stored alongside it in
/// lance. Used when the SPA loads `/a/{kb}/{id}` → iframe at
/// `<id>.artifacts.localhost` and the id doesn't match any source
/// filename.
async fn resolve_by_id(
    ctx: &crate::state::KbContext,
    id: &str,
    source_root: &Path,
) -> Option<PathBuf> {
    let row = ctx.storage.get_by_id(id.to_string()).await.ok().flatten()?;
    let candidate = PathBuf::from(&row.path);
    let resolved = candidate.canonicalize().ok()?;
    if !resolved.starts_with(source_root) {
        return None;
    }
    Some(resolved)
}

/// Find which kb owns a BARE `artifact_id` (a raw label — 12-hex id or
/// legacy file stem) and resolve it to a file path. The pre-v2
/// `resolve_artifact_kb` in full, kept as its own function so the v2
/// `Qualified` branch can fall back into it (see `resolve_artifact_kb`).
///
/// The daemon walks all configured kbs in `KbHandles.kbs` (`BTreeMap`,
/// alphabetical) and stops at the first one that claims the id/stem. Two
/// shapes are recognised:
///
/// - **Artifact ids** (12 ascii-hex chars — a hash of the source-
///   relative path): the kb's storage owns the id iff `get_by_id`
///   returns a row pointing at a file inside its source root.
/// - **Legacy file-stem ids** (e.g. `kitchen-sink` for the test
///   corpus): the kb owns the id iff `<source_root>/<id>.html` exists.
///
/// Returns `(ctx, root_resolved)`. `root_resolved` is `Some` only on
/// the path-id branch — the serve loop uses it for `asset_base`
/// derivation; for file-stem ids the caller falls back to
/// `<source_root>/<id>.html` at request time.
async fn resolve_bare<'a>(
    state: &'a crate::state::KbHandles,
    artifact_id: &str,
) -> Option<(&'a crate::state::KbContext, Option<PathBuf>)> {
    // `ctx.source_path` is already canonical (invariant #27), so use it
    // directly as the source root instead of a per-kb `canonicalize()`
    // syscall per request. The per-request existence checks below
    // (`resolve_by_id`'s row-path canonicalize, and the `<id>.html`
    // candidate canonicalize) necessarily stay — they resolve a specific
    // file that must exist.
    if looks_like_artifact_id(artifact_id) {
        for ctx in state.kbs.values() {
            if let Some(resolved) = resolve_by_id(ctx, artifact_id, &ctx.source_path).await {
                return Some((ctx, Some(resolved)));
            }
        }
        None
    } else {
        for ctx in state.kbs.values() {
            let candidate = ctx.source_path.join(format!("{artifact_id}.html"));
            if candidate.canonicalize().is_ok() {
                return Some((ctx, None));
            }
        }
        None
    }
}

/// Find which kb owns a parsed [`ArtifactHostId`] and resolve it to a file
/// path — the ARTIFACT HOST GRAMMAR v2 entry point for `serve`.
///
/// - **`Bare`** — delegates to [`resolve_bare`] unchanged (pre-v2
///   behaviour, byte-identical).
/// - **`Qualified { kb_enc, id }`** — walks `state.kbs` (`BTreeMap`,
///   alphabetical) for the FIRST kb whose name encodes
///   (`kb_core::iframe::encode_kb_name`) to `kb_enc` AND whose storage
///   resolves `id` (both conditions on the SAME kb — a name match whose
///   storage doesn't own the id keeps walking, it isn't an early return).
///   A miss (no kb encodes to `kb_enc`, or none of the name-matches own
///   the id) falls back to treating the reconstructed FULL label
///   (`{kb_enc}--{id}`) as a bare id through [`resolve_bare`] — honest:
///   this will normally 404, but preserves the pathological case of a
///   legacy file-stem id that happens to contain `--`.
///
/// Returns `(ctx, root_resolved, artifact_id)` — `artifact_id` is the
/// label the REST of `serve` should use downstream (headers, the
/// comments-payload lookup, the `<id>.html` fallback join): the plain
/// hex id on the `Qualified` (and its fallback) path, the bare label
/// unchanged on the `Bare` path.
async fn resolve_artifact_kb<'a>(
    state: &'a crate::state::KbHandles,
    host_id: &ArtifactHostId,
) -> Option<(&'a crate::state::KbContext, Option<PathBuf>, String)> {
    match host_id {
        ArtifactHostId::Bare(label) => resolve_bare(state, label)
            .await
            .map(|(ctx, root)| (ctx, root, label.clone())),
        ArtifactHostId::Qualified { kb_enc, id } => {
            for (name, ctx) in state.kbs.iter() {
                if &kb_core::iframe::encode_kb_name(name.as_str()) != kb_enc {
                    continue;
                }
                if let Some(resolved) = resolve_by_id(ctx, id, &ctx.source_path).await {
                    return Some((ctx, Some(resolved), id.clone()));
                }
            }
            let legacy_label = format!("{kb_enc}--{id}");
            resolve_bare(state, &legacy_label)
                .await
                .map(|(ctx, root)| (ctx, root, legacy_label))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::OriginConfig;

    fn origin_with_parent(parent: &str) -> OriginConfig {
        OriginConfig {
            parent_origin: parent.to_string(),
            ..OriginConfig::default()
        }
    }

    #[test]
    fn wants_raw_parses_query_flag() {
        let p = |q: &str| wants_raw(&q.parse::<Uri>().unwrap());
        assert!(p("/a.html?raw=1"));
        assert!(p("/a.html?raw=true"));
        assert!(p("/a.html?cm=on&raw=1"));
        assert!(!p("/a.html"));
        assert!(!p("/a.html?raw=0"));
        assert!(!p("/a.html?raw"));
        // Must not collide with the annotator flag.
        assert!(!p("/a.html?cm=1"));
    }

    #[test]
    fn default_dev_parent_origin_keeps_postmessage_permissive() {
        let cfg = origin_with_parent(kb_core::config::ServerSection::DEFAULT_PARENT_ORIGIN);
        assert_eq!(postmessage_target(&cfg), "*");
        assert!(artifact_csp_header(&cfg).is_none());
        // Probe script falls back to TARGET = "*" when default-dev.
        let js = probe_js(postmessage_target(&cfg));
        assert!(js.contains("const TARGET = \"*\""));
    }

    #[test]
    fn empty_parent_origin_also_permissive() {
        let cfg = origin_with_parent("");
        assert_eq!(postmessage_target(&cfg), "*");
        assert!(artifact_csp_header(&cfg).is_none());
    }

    #[test]
    fn configured_parent_origin_locks_down_postmessage_and_csp() {
        let cfg = origin_with_parent("https://kb.example.com");
        assert_eq!(postmessage_target(&cfg), "https://kb.example.com");
        let csp = artifact_csp_header(&cfg).expect("CSP set for production parent");
        assert_eq!(
            csp.to_str().unwrap(),
            "frame-ancestors https://kb.example.com;"
        );
        let js = probe_js(postmessage_target(&cfg));
        assert!(js.contains("const TARGET = \"https://kb.example.com\""));
    }

    #[test]
    fn runtime_intercepts_links_with_replace_not_push() {
        // A1 back-button fix: the runtime must intercept link clicks and
        // navigate via location.replace (so the cross-artifact trampoline
        // trip can't pollute joint session history), while leaving
        // same-page #hash links to scroll natively.
        let js = runtime_js("*");
        assert!(js.contains("addEventListener('click'"));
        assert!(js.contains("location.replace"));
        assert!(js.contains("url.hash"));
        // It must NOT hijack modified / new-tab clicks.
        assert!(js.contains("e.metaKey"));
        assert!(js.contains("url.origin !== location.origin"));
    }

    #[test]
    fn runtime_flashes_section_on_scroll_to_id() {
        // RLs1 — `?sec=` deep links and reading-list trail hops ask for a
        // one-shot arrival highlight via `flash: true`; the runtime
        // honours it with the Web Animations API (guarded — a missing
        // el.animate degrades to scroll-only).
        let js = runtime_js("*");
        assert!(js.contains("data.flash"));
        assert!(js.contains("el.animate"));
    }

    #[test]
    fn runtime_captures_reading_dwell_and_active_time() {
        let js = runtime_js("*");
        // per-section dwell beacon + section-change accrual
        assert!(js.contains("kb:reading"));
        assert!(js.contains("snapshotReading"));
        assert!(js.contains("onSectionChange"));
        // stopwatch-with-pause: idle + hidden-tab exclusion
        assert!(js.contains("visibilitychange"));
        assert!(js.contains("R_IDLE_MS"));
        // seed-on-open resume restore
        assert!(js.contains("kb:reading-seed"));
    }

    #[test]
    fn runtime_snapshots_scroll_before_navigating_away() {
        // The debounced kb:scroll can be up to 500ms stale; both
        // navigating branches of the interceptor must post the snapshot
        // synchronously BEFORE they hand the page over, so the parent's
        // last-known offset (and the server's history row) records the
        // true leave-point.
        //
        // Both index probes below anchor on the CALL `location.replace(` —
        // never the bare identifier, which any prose comment in the
        // interceptor would also match and silently invert the ordering
        // assertion (it did, once).
        let js = runtime_js("*");
        let post = js.find("kind: 'kb:scroll'").expect("scroll payload");
        let replace = js.find("location.replace(").expect("replace kept");
        assert!(post < replace, "kb:scroll payload must precede the nav");
        let click = js
            .find("addEventListener('click'")
            .expect("click interceptor");
        let tail = &js[click..];
        let snap = tail.find("snapshot();").expect("pre-nav snapshot call");
        let tail_replace = tail.find("location.replace(").expect("replace kept");
        assert!(snap < tail_replace, "snapshot() must precede replace");
        // …and a final flush when the page goes away.
        assert!(
            js.contains("addEventListener('pagehide', function() { snapshot(); clearHover(); }")
        );
    }

    #[test]
    fn runtime_relays_artifact_shaped_cross_origin_links() {
        // Artifact siblings live on their own subdomain; left native they
        // navigate the iframe out from under the SPA. The host suffix is
        // derived from location.host at runtime (#7 — never hardcoded).
        let js = runtime_js("*");
        assert!(js.contains("kb:link-open"));
        assert!(js.contains("location.host.indexOf('.')"));
        assert!(js.contains("url.host.slice(d) === HOST_REM"));
        assert!(js.contains("isArtifactSubdomain(url) || isSpaPermalink(url)"));
        // SPA permalinks only relay when the postMessage target is a real
        // origin (or same-origin) — dev's '*' keeps native behaviour.
        assert!(js.contains("TARGET !== '*' && url.origin === TARGET"));
        // A same-document link (bare '#hash' / the page itself) is excluded
        // BEFORE the shape test, so an artifact served at a path that looks
        // like `/a/<kb>/<rel>` keeps native hash scrolling.
        let same = js.find("var samePage").expect("samePage guard");
        let relay = js
            .find("!samePage && (isArtifactSubdomain(url)")
            .expect("relay branch");
        assert!(same < relay, "samePage must be computed before the relay");
    }

    #[test]
    fn runtime_relays_link_hover_and_clear() {
        let js = runtime_js("*");
        assert!(js.contains("kb:link-hover"));
        assert!(js.contains("kb:link-clear"));
        assert!(js.contains("addEventListener('mouseover'"));
        assert!(js.contains("addEventListener('mouseout'"));
        // rect (iframe-viewport coords) + trimmed, capped anchor text
        assert!(js.contains("getBoundingClientRect"));
        assert!(js.contains("rect: { x: r.left, y: r.top, w: r.width, h: r.height }"));
        assert!(js.contains("text.slice(0, 120)"));
        // a scroll while a hover is live clears it once (hoverA is the guard)
        assert!(js.contains("if (hoverA) clearHover();"));
    }

    #[test]
    fn trampoline_forwards_fragment_as_sec() {
        // location.replace keeps the '#frag' client-side (the server never
        // sees it), so the trampoline script is the only place it can be
        // recovered — read from location.hash, attached OPTIONALLY.
        let kb = KbName::new("canon").unwrap();
        let body = trampoline_body(
            &kb,
            "abc123abc123",
            "a/b.html",
            &origin_with_parent("https://kb.example.com"),
        );
        assert!(body.contains("location.hash"));
        assert!(body.contains("M.sec=SEC"));
        assert!(body.contains("if(SEC)"), "sec omitted when no fragment");
        // The existing payload shape is untouched.
        assert!(body.contains(r#"kind:"open-artifact""#));
        assert!(body.contains("abc123abc123"));
        assert!(body.contains("parent.postMessage(M,\"https://kb.example.com\")"));
    }
}
