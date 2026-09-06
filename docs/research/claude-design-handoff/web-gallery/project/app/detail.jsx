// Detail view — iframe + floating chrome variants + slide-in details panel

const { useState: useState_d, useEffect: useEffect_d, useRef: useRef_d } = React;

function ArtifactIframe({ a, src, iframeRef }) {
  const useSrc = src || a.realSrc;
  if (useSrc) {
    return <iframe ref={iframeRef} src={useSrc} className="art-iframe" sandbox="allow-scripts allow-popups allow-forms allow-same-origin" title={a.title}/>;
  }
  // Render synthetic article into a srcDoc, with the artifact's "own" theme
  const accent = window.TAG_COLORS[a.accentTag] || '#888';
  const html = `
    <!DOCTYPE html>
    <html><head><style>
      * { box-sizing: border-box; }
      body { margin: 0; font-family: 'Lora', Georgia, serif; background: #fbf9f4; color: #2a2620; line-height: 1.65; padding: 64px 24px 80px; }
      .wrap { max-width: 680px; margin: 0 auto; }
      .eyebrow { font-family: 'JetBrains Mono', ui-monospace, monospace; font-size: 11px; letter-spacing: 0.12em; text-transform: uppercase; color: ${accent}; margin-bottom: 24px; }
      h1 { font-family: 'Lora', Georgia, serif; font-size: 44px; line-height: 1.1; letter-spacing: -0.02em; font-weight: 600; margin: 0 0 20px; color: #1a1714; }
      .lede { font-size: 20px; line-height: 1.5; color: #5a5248; margin: 0 0 32px; }
      .meta { font-family: 'JetBrains Mono', monospace; font-size: 12px; color: #8a7f70; display: flex; gap: 12px; margin-bottom: 48px; }
      hr { border: none; border-top: 1px solid #e8e0d0; margin: 48px 0; }
      h2 { font-size: 28px; margin: 56px 0 16px; letter-spacing: -0.01em; font-weight: 600; }
      h3 { font-size: 20px; margin: 36px 0 12px; font-weight: 600; }
      p { font-size: 17px; margin: 0 0 18px; }
      ul, ol { font-size: 17px; padding-left: 24px; }
      li { margin-bottom: 8px; }
      blockquote { border-left: 3px solid ${accent}; padding-left: 20px; margin: 28px 0; color: #5a5248; font-style: italic; }
      pre { background: #f3eee2; border: 1px solid #e8e0d0; border-radius: 6px; padding: 16px 18px; overflow-x: auto; font-family: 'JetBrains Mono', monospace; font-size: 13.5px; line-height: 1.5; color: #2a2620; }
      code { font-family: 'JetBrains Mono', monospace; font-size: 0.92em; background: #f3eee2; padding: 1px 5px; border-radius: 3px; }
      pre code { background: none; padding: 0; }
      table { width: 100%; border-collapse: collapse; margin: 24px 0; font-size: 14.5px; }
      th, td { text-align: left; padding: 10px 12px; border-bottom: 1px solid #e8e0d0; }
      th { font-weight: 600; color: #5a5248; font-family: 'JetBrains Mono', monospace; font-size: 12px; text-transform: uppercase; letter-spacing: 0.05em; }
      .foot { color: #8a7f70; font-family: 'JetBrains Mono', monospace; font-size: 12px; margin-top: 64px; padding-top: 24px; border-top: 1px solid #e8e0d0; }
    </style>
    <link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Lora:wght@400;500;600&family=JetBrains+Mono:wght@400;500&display=swap"/>
    </head><body><div class="wrap">
      <div class="eyebrow">${a.tags.join(' · ')}</div>
      <h1>${a.title}</h1>
      <p class="lede">${a.summary}</p>
      <div class="meta"><span>${a.words.toLocaleString()} words</span><span>·</span><span>${Math.round(a.words/240)} min read</span><span>·</span><span>updated ${a.age} ago</span></div>
      <hr/>
      <h2>The shape of the problem</h2>
      <p>Every approach to ${a.tags[0]} eventually runs into the same wall: composability across boundaries that the type system can&rsquo;t see. The good news is that we have a small set of patterns that handle the 80% case cleanly. The bad news is the 20% case is what your production system will hit at 3am.</p>
      <p>Let&rsquo;s walk through the canonical setup, then look at where it falls down.</p>
      <h3>A first pass</h3>
      <pre><code>async fn fetch_user(id: UserId) -&gt; Result&lt;User, Error&gt; {
    let conn = pool.get().await?;
    let row = conn.query_one("SELECT * FROM users WHERE id = $1", &amp;[&amp;id]).await?;
    Ok(User::from_row(row))
}</code></pre>
      <p>This is the form everyone reaches for, and it&rsquo;s correct. The <code>?</code> operator hides the actual shape of the error flow, which is fine until it isn&rsquo;t.</p>
      <h3>What goes wrong</h3>
      <ul>
        <li>Errors lose context as they bubble. By the time they reach the top, you&rsquo;ve lost the <em>which</em> of which user.</li>
        <li>Concurrent calls are easy to start, hard to coordinate. Cancellation discipline takes practice.</li>
        <li>The boundary between "recoverable" and "abort the request" lives in your head, not the types.</li>
      </ul>
      <blockquote><p>The compiler will save you from a category of bug. It will not save you from a confused mental model.</p></blockquote>
      <h3>A second pass with structure</h3>
      <pre><code>#[derive(thiserror::Error, Debug)]
enum FetchError {
    #[error("user {0} not found")]
    NotFound(UserId),
    #[error("database unavailable")]
    Db(#[from] sqlx::Error),
}</code></pre>
      <p>Now the call site can <code>match</code> on something meaningful. The <code>#[from]</code> propagation keeps the <code>?</code> ergonomics. You pay one enum variant per failure mode you actually care about, which is the right price.</p>
      <h2>Patterns worth memorizing</h2>
      <ol>
        <li><strong>Error-as-data, not error-as-string.</strong> Strings are for humans. Match arms are for code.</li>
        <li><strong>Boundary types.</strong> What leaves your crate is not the same as what flows internally.</li>
        <li><strong>Context, not wrapping.</strong> Use <code>.context()</code> at the seams, not at every layer.</li>
      </ol>
      <h2>Tradeoffs</h2>
      <table>
        <thead><tr><th>Approach</th><th>Allocations</th><th>Match-friendly</th><th>Boilerplate</th></tr></thead>
        <tbody>
          <tr><td>anyhow</td><td>1 box</td><td>no</td><td>none</td></tr>
          <tr><td>thiserror</td><td>0</td><td>yes</td><td>per variant</td></tr>
          <tr><td>hand-written</td><td>0</td><td>yes</td><td>a lot</td></tr>
        </tbody>
      </table>
      <h2>What to take away</h2>
      <p>Pick anyhow at the top of your binary. Pick thiserror at the top of your library. Don&rsquo;t mix them in the same module. The rest is taste.</p>
      <p class="foot">Last edited ${a.age} ago · ${a.backlinks} backlinks · ${a.outlinks} outgoing</p>
    </div></body></html>
  `;
  return <iframe ref={iframeRef} srcDoc={html} className="art-iframe" sandbox="allow-scripts allow-same-origin" title={a.title}/>;
}

