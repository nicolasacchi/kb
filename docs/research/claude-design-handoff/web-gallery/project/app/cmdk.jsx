// Cmd+K search overlay + status pill

const { useState: useStateK, useEffect: useEffectK, useRef: useRefK } = React;

function CmdK({ open, onClose, onOpen }) {
  const [q, setQ] = useStateK('');
  const [mode, setMode] = useStateK('hybrid');
  const [sel, setSel] = useStateK(0);
  const inputRef = useRefK(null);

  useEffectK(() => {
    if (open) {
      setQ('');
      setSel(0);
      setTimeout(() => inputRef.current?.focus(), 30);
    }
  }, [open]);

  // Parse prefix filters: tag:rust, kb:work, since:7d
  const parsed = useStateK.parse ? null : (() => {
    const tokens = q.split(/\s+/).filter(Boolean);
    const filters = {};
    const terms = [];
    tokens.forEach(t => {
      const m = t.match(/^(tag|kb|since|mode):(.+)$/);
      if (m) filters[m[1]] = m[2]; else terms.push(t);
    });
    return { filters, terms: terms.join(' ').toLowerCase() };
  })();

  const results = (() => {
    if (!q && !open) return [];
    const tokens = q.split(/\s+/).filter(Boolean);
    const filters = {};
    const terms = [];
    tokens.forEach(t => {
      const m = t.match(/^(tag|kb|since|mode):(.+)$/);
      if (m) filters[m[1]] = m[2]; else terms.push(t);
    });
    const term = terms.join(' ').toLowerCase();
    let pool = window.ARTIFACTS;
    if (filters.tag) pool = pool.filter(a => a.tags.includes(filters.tag));
    if (filters.kb) pool = pool.filter(a => a.kb === filters.kb);
    if (!term) return pool.slice(0, 6).map(a => ({ a, score: 0.5, hl: a.summary }));
    return pool
      .map(a => {
        const hay = (a.title + ' ' + a.summary + ' ' + a.tags.join(' ')).toLowerCase();
        if (!hay.includes(term)) return null;
        const titleHit = a.title.toLowerCase().includes(term) ? 0.4 : 0;
        const tagHit = a.tags.some(t => t.includes(term)) ? 0.2 : 0;
        const score = Math.min(0.99, 0.5 + titleHit + tagHit + Math.random() * 0.1);
        return { a, score, hl: highlight(a.summary, term) };
      })
      .filter(Boolean)
      .sort((x, y) => y.score - x.score)
      .slice(0, 8);
  })();

  function highlight(text, term) {
    if (!term) return text;
    const i = text.toLowerCase().indexOf(term);
    if (i < 0) return text;
    return (
      <>
        {text.slice(0, i)}
        <mark>{text.slice(i, i + term.length)}</mark>
        {text.slice(i + term.length)}
      </>
    );
  }

  const onKey = (e) => {
    if (e.key === 'Escape') { onClose(); return; }
    if (e.key === 'ArrowDown') { e.preventDefault(); setSel(s => Math.min(results.length - 1, s + 1)); }
    if (e.key === 'ArrowUp') { e.preventDefault(); setSel(s => Math.max(0, s - 1)); }
    if (e.key === 'Tab') { e.preventDefault(); setMode(m => m === 'hybrid' ? 'keyword' : m === 'keyword' ? 'semantic' : 'hybrid'); }
    if (e.key === 'Enter' && results[sel]) { onOpen(results[sel].a.id); onClose(); }
  };

  if (!open) return null;
  const modeBadge = mode === 'hybrid' ? 'k+v' : mode === 'keyword' ? 'k' : 'v';
  const modeLabel = mode === 'hybrid' ? 'hybrid' : mode === 'keyword' ? 'keyword' : 'semantic';

  return (
    <div className="cmdk-backdrop" onClick={onClose}>
      <div className="cmdk-panel" onClick={e => e.stopPropagation()} onKeyDown={onKey}>
        <div className="cmdk-input-wrap">
          <Icon.Search/>
          <input
            ref={inputRef}
            value={q}
            onChange={e => { setQ(e.target.value); setSel(0); }}
            placeholder="search artifacts… try tag:rust or kb:work"
            className="cmdk-input"
          />
          <button className={`cmdk-mode mode-${mode}`} onClick={() => setMode(m => m === 'hybrid' ? 'keyword' : m === 'keyword' ? 'semantic' : 'hybrid')}>
            <span className="cmdk-mode-dot"/>
            <span className="cmdk-mode-label">{modeLabel}</span>
            <kbd>tab</kbd>
          </button>
        </div>
        <div className="cmdk-results">
          {results.length === 0 && q && (
            <div className="cmdk-empty">no matches. try fewer terms, or <code>mode:semantic</code>.</div>
          )}
          {results.length === 0 && !q && (
            <div className="cmdk-empty">type to search across {window.ARTIFACTS.length} artifacts in 3 kbs.</div>
          )}
          {results.map((r, i) => (
            <button
              key={r.a.id}
              className={`cmdk-row ${i === sel ? 'is-sel' : ''}`}
              onMouseEnter={() => setSel(i)}
              onClick={() => { onOpen(r.a.id); onClose(); }}
            >
              <span className="cmdk-row-icon"><Icon.Doc/></span>
              <div className="cmdk-row-body">
                <div className="cmdk-row-title">{r.a.title}</div>
                <div className="cmdk-row-summary">{r.hl}</div>
                <div className="cmdk-row-tags">
                  {r.a.tags.slice(0, 3).map(t => (
                    <span key={t} className="cmdk-row-tag" style={{ color: window.TAG_COLORS[t] }}>{t}</span>
                  ))}
                  <span className="cmdk-row-kb">in {r.a.kb}</span>
                </div>
              </div>
              <div className="cmdk-row-score">
                <div className="cmdk-row-score-bar"><div style={{ width: (r.score * 100) + '%' }}/></div>
                <div className="cmdk-row-score-n">{r.score.toFixed(2)}</div>
              </div>
            </button>
          ))}
        </div>
        <div className="cmdk-foot">
          <span><Icon.Settings/> searching {window.ARTIFACTS.length} artifacts in 3 kbs</span>
          <span className="cmdk-shortcuts">
            <kbd>↵</kbd> open <kbd>tab</kbd> mode <kbd>esc</kbd> dismiss
          </span>
        </div>
      </div>
    </div>
  );
}

