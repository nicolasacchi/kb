// app/comments.jsx — annotate artifacts inline; threaded replies; export to MD/JSON/Claude prompt

const { useState: cmS, useEffect: cmE, useMemo: cmM, useRef: cmR } = React;

const CM_KEY = 'artifact-comments-v2';
const CM_LEGACY = 'artifact-comments-v1';

// ──────────────────────────────────────────────────────────────────────────
// Storage + hook
// ──────────────────────────────────────────────────────────────────────────

function cmLoadAll() {
  try {
    const v2 = JSON.parse(localStorage.getItem(CM_KEY) || 'null');
    if (v2) return v2;
    const v1 = JSON.parse(localStorage.getItem(CM_LEGACY) || '{}');
    // migrate
    const migrated = {};
    Object.entries(v1).forEach(([k, list]) => {
      migrated[k] = list.map(c => ({
        ...c,
        body: c.body || c.comment || '',
        status: c.status || 'open',
        replies: c.replies || [],
      }));
    });
    return migrated;
  } catch { return {}; }
}
function cmSaveAll(all) {
  try { localStorage.setItem(CM_KEY, JSON.stringify(all)); } catch {}
}

function rid(prefix) {
  return prefix + '_' + Date.now().toString(36) + '_' + Math.random().toString(36).slice(2, 6);
}

function useComments(artifactId) {
  const [all, setAll] = cmS(cmLoadAll);
  const list = all[artifactId] || [];
  const update = (next) => { const c = { ...all, [artifactId]: next }; setAll(c); cmSaveAll(c); };

  const add = (c) => {
    const item = {
      id: rid('c'),
      ts: Date.now(),
      status: 'open',
      replies: [],
      body: c.body || c.comment || '',
      ...c,
    };
    delete item.comment;
    update([...list, item]);
    return item;
  };
  const editBody = (id, body) => update(list.map(c => c.id === id ? { ...c, body, editedTs: Date.now() } : c));
  const setStatus = (id, status) => update(list.map(c => c.id === id ? { ...c, status, statusTs: Date.now() } : c));
  const addReply = (id, reply) => update(list.map(c => c.id === id ? {
    ...c,
    replies: [...(c.replies || []), { id: rid('r'), ts: Date.now(), author: 'you', ...reply }],
  } : c));
  const editReply = (cid, rid_, body) => update(list.map(c => c.id === cid ? {
    ...c,
    replies: (c.replies || []).map(r => r.id === rid_ ? { ...r, body, editedTs: Date.now() } : r),
  } : c));
  const removeReply = (cid, rid_) => update(list.map(c => c.id === cid ? {
    ...c,
    replies: (c.replies || []).filter(r => r.id !== rid_),
  } : c));
  const remove = (id) => update(list.filter(c => c.id !== id));
  const clear = () => update([]);

  return { comments: list, add, editBody, setStatus, addReply, editReply, removeReply, remove, clear };
}

// ──────────────────────────────────────────────────────────────────────────
// In-iframe annotator bridge
// ──────────────────────────────────────────────────────────────────────────