function FloatingPill({ variant, onClose, onCmdK, onGraph, onCopyId, onDetails, detailsOpen, commentMode, onAnnotate, commentCount, onComments }) {
  const annBtn = (
    <button onClick={onAnnotate} className={commentMode ? 'is-on' : ''} title={commentMode ? 'annotating · click to stop' : 'annotate'}>
      <Icon.Pen/>
    </button>
  );
  const noteBtn = (
    <button onClick={onComments} className="fp-note" title={`comments · ${commentCount || 0}`}>
      <Icon.Note/>{commentCount > 0 && <span className="fp-badge">{commentCount}</span>}
    </button>
  );

  if (variant === 'capsule') {
    return (
      <div className="float-pill float-pill--capsule">
        <button onClick={onClose} title="back to gallery"><Icon.ArrowLeft/> <span>gallery</span></button>
        <span className="fp-sep"/>
        <button onClick={onCmdK} title="search"><Icon.Search/></button>
        <button onClick={onGraph} title="open in graph"><Icon.Graph/></button>
        <button onClick={onCopyId} title="copy id"><Icon.Copy/></button>
        <span className="fp-sep"/>
        {annBtn}
        {noteBtn}
        <span className="fp-sep"/>
        <button onClick={onDetails} className={detailsOpen ? 'is-on' : ''} title="details">
          <span>details</span> <Icon.Chevron/>
        </button>
      </div>
    );
  }
  if (variant === 'dock') {
    return (
      <div className="float-pill float-pill--dock">
        <button onClick={onClose}><Icon.ArrowLeft/></button>
        <button onClick={onCmdK}><Icon.Search/></button>
        <button onClick={onGraph}><Icon.Graph/></button>
        {annBtn}
        {noteBtn}
        <button onClick={onCopyId}><Icon.Copy/></button>
        <button onClick={onDetails} className={detailsOpen ? 'is-on' : ''}><Icon.More/></button>
      </div>
    );
  }
  if (variant === 'topright') {
    return (
      <div className="float-pill float-pill--topright">
        <button onClick={onClose} title="back"><Icon.ArrowLeft/></button>
        <button onClick={onCmdK} title="search"><Icon.Search/></button>
        {annBtn}
        {noteBtn}
        <button onClick={onGraph} title="graph"><Icon.Graph/></button>
        <button onClick={onDetails} title="details" className={detailsOpen ? 'is-on' : ''}><Icon.More/></button>
      </div>
    );
  }
  return (
    <div className="float-pill float-pill--rail">
      <button onClick={onClose} title="back"><Icon.ArrowLeft/></button>
      <button onClick={onCmdK} title="search"><Icon.Search/></button>
      <button onClick={onGraph} title="graph"><Icon.Graph/></button>
      <button onClick={onCopyId} title="copy id"><Icon.Copy/></button>
      <span className="fp-sep-v"/>
      {annBtn}
      {noteBtn}
      <span className="fp-sep-v"/>
      <button onClick={onDetails} title="details" className={detailsOpen ? 'is-on' : ''}><Icon.More/></button>
    </div>
  );
}