function StatusPill() {
  const [phase, setPhase] = useStateK('idle');
  const [progress, setProgress] = useStateK({ done: 47, total: 89 });
  const [open, setOpen] = useStateK(false);
  const [events, setEvents] = useStateK([
    { t: 'index.progress', msg: '47/89 indexed', age: '2s' },
    { t: 'watcher.event', msg: '+1 artifact: Tokio Task Scheduling', age: '1m' },
    { t: 'index.start', msg: 'work · 89 artifacts', age: '1m' },
  ]);

  // Simulated SSE: cycle through phases
  useEffectK(() => {
    let i = 0;
    const cycle = () => {
      i++;
      if (i % 8 === 1) {
        setPhase('indexing');
        setProgress({ done: 0, total: 89 });
      } else if (i % 8 < 7 && phase !== 'idle') {
        setProgress(p => ({ ...p, done: Math.min(p.total, p.done + Math.ceil(89/7)) }));
      } else {
        setPhase('idle');
      }
    };
    const id = setInterval(cycle, 1500);
    return () => clearInterval(id);
  }, []);

  if (phase === 'idle' && !open) {
    return (
      <button className="status-pill is-idle" onClick={() => setOpen(o => !o)} title="status">
        <span className="sp-dot"/>
      </button>
    );
  }

  return (
    <>
      {open && (
        <div className="sp-drop">
          <div className="sp-drop-h">recent events</div>
          <ul className="sp-drop-list">
            {events.map((e, i) => (
              <li key={i}>
                <span className={`sp-evt sp-evt-${e.t.split('.')[0]}`}>{e.t}</span>
                <span className="sp-evt-msg">{e.msg}</span>
                <span className="sp-evt-age">{e.age}</span>
              </li>
            ))}
          </ul>
          <div className="sp-drop-foot">
            <button>view errors</button>
            <button>open in tui</button>
          </div>
        </div>
      )}
      <button className={`status-pill is-${phase}`} onClick={() => setOpen(o => !o)}>
        <span className="sp-spinner"/>
        <span className="sp-text">
          {phase === 'indexing'
            ? <>indexing <span className="sp-num">{progress.done}/{progress.total}</span> · work</>
            : 'idle'}
        </span>
      </button>
    </>
  );
}

window.CmdK = CmdK;
window.StatusPill = StatusPill;
