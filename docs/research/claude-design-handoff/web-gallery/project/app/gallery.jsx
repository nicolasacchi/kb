// Gallery view — top bar, left rail, virtualized-feel grid

const { useState, useMemo, useEffect, useRef } = React;

function GalleryTopBar({ q, setQ, view, setView, onOpenCmdK, kb, onKbClick, onSettings }) {
  return (
    <div className="topbar">
      <div className="topbar-left">
        <button className="kb-selector" onClick={onKbClick}>
          <span className="kb-mark">
            <svg viewBox="0 0 16 16" width="16" height="16"><path d="M3 2v12M3 8l5-5M3 8l5 5" stroke="currentColor" strokeWidth="1.6" fill="none" strokeLinecap="square"/></svg>
          </span>
          <span className="kb-name">{kb}</span>
          <Icon.ChevDown/>
        </button>
        <span className="topbar-sep">/</span>
        <span className="topbar-path">work</span>
      </div>
      <div className="topbar-search" onClick={onOpenCmdK}>
        <Icon.Search/>
        <span className="topbar-search-placeholder">Search artifacts…</span>
        <kbd className="kbd">⌘K</kbd>
      </div>
      <div className="topbar-right">
        <div className="view-toggle">
          <button className={view === 'grid' ? 'is-on' : ''} onClick={() => setView('grid')} title="Grid"><Icon.Grid/></button>
          <button className={view === 'list' ? 'is-on' : ''} onClick={() => setView('list')} title="List"><Icon.List/></button>
          <button className={view === 'atlas' ? 'is-on' : ''} onClick={() => setView('atlas')} title="Atlas">
            <svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" strokeWidth="1.5">
              <circle cx="4" cy="4" r="1.6"/><circle cx="12" cy="5" r="1.6"/><circle cx="8" cy="11" r="1.6"/>
              <path d="M5 5l5 0M5 5l3 5M11 6l-2 4" strokeLinecap="round"/>
            </svg>
          </button>
        </div>
        <button className="icon-btn" onClick={onSettings} title="Settings"><Icon.Settings/></button>
      </div>
    </div>
  );
}

function LeftRail({ active, onChange, caps, setCaps, dateRange, setDateRange }) {
  return (
    <aside className="leftrail">
      <div className="rail-section">
        <div className="rail-h">tags</div>
        <ul className="rail-tags">
          {window.TOP_TAGS.map(([t, n]) => (
            <li key={t}>
              <button className={`rail-tag ${active.has(t) ? 'is-on' : ''}`} onClick={() => onChange(t)}>
                <span className="rail-tag-dot" style={{ background: window.TAG_COLORS[t] || 'var(--muted)' }}/>
                <span className="rail-tag-name">{t}</span>
                <span className="rail-tag-n">{n}</span>
              </button>
            </li>
          ))}
        </ul>
      </div>
      <div className="rail-section">
        <div className="rail-h">capabilities</div>
        <ul className="rail-caps">
          {[
            ['svg', 'visual / svg'],
            ['interactive', 'interactive'],
            ['longread', 'long-read'],
            ['code', 'code-heavy'],
          ].map(([k, label]) => (
            <li key={k}>
              <label className="rail-cap">
                <input type="checkbox" checked={!!caps[k]} onChange={e => setCaps({ ...caps, [k]: e.target.checked })}/>
                <span>{label}</span>
              </label>
            </li>
          ))}
        </ul>
      </div>
      <div className="rail-section">
        <div className="rail-h">date</div>
        <div className="rail-date">
          {[['7', '7d'], ['30', '30d'], ['all', 'all']].map(([v, l]) => (
            <label key={v} className={`rail-date-opt ${dateRange === v ? 'is-on' : ''}`}>
              <input type="radio" name="date" checked={dateRange === v} onChange={() => setDateRange(v)}/>
              <span>{l}</span>
            </label>
          ))}
        </div>
      </div>
      <div className="rail-foot">
        <span>{window.ARTIFACTS.length} artifacts</span>
      </div>
    </aside>
  );
}

function Gallery({ tweaks, onOpenArtifact, onOpenCmdK, onSettings }) {
  const [view, setView] = useState('grid');
  const [activeTags, setActiveTags] = useState(new Set());
  const [caps, setCaps] = useState({});
  const [dateRange, setDateRange] = useState('30');
  const [q, setQ] = useState('');

  const toggleTag = (t) => {
    const n = new Set(activeTags);
    if (n.has(t)) n.delete(t); else n.add(t);
    setActiveTags(n);
  };

  const filtered = useMemo(() => {
    return window.ARTIFACTS.filter(a => {
      if (activeTags.size > 0 && !a.tags.some(t => activeTags.has(t))) return false;
      if (caps.svg && !a.indicators.svg) return false;
      if (caps.interactive && !a.indicators.interactive) return false;
      if (caps.longread && !a.indicators.longread) return false;
      if (caps.code && a.indicators.code < 8) return false;
      if (dateRange === '7' && a.mtime > 7) return false;
      if (dateRange === '30' && a.mtime > 30) return false;
      return true;
    });
  }, [activeTags, caps, dateRange]);

  const cardStyle = tweaks.cardStyle;
  const density = tweaks.density;
  const cols = tweaks.cols;

  return (
    <div className="gallery">
      <GalleryTopBar
        view={view} setView={setView}
        onOpenCmdK={onOpenCmdK}
        kb={tweaks.kbName || 'work'}
        onSettings={onSettings}
      />
      <div className={`gallery-body ${tweaks.showRail ? '' : 'no-rail'}`}>
        {tweaks.showRail && (
          <LeftRail
            active={activeTags} onChange={toggleTag}
            caps={caps} setCaps={setCaps}
            dateRange={dateRange} setDateRange={setDateRange}
          />
        )}
        <main className="gallery-main">
          <div className="gallery-header">
            <h1 className="gallery-h1">
              {activeTags.size > 0
                ? <>tagged <span className="g-h1-accent">{[...activeTags].join(' + ')}</span></>
                : <>recent</>}
            </h1>
            <div className="gallery-count">{filtered.length} of {window.ARTIFACTS.length}</div>
          </div>
          {view === 'grid' && (
            <div className="card-grid" style={{ '--cols': cols }}>
              {filtered.map(a => (
                <Card key={a.id} a={a} style={cardStyle} density={density}
                      onClick={() => onOpenArtifact(a.id)}/>
              ))}
            </div>
          )}
          {view === 'list' && (
            <div className="list-view">
              {filtered.map(a => (
                <ListRow key={a.id} a={a} onClick={() => onOpenArtifact(a.id)}/>
              ))}
            </div>
          )}
          {view === 'atlas' && (
            <AtlasView artifacts={filtered} activeTags={activeTags}
              onOpenArtifact={onOpenArtifact} kb={tweaks.kbName || 'work'}/>
          )}
          {filtered.length === 0 && (
            <div className="gallery-empty">
              <div className="gallery-empty-h">no matches</div>
              <p>try clearing filters, or <button className="link" onClick={onOpenCmdK}>search across kbs</button>.</p>
            </div>
          )}
        </main>
      </div>
    </div>
  );
}

window.Gallery = Gallery;