function DetailsPanel({ a, onClose, onOpen }) {
  if (!a) return null;
  // Compute backlinks/related from data
  const all = window.ARTIFACTS;
  const backlinks = all.filter(x => x.id !== a.id && x.tags.some(t => a.tags.includes(t))).slice(0, 5);
  const related = all.filter(x => x.id !== a.id && x.accentTag === a.accentTag).slice(0, 4);
  return (
    <aside className="details-panel">
      <div className="dp-head">
        <span className="dp-eyebrow">artifact</span>
        <button className="dp-close" onClick={onClose}><Icon.X/></button>
      </div>
      <h2 className="dp-title">{a.title}</h2>
      <dl className="dp-meta">
        <div><dt>kb</dt><dd>{a.kb}</dd></div>
        <div><dt>id</dt><dd className="mono">{a.id}</dd></div>
        <div><dt>path</dt><dd className="mono">kb/{a.kb}/{a.id}.html</dd></div>
        <div><dt>updated</dt><dd>{a.age} ago</dd></div>
        <div><dt>words</dt><dd>{a.words.toLocaleString()}</dd></div>
      </dl>
      <div className="dp-section">
        <div className="dp-h">tags</div>
        <div className="dp-tags">{a.tags.map(t => <TagPill key={t} tag={t} accent={t === a.accentTag}/>)}</div>
      </div>
      <div className="dp-section">
        <div className="dp-h">backlinks · {backlinks.length}</div>
        <ul className="dp-list">
          {backlinks.map(b => (
            <li key={b.id}><button onClick={() => onOpen(b.id)}>{b.title}<span className="dp-list-meta">{b.tags.slice(0,2).join(' · ')}</span></button></li>
          ))}
        </ul>
      </div>
      <div className="dp-section">
        <div className="dp-h">related <span className="dp-h-tag">vector</span></div>
        <ul className="dp-list">
          {related.map(b => (
            <li key={b.id}><button onClick={() => onOpen(b.id)}>{b.title}<span className="dp-list-meta">{b.words.toLocaleString()}w · {b.tags.slice(0,2).join(' · ')}</span></button></li>
          ))}
        </ul>
      </div>
      <div className="dp-section">
        <div className="dp-h">actions</div>
        <div className="dp-actions">
          <button>open in editor</button>
          <button>regenerate</button>
          <button>copy permalink</button>
          <button className="danger">delete</button>
        </div>
      </div>
    </aside>
  );
}

