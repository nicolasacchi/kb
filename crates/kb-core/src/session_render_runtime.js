// Session-transcript runtime — v2 nav core (sessions-rethink W1: minimap +
// stable t-<uuid12> anchors replace the old #turn-N jump rail).
// Filter pills + tool search + minimap click/cursor tracking + the ↧ result
// FAB + expand/collapse all. Vanilla DOM only; loaded by the renderer as an
// inline <script> at end of <body>. Survives static export (kb share)
// because everything is self-contained.

(function () {
  const root = document.body;
  const STORAGE_KEY = "kb-ses-v2";
  const FILTER_FLAGS = ["user", "assistant", "tools", "thinking", "system", "hooks", "debug"];
  const DEFAULTS = { user: 1, assistant: 1, tools: 1, thinking: 0, system: 1, hooks: 0, debug: 0, search: "" };

  // ── state load/save ──────────────────────────────────────────────────
  let state = { ...DEFAULTS };
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) state = { ...DEFAULTS, ...JSON.parse(raw) };
  } catch (e) {
    // localStorage unavailable (static export, sandboxed) — defaults are fine.
  }
  function persist() {
    try { localStorage.setItem(STORAGE_KEY, JSON.stringify(state)); } catch (e) {}
  }

  // ── apply state → DOM ────────────────────────────────────────────────
  function applyFilters() {
    for (const f of FILTER_FLAGS) {
      const hide = state[f] === 0;
      if (hide) root.dataset["hide" + f[0].toUpperCase() + f.slice(1)] = "1";
      else delete root.dataset["hide" + f[0].toUpperCase() + f.slice(1)];
      const pill = document.querySelector('input[data-filter="' + f + '"]');
      if (pill) pill.checked = !hide;
    }
    applySearch();
  }

  function applySearch() {
    const q = (state.search || "").trim().toLowerCase();
    const input = document.querySelector('input[data-search]');
    if (input && input.value !== state.search) input.value = state.search;
    const tools = document.querySelectorAll('.ses-tool');
    if (!q) {
      root.removeAttribute('data-tool-search');
      tools.forEach(t => t.removeAttribute('data-search-match'));
      return;
    }
    root.dataset.toolSearch = q;
    tools.forEach(t => {
      const name = (t.querySelector('.ses-tool__name')?.textContent || '').toLowerCase();
      const head = (t.querySelector('.ses-tool__head')?.textContent || '').toLowerCase();
      if (name.includes(q) || head.includes(q)) t.setAttribute('data-search-match', '1');
      else t.removeAttribute('data-search-match');
    });
  }

  // ── filter pills ─────────────────────────────────────────────────────
  // Two click modes:
  //   plain click  → flip just this pill
  //   shift+click  → solo (this on, everything else off)
  document.querySelectorAll('label.ses-bar__pill').forEach(label => {
    const input = label.querySelector('input[data-filter]');
    if (!input) return;
    label.addEventListener('click', e => {
      if (e.shiftKey) {
        e.preventDefault();
        const solo = input.dataset.filter;
        for (const f of FILTER_FLAGS) state[f] = f === solo ? 1 : 0;
        applyFilters();
        persist();
      }
    });
    input.addEventListener('change', () => {
      state[input.dataset.filter] = input.checked ? 1 : 0;
      applyFilters();
      persist();
    });
  });

  // ── all / none filter shortcuts ──────────────────────────────────────
  document.querySelectorAll('button[data-action="filter-all"]').forEach(btn => {
    btn.addEventListener('click', () => {
      for (const f of FILTER_FLAGS) state[f] = 1;
      applyFilters();
      persist();
    });
  });
  document.querySelectorAll('button[data-action="filter-none"]').forEach(btn => {
    btn.addEventListener('click', () => {
      for (const f of FILTER_FLAGS) state[f] = 0;
      applyFilters();
      persist();
    });
  });

  // ── search input ─────────────────────────────────────────────────────
  const search = document.querySelector('input[data-search]');
  if (search) {
    search.addEventListener('input', () => {
      state.search = search.value;
      applySearch();
      persist();
    });
  }

  // ── expand-all / collapse-all ────────────────────────────────────────
  document.querySelectorAll('button[data-action="expand-all"]').forEach(btn => {
    btn.addEventListener('click', () => {
      document.querySelectorAll('details:not(.ses-debug):not(.ses-rawblock)').forEach(d => { d.open = true; });
    });
  });
  document.querySelectorAll('button[data-action="collapse-all"]').forEach(btn => {
    btn.addEventListener('click', () => {
      document.querySelectorAll('details:not(.ses-debug):not(.ses-rawblock)').forEach(d => { d.open = false; });
    });
  });

  // ── minimap click + viewport cursor (replaces the old #turn-N jump rail —
  // W1's stable t-<uuid12> anchors + the horizontal density strip are the
  // nav answer at every iframe width, not just >1280px) ──────────────────
  document.querySelectorAll('.ses-minimap a[href^="#t-"]').forEach(a => {
    a.addEventListener('click', e => {
      e.preventDefault();
      const id = a.getAttribute('href').slice(1);
      const tgt = document.getElementById(id);
      if (tgt) tgt.scrollIntoView({ behavior: 'smooth', block: 'start' });
    });
  });

  if ('IntersectionObserver' in window) {
    const marks = new Map();
    document.querySelectorAll('.ses-minimap a[href^="#t-"]').forEach(a => {
      marks.set(a.getAttribute('href').slice(1), a);
    });
    const io = new IntersectionObserver(entries => {
      for (const en of entries) {
        const a = marks.get(en.target.id);
        if (!a) continue;
        if (en.isIntersecting) a.classList.add('is-current');
        else a.classList.remove('is-current');
      }
    }, { rootMargin: '-30% 0px -60% 0px', threshold: 0 });
    document.querySelectorAll('article.ses-turn[id^="t-"]').forEach(el => io.observe(el));
  }

  // ── ↧ result FAB — visible once scrolled more than one viewport away
  // from the bottom (reader.md Proposal 3); a plain anchor otherwise, no
  // JS required for the jump itself. rAF-throttled scroll listener, no
  // standing interval.
  const fab = document.querySelector('.ses-endfab');
  if (fab) {
    let ticking = false;
    const update = () => {
      ticking = false;
      const remaining = document.documentElement.scrollHeight - window.scrollY - window.innerHeight;
      if (remaining > window.innerHeight) fab.classList.add('is-visible');
      else fab.classList.remove('is-visible');
    };
    window.addEventListener('scroll', () => {
      if (!ticking) { ticking = true; requestAnimationFrame(update); }
    }, { passive: true });
    update();
  }

  // ── file-path link → parent (open-artifact postMessage) ─────────────
  // Render-time Rust stamps <a class="ses-path" data-kb="…" data-rel="…"> on
  // file_path tool args that resolved to an in-corpus artifact (data-rel is
  // the SOURCE-RELATIVE path the SPA navigates to — not the raw absolute path,
  // which would mis-navigate). Out-of-corpus paths are plain <span>s with no
  // data-rel, so they're inert. Click delegates here (one listener for all).
  const kbName = root.dataset.kb || '';
  document.addEventListener('click', e => {
    const a = e.target.closest('a.ses-path');
    if (!a) return;
    e.preventDefault();
    const rel = a.dataset.rel;
    if (!rel) return;
    const kb = a.dataset.kb || kbName;
    try {
      window.parent.postMessage({ kind: 'open-artifact', kb, path: rel }, '*');
    } catch (err) {}
  });

  // ── permalink hover affordance (the # glyph already in CSS) ──────────
  // Nothing to do — pure CSS via :hover.

  // ── "comment on this turn" (W6 — moonshots M2) ───────────────────────
  // Posts the SAME `cm:compose` message web/src/scripts/annotate.ts's
  // click-to-compose path already sends — a Section-scope anchor via the
  // EXISTING comment APIs, no new anchor kind, no new wire. Works whether
  // or not annotate mode is toggled (the parent's AnnotatorBridge listens
  // unconditionally whenever comments are enabled for this artifact/kb —
  // `reviewActive` in web/src/components/reader/ArtifactPane.tsx); if
  // they're not, the message is simply unheard, same inert posture as the
  // open-artifact link above when its target doesn't resolve. If annotate
  // mode IS on, annotate.ts's own capture-phase click handler (bound with
  // `useCapture: true`) fires FIRST and calls `stopPropagation()`, so this
  // bubble-phase listener never double-posts — that path already resolves
  // the identical `t-<uuid12>` id via `pickAnchor`'s `[id]` ladder.
  document.addEventListener('click', e => {
    const btn = e.target.closest('[data-kb-turn-comment]');
    if (!btn) return;
    e.preventDefault();
    const id = btn.getAttribute('data-kb-turn-comment');
    if (!id) return;
    const turn = document.getElementById(id);
    const body = turn ? turn.querySelector('.ses-turn__body') : null;
    const snippet = ((body && body.textContent) || '')
      .replace(/\s+/g, ' ')
      .trim()
      .slice(0, 200);
    // Same fallback ladder as annotate.ts's `env?.file?.artifact?.id ||
    // location.host.split(".")[0]` — `window.__KB_COMMENTS` is only present
    // when `?cm=on` injected the annotator's data block; this runtime script
    // (unconditionally injected) stands alone without it.
    let fileId = '';
    try {
      fileId = (window.__KB_COMMENTS && window.__KB_COMMENTS.file &&
        window.__KB_COMMENTS.file.artifact && window.__KB_COMMENTS.file.artifact.id) || '';
    } catch (err) {}
    if (!fileId) fileId = location.host.split('.')[0];
    try {
      window.parent.postMessage({
        type: 'cm:compose',
        anchor: { kind: 'section', id, tag: 'article', snippet },
        file: fileId,
        fileLabel: 'main',
      }, '*');
    } catch (err) {}
  });

  // ── find-in-transcript (W6 — moonshots M7) ───────────────────────────
  // Serve-time, retroactive to every capture, zero server round-trip. The
  // bar's MARKUP is static server-rendered HTML (`render_find_bar`,
  // session_render.rs) — this only wires listeners onto those exact
  // elements (same discipline as the filter-pill wiring above), so there's
  // never an `innerHTML` write of the search term anywhere (no XSS surface:
  // matching + highlighting are `textContent` reads and class toggles).
  // Turn-level granularity (not sub-string spans) — matches are `.ses-turn`
  // cards whose BODY text contains the query, which is also exactly the
  // grain `#t-<uuid12>` deep links (and M2's stable ids) address.
  (function () {
    const bar = document.getElementById('ses-find');
    if (!bar) return; // husk capture — render_find_bar emits nothing
    const input = document.getElementById('ses-find-input');
    const countEl = document.getElementById('ses-find-count');
    let matches = [];
    let activeIdx = -1;

    function clearHighlight() {
      const cur = document.querySelector('.ses-find-current');
      if (cur) cur.classList.remove('ses-find-current');
    }

    // Reveal any collapsed ancestor `<details>` (side-lane wrapper, a
    // closed tool/result accordion) so the hit is actually visible once
    // scrolled to — mirrors annotate.ts's `revealAncestors`.
    function revealAncestors(el) {
      let cur = el;
      while (cur && cur !== document.body) {
        if (cur.tagName === 'DETAILS' && !cur.open) cur.open = true;
        cur = cur.parentElement;
      }
    }

    function updateCount() {
      if (!countEl) return;
      countEl.textContent = matches.length ? (activeIdx + 1) + '/' + matches.length : '0/0';
    }

    // Lazy, DATA-only walk — `.textContent` never forces layout, so this
    // stays cheap even under `content-visibility: auto` turn cards that
    // are currently unrendered off-screen (their text is still fully
    // present in the DOM; only paint/layout is skipped). Runs once per
    // committed query, not per keystroke-and-render.
    function runSearch(raw) {
      const q = (raw || '').trim().toLowerCase();
      clearHighlight();
      matches = [];
      activeIdx = -1;
      if (q) {
        document.querySelectorAll('.ses-turn').forEach(turn => {
          const body = turn.querySelector('.ses-turn__body');
          const text = ((body && body.textContent) || '').toLowerCase();
          if (text.indexOf(q) !== -1) matches.push(turn);
        });
      }
      updateCount();
      if (matches.length) go(0);
    }

    function go(idx) {
      if (!matches.length) return;
      activeIdx = ((idx % matches.length) + matches.length) % matches.length;
      clearHighlight();
      const turn = matches[activeIdx];
      revealAncestors(turn);
      turn.classList.add('ses-find-current');
      turn.scrollIntoView({ behavior: 'smooth', block: 'center' });
      // Update the permalink hash to the hit's stable turn id (M7 —
      // "updates the hash to the hit's stable turn id") without pushing a
      // new history entry per hit.
      if (turn.id && window.history && history.replaceState) {
        try { history.replaceState(null, '', '#' + turn.id); } catch (err) {}
      }
      updateCount();
    }

    function next() { go(activeIdx + 1); }
    function prev() { go(activeIdx - 1); }

    function openBar(prefill) {
      bar.hidden = false;
      if (typeof prefill === 'string') input.value = prefill;
      input.focus();
      input.select();
      if (input.value) runSearch(input.value);
      else updateCount();
    }

    function closeBar() {
      if (bar.hidden) return;
      bar.hidden = true;
      clearHighlight();
      matches = [];
      activeIdx = -1;
    }

    if (input) {
      input.addEventListener('input', () => runSearch(input.value));
      input.addEventListener('keydown', e => {
        if (e.key === 'Enter') {
          e.preventDefault();
          if (e.shiftKey) prev(); else next();
        } else if (e.key === 'Escape') {
          e.preventDefault();
          closeBar();
        }
      });
    }
    bar.addEventListener('click', e => {
      const btn = e.target.closest('[data-find-action]');
      if (!btn) return;
      const action = btn.getAttribute('data-find-action');
      if (action === 'next') next();
      else if (action === 'prev') prev();
      else if (action === 'close') closeBar();
    });

    // `/` opens the bar — guarded against firing while typing in ANY
    // input/textarea/contenteditable (including the existing tool-search
    // box in the filter toolbar).
    document.addEventListener('keydown', e => {
      if (!bar.hidden && e.key === 'Escape' && document.activeElement !== input) {
        closeBar();
        return;
      }
      if (e.key !== '/' || e.metaKey || e.ctrlKey || e.altKey) return;
      const t = e.target;
      const typing = t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable);
      if (typing) return;
      e.preventDefault();
      openBar();
    });

    // `#find=<term>` pre-seeding — deep-links FROM corpus search / an
    // agent-emitted link INTO the exact matching turn. Runs on load and on
    // any later in-page hash change (the SPA can rewrite the iframe's hash
    // without a full reload).
    function seedFromHash() {
      const h = location.hash;
      const prefix = '#find=';
      if (h.indexOf(prefix) !== 0) return;
      const term = decodeURIComponent(h.slice(prefix.length));
      if (term) openBar(term);
    }
    window.addEventListener('hashchange', seedFromHash);
    seedFromHash();
  })();

  // ── boot ─────────────────────────────────────────────────────────────
  applyFilters();
})();