function injectCommentBridge(iframe, ctx) {
  const doc = iframe.contentDocument;
  if (!doc || !doc.body) return () => {};
  if (doc.getElementById('__cm_style')) return () => {};

  const accent = (getComputedStyle(document.documentElement).getPropertyValue('--accent').trim()) || '#d97757';

  const style = doc.createElement('style');
  style.id = '__cm_style';
  style.textContent = `
    .__cm-on [data-cm-target] { cursor: pointer; transition: outline 0.1s, background 0.1s; }
    .__cm-on [data-cm-target]:hover { outline: 1px dashed ${accent}; outline-offset: 3px; background: ${accent}10; }
    [data-cm-has-comment] { box-shadow: -3px 0 0 ${accent}; padding-left: 8px; }
    [data-cm-has-comment="resolved"] { box-shadow: -3px 0 0 #9ca29c; opacity: 0.85; }
    [data-cm-flash] { outline: 2px solid ${accent} !important; outline-offset: 4px; transition: outline 0.18s; background: ${accent}15; }
    .__cm-pop {
      position: absolute; z-index: 99999;
      background: var(--bg, #fffaf0); color: var(--text, #1a1714);
      border: 1px solid ${accent}; border-radius: 6px;
      box-shadow: 0 12px 28px rgba(0,0,0,0.18);
      padding: 12px; width: 320px;
      font-family: -apple-system, system-ui, 'Inter', sans-serif; font-size: 13px;
    }
    .__cm-pop .__t { font-family: ui-monospace, monospace; font-size: 11px; color: #6a6055; margin-bottom: 8px; line-height: 1.4; }
    .__cm-pop .__t b { color: ${accent}; font-weight: 600; }
    .__cm-pop textarea {
      width: 100%; min-height: 70px; resize: vertical; box-sizing: border-box;
      border: 1px solid #d8cfc0; border-radius: 4px; padding: 7px 8px; font: inherit; color: #1a1714; background: white;
    }
    .__cm-pop textarea:focus { outline: none; border-color: ${accent}; }
    .__cm-pop .__r { display: flex; gap: 4px; margin-top: 8px; justify-content: flex-end; align-items: center; }
    .__cm-pop .__hint { margin-right: auto; font-family: ui-monospace, monospace; font-size: 10px; color: #8a8275; }
    .__cm-pop button {
      font: inherit; padding: 5px 8px; border-radius: 4px; cursor: pointer; border: 1px solid #d8cfc0;
      background: white; color: #555; display: inline-flex; align-items: center; gap: 4px;
    }
    .__cm-pop button:hover { border-color: ${accent}; color: ${accent}; }
    .__cm-pop button.__s { background: ${accent}; color: white; border-color: ${accent}; }
    .__cm-pop button.__s:hover { color: white; filter: brightness(1.07); }
    .__cm-pop svg { display: block; }
  `;
  doc.head.appendChild(style);
  doc.body.classList.add('__cm-on');

  // tag eligible elements
  const SEL = 'h1, h2, h3, h4, p, li, td, pre, blockquote, .lede, .callout, .pull, .stat, .diagram';
  doc.querySelectorAll(SEL).forEach((el, i) => {
    if (el.closest('.__cm-pop')) return;
    if (!el.hasAttribute('data-cm-target')) el.setAttribute('data-cm-target', '');
    if (!el.hasAttribute('data-cm-id')) el.setAttribute('data-cm-id', 'el-' + i);
  });

  let pop = null;
  const closePop = () => { if (pop) { pop.remove(); pop = null; } };

  const onClick = (e) => {
    if (e.target.closest('.__cm-pop')) return;
    const sel = doc.getSelection ? doc.getSelection() : null;
    const selText = sel ? sel.toString().trim() : '';
    const target = e.target.closest('[data-cm-target]');
    if (!target && !selText) { closePop(); return; }

    e.preventDefault(); e.stopPropagation();
    closePop();

    let kind, ref, snippet, tag;
    if (selText && selText.length > 0) {
      kind = 'selection';
      tag = (target && target.tagName.toLowerCase()) || null;
      ref = (target && target.getAttribute('data-cm-id')) || null;
      snippet = selText.slice(0, 240);
    } else {
      const t = target.tagName;
      kind = /^H[1-4]$/.test(t) ? 'heading' : 'block';
      tag = t.toLowerCase();
      ref = target.getAttribute('data-cm-id');
      snippet = (target.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 200);
    }

    const rect = target ? target.getBoundingClientRect() : { left: e.clientX, top: e.clientY, bottom: e.clientY + 16 };
    pop = doc.createElement('div');
    pop.className = '__cm-pop';
    const safeSnip = snippet.slice(0, 70).replace(/[<>&]/g, ch => ({'<':'&lt;','>':'&gt;','&':'&amp;'}[ch]));
    // svg icons inline so they render inside the iframe doc
    const ICON_X = '<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.5"><path d="m4 4 8 8M12 4l-8 8"/></svg>';
    const ICON_CHECK = '<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.7"><path d="m3 8 3.5 3.5L13 5"/></svg>';
    pop.innerHTML =
      '<div class="__t"><b>' + (kind === 'selection' ? 'selection' : kind) + '</b> · ' + (tag || '?') +
      ' · "' + safeSnip + (snippet.length > 70 ? '…' : '') + '"</div>' +
      '<textarea placeholder="comment, question, or instruction for Claude Code…"></textarea>' +
      '<div class="__r">' +
        '<span class="__hint">⌘⏎ to save</span>' +
        '<button class="__c" title="cancel">' + ICON_X + '</button>' +
        '<button class="__s" title="save">' + ICON_CHECK + '</button>' +
      '</div>';

    doc.body.appendChild(pop);
    const sx = doc.defaultView.pageXOffset || doc.documentElement.scrollLeft || 0;
    const sy = doc.defaultView.pageYOffset || doc.documentElement.scrollTop || 0;
    const top = rect.bottom + sy + 6;
    const left = Math.max(8, Math.min(rect.left + sx, doc.documentElement.clientWidth - 340));
    pop.style.top = top + 'px';
    pop.style.left = left + 'px';

    const ta = pop.querySelector('textarea');
    setTimeout(() => ta.focus(), 0);
    pop.querySelector('.__c').onclick = closePop;
    pop.querySelector('.__s').onclick = () => {
      const txt = ta.value.trim();
      if (!txt) { closePop(); return; }
      try {
        // Bridge runs in the host realm (we attached the click handler from
        // the parent), so post to our own window — the useComments listener
        // is on `window`, not `window.parent`.
        window.postMessage({
          type: 'cm:add',
          artifactId: ctx.artifactId,
          file: ctx.file,
          fileLabel: ctx.label,
          kind, tag, ref, snippet,
          body: txt,
        }, '*');
      } catch (_) {}
      closePop();
    };
    ta.addEventListener('keydown', (ev) => {
      if (ev.key === 'Escape') closePop();
      else if ((ev.metaKey || ev.ctrlKey) && ev.key === 'Enter') pop.querySelector('.__s').click();
    });
  };

  doc.addEventListener('click', onClick, true);

  const refresh = (existing) => {
    doc.querySelectorAll('[data-cm-has-comment]').forEach(el => el.removeAttribute('data-cm-has-comment'));
    (existing || []).forEach(c => {
      if (!c.ref) return;
      const el = doc.querySelector('[data-cm-id="' + c.ref + '"]');
      if (el) el.setAttribute('data-cm-has-comment', c.status === 'resolved' ? 'resolved' : 'open');
    });
  };
  if (ctx.existing) refresh(ctx.existing);
  iframe.__cmRefresh = refresh;

  iframe.__cmFlash = (refId) => {
    const el = doc.querySelector('[data-cm-id="' + refId + '"]');
    if (!el) return false;
    el.scrollIntoView({ block: 'center', behavior: 'smooth' });
    el.setAttribute('data-cm-flash', '');
    setTimeout(() => el.removeAttribute('data-cm-flash'), 1300);
    return true;
  };

  return () => {
    closePop();
    doc.body.classList.remove('__cm-on');
    style.remove();
    doc.removeEventListener('click', onClick, true);
    doc.querySelectorAll('[data-cm-target]').forEach(el => el.removeAttribute('data-cm-target'));
    doc.querySelectorAll('[data-cm-has-comment]').forEach(el => el.removeAttribute('data-cm-has-comment'));
    delete iframe.__cmRefresh;
    delete iframe.__cmFlash;
  };
}