function PagesPanel({ a, currentIdx, onPick }) {
  if (!a.pages || a.pages.length < 2) return null;
  return (
    <aside className="pages-panel">
      <div className="pp-head">
        <span className="pp-eyebrow">artifact · {a.pages.length} files</span>
        <span className="pp-id mono">{a.id}</span>
      </div>
      <ol className="pp-list">
        {a.pages.map((p, i) => (
          <li key={p.src} className={i === currentIdx ? 'is-current' : ''}>
            <button onClick={() => onPick(i)}>
              <span className="pp-num mono">{String(i + 1).padStart(2, '0')}</span>
              <span className="pp-body">
                <span className="pp-label">{p.label}</span>
                <span className="pp-file mono">{p.file}</span>
              </span>
              {i === currentIdx && <span className="pp-dot"/>}
            </button>
          </li>
        ))}
      </ol>
      <div className="pp-foot mono">
        <button
          className="pp-step"
          disabled={currentIdx <= 0}
          onClick={() => onPick(currentIdx - 1)}
          title="prev page"
        >←</button>
        <span>page {currentIdx + 1} / {a.pages.length}</span>
        <button
          className="pp-step"
          disabled={currentIdx >= a.pages.length - 1}
          onClick={() => onPick(currentIdx + 1)}
          title="next page"
        >→</button>
      </div>
    </aside>
  );
}

function ArtifactDetail({ a, onClose, onOpen, onCmdK, onGraph, pillVariant }) {
  const [detailsOpen, setDetailsOpen] = useState_d(false);
  const [pageIdx, setPageIdx] = useState_d(0);
  const [crumb, setCrumb] = useState_d(true);
  const [commentMode, setCommentMode] = useState_d(false);
  const [commentsOpen, setCommentsOpen] = useState_d(false);
  const [exportOpen, setExportOpen] = useState_d(false);
  const iframeRef = useRef_d(null);
  const {
    comments, add: addComment, remove: removeComment, clear: clearComments,
    editBody, setStatus, addReply, editReply, removeReply,
  } = window.useComments(a.id);
  const currentFile = a.pages ? a.pages[pageIdx].src : (a.realSrc || 'main');
  const currentLabel = a.pages ? a.pages[pageIdx].label : 'main';
  useEffect_d(() => { setPageIdx(0); }, [a.id]);
  useEffect_d(() => {
    const t = setTimeout(() => setCrumb(false), 2200);
    return () => clearTimeout(t);
  }, [a.id]);

  // Inject annotator into iframe when commentMode is on
  useEffect_d(() => {
    if (!commentMode) return;
    const iframe = iframeRef.current;
    if (!iframe) return;
    let cleanup = () => {};
    const setup = () => {
      const existing = comments.filter(c => c.file === currentFile);
      cleanup = window.injectCommentBridge(iframe, {
        artifactId: a.id, file: currentFile, label: currentLabel, existing,
      }) || (() => {});
    };
    if (iframe.contentDocument && iframe.contentDocument.readyState === 'complete') setup();
    else iframe.addEventListener('load', setup, { once: true });
    return () => { cleanup(); iframe.removeEventListener && iframe.removeEventListener('load', setup); };
  }, [commentMode, a.id, pageIdx]);

  // Refresh in-iframe annotation marks when comments list changes
  useEffect_d(() => {
    if (!commentMode) return;
    const iframe = iframeRef.current;
    if (iframe && iframe.__cmRefresh) {
      iframe.__cmRefresh(comments.filter(c => c.file === currentFile));
    }
  }, [comments, commentMode, currentFile]);

  // Receive cm:add posts from inside the iframe
  useEffect_d(() => {
    const onMsg = (e) => {
      const d = e.data;
      if (d && d.type === 'cm:add' && d.artifactId === a.id) {
        addComment({ file: d.file, fileLabel: d.fileLabel, kind: d.kind, tag: d.tag, ref: d.ref, snippet: d.snippet, body: d.body || d.comment });
        setCommentsOpen(true);
      }
    };
    window.addEventListener('message', onMsg);
    return () => window.removeEventListener('message', onMsg);
  }, [a.id, addComment]);

  // Sync sub-page when an iframe navigates internally (postMessage handshake)
  useEffect_d(() => {
    if (!a.pages) return;
    const onMsg = (e) => {
      const d = e.data;
      if (d && d.type === 'pm:page' && typeof d.page === 'number') {
        setPageIdx(d.page);
      }
    };
    window.addEventListener('message', onMsg);
    return () => window.removeEventListener('message', onMsg);
  }, [a.id, a.pages]);

  const currentSrc = a.pages ? a.pages[pageIdx]?.src : a.realSrc;

  const onMouseMove = (e) => {
    if (e.clientY < 80) setCrumb(true);
  };
  useEffect_d(() => {
    let timer;
    if (crumb) {
      timer = setTimeout(() => setCrumb(false), 2200);
    }
    return () => clearTimeout(timer);
  }, [crumb]);

  const [toast, setToast] = useState_d(null);
  const copyId = () => { setToast(`copied ${a.id}`); setTimeout(() => setToast(null), 1600); };

  return (
    <div className="detail" onMouseMove={onMouseMove}>
      <div className={`detail-crumb ${crumb ? 'is-show' : ''}`}>
        <button onClick={onClose}>{a.kb}</button>
        <Icon.Chevron/>
        <span className="detail-crumb-title">{a.title}</span>
      </div>
      <div className={`detail-stage ${detailsOpen ? 'with-panel' : ''} ${commentsOpen ? 'with-comments' : ''} ${a.pages ? 'with-pages' : ''}`}>
        <div className="detail-iframe-wrap">
          <ArtifactIframe a={a} src={currentSrc} iframeRef={iframeRef} key={a.id + ':' + pageIdx}/>
        </div>
        {a.pages && <PagesPanel a={a} currentIdx={pageIdx} onPick={setPageIdx}/>}
        {commentsOpen && <window.CommentsPanel
          a={a}
          comments={comments}
          currentFile={currentFile}
          onClose={() => setCommentsOpen(false)}
          onRemove={removeComment}
          onClear={clearComments}
          onExport={() => setExportOpen(true)}
          onEdit={editBody}
          onSetStatus={setStatus}
          onAddReply={addReply}
          onEditReply={editReply}
          onRemoveReply={removeReply}
          commentMode={commentMode}
          onToggleMode={() => setCommentMode(v => !v)}
          onJump={(c) => {
            // Cross-file jump: switch page first if needed
            const doJump = () => {
              const iframe = iframeRef.current;
              if (!iframe) return;
              if (iframe.__cmFlash) {
                if (!iframe.__cmFlash(c.ref)) {
                  // If bridge isn't set up (annotate mode off), fall back to a manual scroll
                  const doc = iframe.contentDocument;
                  const el = doc && doc.querySelector(`[data-cm-id="${c.ref}"]`);
                  if (el) el.scrollIntoView({ block: 'center', behavior: 'smooth' });
                }
              } else {
                const doc = iframe.contentDocument;
                const el = doc && doc.querySelector(`[data-cm-id="${c.ref}"]`);
                if (el) {
                  el.scrollIntoView({ block: 'center', behavior: 'smooth' });
                  el.style.transition = 'outline 0.2s';
                  el.style.outline = `2px solid ${getComputedStyle(document.documentElement).getPropertyValue('--accent')}`;
                  setTimeout(() => { el.style.outline = ''; }, 1400);
                }
              }
            };
            if (a.pages && c.file !== currentFile) {
              const idx = a.pages.findIndex(p => p.src === c.file);
              if (idx >= 0) {
                setPageIdx(idx);
                // Wait for iframe to load before flashing
                setTimeout(doJump, 700);
                return;
              }
            }
            doJump();
          }}
        />}
        {detailsOpen && <DetailsPanel a={a} onClose={() => setDetailsOpen(false)} onOpen={onOpen}/>}
      </div>
      <FloatingPill
        variant={(commentsOpen || detailsOpen) && (pillVariant === 'rail' || pillVariant === 'topright') ? 'capsule' : pillVariant}
        onClose={onClose}
        onCmdK={onCmdK}
        onGraph={onGraph}
        onCopyId={copyId}
        onDetails={() => setDetailsOpen(v => !v)}
        detailsOpen={detailsOpen}
        commentMode={commentMode}
        onAnnotate={() => { setCommentMode(v => !v); setCommentsOpen(true); }}
        commentCount={comments.length}
        onComments={() => setCommentsOpen(v => !v)}
      />
      {exportOpen && <window.ExportModal a={a} comments={comments} onClose={() => setExportOpen(false)}/>}
      {toast && <div className="toast">{toast}</div>}
    </div>
  );
}

window.ArtifactDetail = ArtifactDetail;