// ──────────────────────────────────────────────────────────────────────────
// Time helper
// ──────────────────────────────────────────────────────────────────────────

function relTime(ts) {
  if (!ts) return '';
  const d = (Date.now() - ts) / 1000;
  if (d < 45) return 'just now';
  if (d < 90) return '1m ago';
  if (d < 60 * 45) return Math.round(d / 60) + 'm ago';
  if (d < 60 * 90) return '1h ago';
  if (d < 60 * 60 * 22) return Math.round(d / 3600) + 'h ago';
  if (d < 60 * 60 * 36) return '1d ago';
  if (d < 60 * 60 * 24 * 7) return Math.round(d / 86400) + 'd ago';
  const dt = new Date(ts);
  return dt.toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
}
function absTime(ts) {
  if (!ts) return '';
  const dt = new Date(ts);
  return dt.toLocaleString(undefined, { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' });
}

// ──────────────────────────────────────────────────────────────────────────
// Tiny inline icon set used inside the panel
// ──────────────────────────────────────────────────────────────────────────

const SVG = (path, w = 14, h = 14) => (
  <svg viewBox="0 0 16 16" width={w} height={h} fill="none" stroke="currentColor" strokeWidth="1.5">{path}</svg>
);
const CIcon = {
  Reply:    SVG(<path d="M9 3 4 7l5 4V8.5c2.5 0 4 1 5 3-.2-3.5-2-5.5-5-5.5z"/>),
  Edit:     SVG(<><path d="m11 2 3 3-8 8H3v-3z"/><path d="m9.5 3.5 3 3"/></>),
  Find:     SVG(<><circle cx="7" cy="7" r="4.5"/><path d="m10.5 10.5 3 3"/></>),
  Check:    SVG(<path d="m3 8 3.5 3.5L13 5"/>),
  Reopen:   SVG(<><path d="M3 8a5 5 0 1 0 1.5-3.5"/><path d="M3 3v3h3"/></>),
  Trash:    SVG(<><path d="M3 5h10M6 5V3h4v2M5 5l1 9h4l1-9"/></>),
  Close:    SVG(<path d="m4 4 8 8M12 4l-8 8"/>),
  Pen:      SVG(<><path d="m11 2 3 3-8 8H3v-3z"/><path d="m9.5 3.5 3 3"/></>),
  Send:     SVG(<><path d="M2 8 14 3l-3 11-3-5z"/><path d="m8 9 3-6"/></>),
  Spark:    SVG(<path d="M8 1v4M8 11v4M1 8h4M11 8h4M3.5 3.5l2 2M10.5 10.5l2 2M3.5 12.5l2-2M10.5 5.5l2-2"/>),
  Copy:     SVG(<><rect x="5" y="5" width="9" height="9" rx="1"/><path d="M3 11V3a1 1 0 0 1 1-1h7"/></>),
  Download: SVG(<><path d="M8 2v9M5 8l3 3 3-3"/><path d="M3 13h10"/></>),
  Filter:   SVG(<path d="M2 3h12l-4.5 6V14L6 12V9z"/>),
  Term:     SVG(<><rect x="2" y="3" width="12" height="10" rx="1"/><path d="m4 6 2 2-2 2M8 11h4"/></>),
};

// ──────────────────────────────────────────────────────────────────────────
// Reply composer (used inline + at-claude shortcut)
// ──────────────────────────────────────────────────────────────────────────

function ReplyComposer({ onSubmit, onCancel, autoFocus, defaultAuthor = 'you' }) {
  const [body, setBody] = cmS('');
  const [author, setAuthor] = cmS(defaultAuthor);
  const taRef = cmR(null);
  cmE(() => { if (autoFocus && taRef.current) taRef.current.focus(); }, [autoFocus]);
  const submit = () => {
    const t = body.trim();
    if (!t) return;
    onSubmit({ body: t, author });
    setBody('');
  };
  return (
    <div className="cm-reply-comp">
      <div className="cm-reply-author">
        <span className="cm-reply-author-label">replying as</span>
        <div className="cm-author-seg">
          {['you', 'claude'].map(a => (
            <button
              key={a}
              className={'cm-author-opt ' + (author === a ? 'is-on' : '')}
              onClick={() => setAuthor(a)}
              type="button"
            >
              {a === 'claude' && <span className="cm-claude-dot"/>}
              {a}
            </button>
          ))}
        </div>
      </div>
      <textarea
        ref={taRef}
        value={body}
        placeholder={author === 'claude' ? 'simulate a Claude response…' : 'reply…'}
        onChange={(e) => setBody(e.target.value)}
        onKeyDown={(e) => {
          if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') submit();
          if (e.key === 'Escape') onCancel();
        }}
      />
      <div className="cm-reply-acts">
        <span className="cm-hint mono">⌘⏎ send · esc cancel</span>
        <button className="cm-icon-btn" onClick={onCancel} title="cancel">{CIcon.Close}</button>
        <button className="cm-icon-btn cm-icon-btn--primary" onClick={submit} title="send" disabled={!body.trim()}>{CIcon.Send}</button>
      </div>
    </div>
  );
}

// ──────────────────────────────────────────────────────────────────────────
// One comment thread item
// ──────────────────────────────────────────────────────────────────────────

function CommentItem({ c, isCurrent, onJump, onEdit, onAddReply, onEditReply, onRemoveReply, onResolve, onReopen, onRemove }) {
  const [editing, setEditing] = cmS(false);
  const [editVal, setEditVal] = cmS(c.body);
  const [replying, setReplying] = cmS(false);
  const [editingRid, setEditingRid] = cmS(null);
  const [editRVal, setEditRVal] = cmS('');

  cmE(() => { setEditVal(c.body); }, [c.body]);

  const saveEdit = () => {
    const t = editVal.trim();
    if (!t) return;
    onEdit(c.id, t);
    setEditing(false);
  };
  const isResolved = c.status === 'resolved';

  return (
    <li className={'cp-item ' + (isResolved ? 'is-resolved' : '') + (isCurrent ? ' is-current-file' : '')}>
      <div className="cp-item-target" onClick={() => onJump(c)} role="button" title="jump to element">
        <span className="cp-tag mono">{c.tag || c.kind}</span>
        <span className="cp-snippet">"{(c.snippet || '').slice(0, 90)}{(c.snippet || '').length > 90 ? '…' : ''}"</span>
      </div>

      {editing ? (
        <div className="cm-edit-comp">
          <textarea
            autoFocus
            value={editVal}
            onChange={(e) => setEditVal(e.target.value)}
            onKeyDown={(e) => {
              if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') saveEdit();
              if (e.key === 'Escape') setEditing(false);
            }}
          />
          <div className="cm-reply-acts">
            <span className="cm-hint mono">⌘⏎ save</span>
            <button className="cm-icon-btn" onClick={() => setEditing(false)} title="cancel">{CIcon.Close}</button>
            <button className="cm-icon-btn cm-icon-btn--primary" onClick={saveEdit} title="save">{CIcon.Check}</button>
          </div>
        </div>
      ) : (
        <div className="cp-comment-block">
          <div className="cp-author-row">
            <span className="cp-author">you</span>
            <span className="cp-time" title={absTime(c.ts)}>{relTime(c.ts)}</span>
            {c.editedTs && <span className="cp-edited" title={absTime(c.editedTs)}>edited</span>}
            {isResolved && <span className="cp-pill cp-pill--resolved">resolved</span>}
          </div>
          <div className="cp-comment">{c.body || c.comment}</div>
        </div>
      )}

      {(c.replies && c.replies.length > 0) && (
        <ul className="cp-replies">
          {c.replies.map(r => {
            const isClaude = r.author === 'claude';
            return (
              <li key={r.id} className={'cp-reply ' + (isClaude ? 'is-claude' : '')}>
                <div className="cp-author-row">
                  <span className="cp-author">
                    {isClaude && <span className="cp-claude-mark">{CIcon.Spark}</span>}
                    {r.author || 'you'}
                  </span>
                  <span className="cp-time" title={absTime(r.ts)}>{relTime(r.ts)}</span>
                  {r.editedTs && <span className="cp-edited">edited</span>}
                  <span className="cp-reply-acts">
                    <button className="cm-icon-btn cm-icon-btn--xs" title="edit reply" onClick={() => { setEditingRid(r.id); setEditRVal(r.body); }}>{CIcon.Edit}</button>
                    <button className="cm-icon-btn cm-icon-btn--xs cm-icon-btn--danger" title="delete reply" onClick={() => onRemoveReply(c.id, r.id)}>{CIcon.Trash}</button>
                  </span>
                </div>
                {editingRid === r.id ? (
                  <div className="cm-edit-comp cm-edit-comp--inline">
                    <textarea
                      autoFocus
                      value={editRVal}
                      onChange={(e) => setEditRVal(e.target.value)}
                      onKeyDown={(e) => {
                        if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
                          if (editRVal.trim()) { onEditReply(c.id, r.id, editRVal.trim()); setEditingRid(null); }
                        }
                        if (e.key === 'Escape') setEditingRid(null);
                      }}
                    />
                    <div className="cm-reply-acts">
                      <button className="cm-icon-btn" onClick={() => setEditingRid(null)} title="cancel">{CIcon.Close}</button>
                      <button className="cm-icon-btn cm-icon-btn--primary" onClick={() => { if (editRVal.trim()) { onEditReply(c.id, r.id, editRVal.trim()); setEditingRid(null); } }} title="save">{CIcon.Check}</button>
                    </div>
                  </div>
                ) : (
                  <div className="cp-comment">{r.body}</div>
                )}
              </li>
            );
          })}
        </ul>
      )}

      {replying && (
        <ReplyComposer
          autoFocus
          onSubmit={(r) => { onAddReply(c.id, r); setReplying(false); }}
          onCancel={() => setReplying(false)}
        />
      )}

      <div className="cp-actions">
        <button className="cm-icon-btn" title="jump to" onClick={() => onJump(c)}>{CIcon.Find}</button>
        <button className="cm-icon-btn" title="reply" onClick={() => setReplying(v => !v)}>{CIcon.Reply}</button>
        <button className="cm-icon-btn" title="edit" onClick={() => setEditing(true)}>{CIcon.Edit}</button>
        {isResolved
          ? <button className="cm-icon-btn" title="reopen" onClick={() => onReopen(c.id)}>{CIcon.Reopen}</button>
          : <button className="cm-icon-btn" title="resolve" onClick={() => onResolve(c.id)}>{CIcon.Check}</button>
        }
        <span className="cp-actions-spacer"/>
        <button className="cm-icon-btn cm-icon-btn--danger" title="delete" onClick={() => onRemove(c.id)}>{CIcon.Trash}</button>
      </div>
    </li>
  );
}

// ──────────────────────────────────────────────────────────────────────────
// Panel
// ──────────────────────────────────────────────────────────────────────────

function CommentsPanel({
  a, comments, currentFile, onClose, onRemove, onClear, onExport, onJump,
  commentMode, onToggleMode,
  onEdit, onSetStatus, onAddReply, onEditReply, onRemoveReply,
}) {
  const [filter, setFilter] = cmS('open'); // 'open' | 'resolved' | 'all'
  const visible = comments.filter(c => filter === 'all' ? true : (c.status || 'open') === filter);
  const counts = {
    open: comments.filter(c => (c.status || 'open') === 'open').length,
    resolved: comments.filter(c => c.status === 'resolved').length,
    all: comments.length,
  };

  const byFile = {};
  visible.forEach(c => {
    const k = c.file || 'main';
    (byFile[k] = byFile[k] || { label: c.fileLabel || '', items: [] }).items.push(c);
  });
  const fileKeys = Object.keys(byFile);

  return (
    <aside className="comments-panel">
      <div className="cp-head">
        <div>
          <span className="dp-eyebrow">comments · {comments.length}</span>
        </div>
        <button className="dp-close" onClick={onClose}><Icon.X/></button>
      </div>

      <div className="cp-mode-row">
        <button
          className={'cp-mode-btn ' + (commentMode ? 'is-on' : '')}
          onClick={onToggleMode}
        >
          <span className="cp-mode-dot"/>
          {commentMode ? 'annotating · click any block' : 'turn on annotate mode'}
        </button>
      </div>

      <div className="cp-filter-row">
        {[
          { k: 'open', label: 'open' },
          { k: 'resolved', label: 'resolved' },
          { k: 'all', label: 'all' },
        ].map(o => (
          <button
            key={o.k}
            className={'cp-filter ' + (filter === o.k ? 'is-on' : '')}
            onClick={() => setFilter(o.k)}
          >
            {o.label} <span className="cp-filter-c">{counts[o.k]}</span>
          </button>
        ))}
      </div>

      {visible.length === 0 ? (
        <div className="cp-empty">
          {comments.length === 0 ? (
            <>
              <p className="cp-empty-h">No comments yet.</p>
              <p className="cp-empty-p">Toggle <b>annotate</b>, then click a heading, paragraph, list item, table cell, code block, or callout. Selecting text first makes a precision comment on those exact words.</p>
            </>
          ) : (
            <>
              <p className="cp-empty-h">Nothing in <b>{filter}</b>.</p>
              <p className="cp-empty-p">Switch the filter above to see other comments.</p>
            </>
          )}
        </div>
      ) : (
        <div className="cp-body">
          {fileKeys.map((file) => (
            <div key={file} className="cp-file-section">
              <div className={'cp-file-head' + (file === currentFile ? ' is-current' : '')}>
                <span className="cp-file mono">{file.replace(/^artifacts\//, '')}</span>
                {byFile[file].label && <span className="cp-file-label">· {byFile[file].label}</span>}
                {file === currentFile && <span className="cp-file-here">here</span>}
              </div>
              <ul className="cp-list">
                {byFile[file].items.map(c => (
                  <CommentItem
                    key={c.id}
                    c={c}
                    isCurrent={file === currentFile}
                    onJump={onJump}
                    onEdit={onEdit}
                    onAddReply={onAddReply}
                    onEditReply={onEditReply}
                    onRemoveReply={onRemoveReply}
                    onResolve={(id) => onSetStatus(id, 'resolved')}
                    onReopen={(id) => onSetStatus(id, 'open')}
                    onRemove={onRemove}
                  />
                ))}
              </ul>
            </div>
          ))}
        </div>
      )}

      <div className="cp-foot">
        <button className="cm-btn cm-btn-primary" disabled={comments.length === 0} onClick={onExport}>
          <span className="cm-btn-icon">{CIcon.Send}</span>
          <span>send to claude</span>
        </button>
        <button className="cm-btn cm-btn-icon-only" disabled={comments.length === 0} onClick={onClear} title="clear all">{CIcon.Trash}</button>
      </div>
    </aside>
  );
}

// ──────────────────────────────────────────────────────────────────────────
// Export builders
// ──────────────────────────────────────────────────────────────────────────

function buildKbJson(a, comments) {
  return {
    schema: 'kb-comments/1',
    artifact: {
      id: a.id,
      title: a.title,
      kb: a.kb,
      tags: a.tags,
      pages: a.pages ? a.pages.map(p => ({ src: p.src, label: p.label })) : null,
    },
    generatedAt: new Date().toISOString(),
    comments: comments.map(c => ({
      id: c.id,
      status: c.status || 'open',
      file: c.file,
      fileLabel: c.fileLabel,
      anchor: { kind: c.kind, tag: c.tag, ref: c.ref, snippet: c.snippet },
      author: 'you',
      body: c.body || c.comment || '',
      createdAt: new Date(c.ts).toISOString(),
      editedAt: c.editedTs ? new Date(c.editedTs).toISOString() : null,
      replies: (c.replies || []).map(r => ({
        id: r.id,
        author: r.author,
        body: r.body,
        createdAt: new Date(r.ts).toISOString(),
        editedAt: r.editedTs ? new Date(r.editedTs).toISOString() : null,
      })),
    })),
  };
}

function buildClaudePrompt(a, comments) {
  const open = comments.filter(c => (c.status || 'open') === 'open');
  const path = `kb/${a.kb}/.review/${a.id}.json`;
  return `# Review: ${a.title}

I've left ${comments.length} comment${comments.length === 1 ? '' : 's'} on this artifact (${open.length} open). The structured review file is at:

  ${path}

## What I'd like you to do

For each comment with \`status: "open"\`:

1. Locate the source by \`file\` + \`anchor.ref\` (the \`data-cm-id\` attribute on the element). The \`anchor.snippet\` is the visible text — use it to confirm you're on the right element.
2. Address the request in \`body\`.
3. Append a reply to the comment's \`replies\` array with \`author: "claude"\` describing what you changed (file + line + summary).
4. If you fully addressed it, set \`status: "resolved"\` and add \`statusTs\` (ISO timestamp).
5. If a comment is unclear, leave it open and reply asking for clarification.

## Schema

Each comment looks like:
\`\`\`json
{
  "id": "c_xxx", "status": "open" | "resolved",
  "file": "artifacts/...html", "fileLabel": "page label",
  "anchor": { "kind": "block"|"heading"|"selection", "tag": "p", "ref": "el-12", "snippet": "..." },
  "author": "you", "body": "the request",
  "createdAt": "ISO", "editedAt": "ISO|null",
  "replies": [{ "id":"r_x","author":"you|claude","body":"...","createdAt":"ISO" }]
}
\`\`\`

## Files in scope

${a.pages ? a.pages.map(p => '- `' + p.src + '` — ' + p.label).join('\n') : '- `' + (a.realSrc || `kb/${a.kb}/${a.id}.html`) + '`'}

## Open comments preview

${open.length === 0 ? '_(no open comments)_' : open.slice(0, 8).map((c, i) =>
  `${i + 1}. **${c.fileLabel || c.file}** · \`<${c.tag || '?'}>\` "${(c.snippet || '').slice(0, 60)}${(c.snippet || '').length > 60 ? '…' : ''}"\n   → ${(c.body || c.comment || '').split('\n')[0].slice(0, 140)}`
).join('\n\n') + (open.length > 8 ? `\n\n_…and ${open.length - 8} more in the JSON._` : '')}
`;
}

function buildExportText(a, comments) {
  if (!comments.length) return `# Review · ${a.title}\n\n_No comments yet._\n`;
  const byFile = {};
  comments.forEach(c => {
    const k = c.file || 'main';
    (byFile[k] = byFile[k] || { label: c.fileLabel || '', items: [] }).items.push(c);
  });
  const open = comments.filter(c => (c.status || 'open') === 'open').length;
  const resolved = comments.length - open;

  let out = `# Review · ${a.title}\n\n`;
  out += `> artifact id: \`${a.id}\`` + (a.pages ? ` · ${a.pages.length} files` : '') + '\n';
  out += `> tags: ${a.tags.join(', ')}\n`;
  out += `> ${comments.length} comment${comments.length === 1 ? '' : 's'} · ${open} open · ${resolved} resolved · generated ${new Date().toISOString().slice(0,16).replace('T',' ')}\n\n`;
  out += `_Anchored to file + element + snippet — paste into Claude Code or any editor._\n\n`;
  out += `---\n\n`;

  Object.entries(byFile).forEach(([file, group]) => {
    out += `## \`${file}\`` + (group.label ? ` — ${group.label}` : '') + '\n\n';
    group.items
      .slice()
      .sort((a, b) => a.ts - b.ts)
      .forEach((c, i) => {
        const where = c.kind === 'selection'
          ? `selection inside \`<${c.tag || '?'}>\``
          : c.kind === 'heading'
          ? `heading \`<${c.tag}>\``
          : `\`<${c.tag}>\``;
        const status = (c.status === 'resolved') ? ' · ✓ resolved' : '';
        out += `### ${i + 1}. ${where}${status}\n\n`;
        out += `> ${(c.snippet || '').replace(/\s+/g, ' ').trim()}\n\n`;
        out += `**you** · ${absTime(c.ts)}\n\n`;
        (c.body || c.comment || '').split('\n').forEach(line => { out += line + '\n'; });
        out += '\n';
        (c.replies || []).forEach(r => {
          out += `**${r.author || 'you'}** · ${absTime(r.ts)}\n\n`;
          (r.body || '').split('\n').forEach(line => { out += '> ' + line + '\n'; });
          out += '\n';
        });
      });
  });
  return out;
}

// ──────────────────────────────────────────────────────────────────────────
// Export modal — three tabs
// ──────────────────────────────────────────────────────────────────────────

function ExportModal({ a, comments, onClose }) {
  const [tab, setTab] = cmS('claude'); // 'claude' | 'markdown' | 'json'
  const md = cmM(() => buildExportText(a, comments), [a, comments]);
  const prompt = cmM(() => buildClaudePrompt(a, comments), [a, comments]);
  const json = cmM(() => JSON.stringify(buildKbJson(a, comments), null, 2), [a, comments]);
  const [copied, setCopied] = cmS(false);
  const taRef = cmR(null);

  const text = tab === 'markdown' ? md : tab === 'json' ? json : prompt;
  const ext  = tab === 'json' ? 'json' : 'md';
  const filename = tab === 'json'
    ? `${a.id}.json`
    : tab === 'markdown'
    ? `review-${a.id}.md`
    : `claude-review-${a.id}.md`;
  const kbPath = `kb/${a.kb}/.review/${a.id}.json`;

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      taRef.current?.select();
      document.execCommand('copy');
    }
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  };
  const download = () => {
    const blob = new Blob([text], { type: tab === 'json' ? 'application/json' : 'text/markdown' });
    const url = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.href = url;
    link.download = filename;
    document.body.appendChild(link);
    link.click();
    link.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  };

  return (
    <div className="cm-modal-bg" onClick={onClose}>
      <div className="cm-modal" onClick={(e) => e.stopPropagation()}>
        <div className="cm-modal-head">
          <div>
            <div className="cm-modal-eyebrow">send to claude</div>
            <h2 className="cm-modal-title">{comments.length} comment{comments.length === 1 ? '' : 's'} on {a.title}</h2>
          </div>
          <button className="cm-modal-x" onClick={onClose} title="close"><Icon.X/></button>
        </div>

        <div className="cm-tabs">
          <button className={'cm-tab ' + (tab === 'claude' ? 'is-on' : '')} onClick={() => setTab('claude')}>
            <span className="cm-tab-icon">{CIcon.Spark}</span>
            <div>
              <div className="cm-tab-h">claude prompt</div>
              <div className="cm-tab-s">paste into Claude Code</div>
            </div>
          </button>
          <button className={'cm-tab ' + (tab === 'json' ? 'is-on' : '')} onClick={() => setTab('json')}>
            <span className="cm-tab-icon">{CIcon.Term}</span>
            <div>
              <div className="cm-tab-h">kb json</div>
              <div className="cm-tab-s mono">{kbPath}</div>
            </div>
          </button>
          <button className={'cm-tab ' + (tab === 'markdown' ? 'is-on' : '')} onClick={() => setTab('markdown')}>
            <span className="cm-tab-icon">{CIcon.Edit}</span>
            <div>
              <div className="cm-tab-h">markdown</div>
              <div className="cm-tab-s">readable summary</div>
            </div>
          </button>
        </div>

        {tab === 'claude' && (
          <p className="cm-modal-sub">
            A self-contained instruction for Claude Code. It expects the JSON file to be at
            <code className="mono">{' '}{kbPath}</code> — save the <b>kb json</b> tab there, then paste this prompt into your terminal.
          </p>
        )}
        {tab === 'json' && (
          <p className="cm-modal-sub">
            Machine-readable. Save to <code className="mono">{kbPath}</code> in your repo. Claude reads it, addresses each open comment, appends replies, and flips status to <code className="mono">resolved</code>.
          </p>
        )}
        {tab === 'markdown' && (
          <p className="cm-modal-sub">
            Human-readable summary. Paste into a doc, an issue tracker, or a chat — every comment is anchored to its source element.
          </p>
        )}

        <textarea ref={taRef} className="cm-modal-text mono" readOnly value={text}/>

        <div className="cm-modal-actions">
          <button className="cm-btn cm-btn-primary" onClick={copy}>
            <span className="cm-btn-icon">{CIcon.Copy}</span>
            <span>{copied ? 'copied' : 'copy'}</span>
          </button>
          <button className="cm-btn" onClick={download}>
            <span className="cm-btn-icon">{CIcon.Download}</span>
            <span>download .{ext}</span>
          </button>
          <span className="cm-modal-hint mono">{filename}</span>
        </div>
      </div>
    </div>
  );
}

window.useComments = useComments;
window.injectCommentBridge = injectCommentBridge;
window.CommentsPanel = CommentsPanel;
window.ExportModal = ExportModal;
