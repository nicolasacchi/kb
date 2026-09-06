// Card variants — 5 preview styles per spec
// Shared: title, summary, tags, age. Variants differ in preview area.

const TAG_COLORS = window.TAG_COLORS;

function relTime(s) { return s; }

function TagPill({ tag, accent }) {
  const c = TAG_COLORS[tag] || 'var(--muted)';
  return (
    <span className="tag-pill" style={{
      borderColor: accent ? c : 'var(--border)',
      color: accent ? c : 'var(--muted)',
    }}>{tag}</span>
  );
}

// ── Variant 1: Synthetic icon collage (spec v1) ─────────────────────────────
function PreviewCollage({ a }) {
  const ind = a.indicators;
  const items = [];
  if (ind.svg > 0) items.push({ type: 'svg', n: ind.svg });
  if (ind.code > 0) items.push({ type: 'code', n: ind.code });
  if (ind.tables > 0) items.push({ type: 'table', n: ind.tables });
  if (ind.interactive > 0) items.push({ type: 'spark', n: ind.interactive });
  if (ind.longread > 0) items.push({ type: 'book', n: ind.longread });
  const accent = TAG_COLORS[a.accentTag];
  return (
    <div className="prev prev-collage" style={{ '--accent-tag': accent }}>
      <div className="prev-collage-grid">
        {items.slice(0, 5).map((it, i) => {
          const I = { svg: Icon.Spark, code: Icon.Code, table: Icon.Table, spark: Icon.Spark, book: Icon.BookOpen }[it.type];
          return (
            <div key={i} className="prev-coll-cell">
              <I/>
              {it.n > 1 && <span className="prev-coll-n">{it.n}</span>}
            </div>
          );
        })}
      </div>
      <div className="prev-coll-words">{a.words.toLocaleString()} words</div>
    </div>
  );
}

// ── Variant 2: Big title, no preview ────────────────────────────────────────
function PreviewBigTitle({ a }) {
  const accent = TAG_COLORS[a.accentTag];
  return (
    <div className="prev prev-bigtitle" style={{ '--accent-tag': accent }}>
      <div className="prev-bt-title">{a.title}</div>
      <div className="prev-bt-meta">
        <span>{a.words.toLocaleString()}w</span>
        <span className="dotsep">·</span>
        <span>{a.backlinks}↩</span>
      </div>
    </div>
  );
}

// ── Variant 3: Real screenshot (synthetic mockup) ───────────────────────────
function PreviewScreenshot({ a }) {
  // Synthesize a "screenshot" by drawing fake article content
  const accent = TAG_COLORS[a.accentTag];
  const seed = a.id.charCodeAt(1) + a.id.charCodeAt(2);
  const lines = 14;
  return (
    <div className="prev prev-shot">
      <div className="prev-shot-page">
        <div className="prev-shot-h" style={{ background: accent }}/>
        <div className="prev-shot-title-line"/>
        <div className="prev-shot-title-line short"/>
        <div className="prev-shot-gap"/>
        {Array.from({length: lines}).map((_, i) => {
          const w = 40 + ((seed * (i + 3)) % 55);
          return <div key={i} className="prev-shot-line" style={{width: w + '%'}}/>;
        })}
        {a.indicators.code > 0 && (
          <div className="prev-shot-code">
            <div style={{width: '70%'}}/>
            <div style={{width: '50%'}}/>
            <div style={{width: '85%'}}/>
          </div>
        )}
      </div>
    </div>
  );
}

// ── Variant 4: Code / terminal aesthetic ────────────────────────────────────
function PreviewTerminal({ a }) {
  const accent = TAG_COLORS[a.accentTag];
  const path = `kb/${a.kb}/${a.id}.html`;
  return (
    <div className="prev prev-term" style={{ '--accent-tag': accent }}>
      <div className="prev-term-bar">
        <span className="prev-term-dot"/><span className="prev-term-dot"/><span className="prev-term-dot"/>
        <span className="prev-term-path">{path}</span>
      </div>
      <div className="prev-term-body">
        <div><span className="tk-com">// {a.tags.join(' · ')}</span></div>
        <div><span className="tk-kw">title</span> <span className="tk-op">=</span> <span className="tk-str">"{a.title}"</span></div>
        <div><span className="tk-kw">words</span> <span className="tk-op">=</span> <span className="tk-num">{a.words.toLocaleString()}</span></div>
        <div><span className="tk-kw">links</span> <span className="tk-op">=</span> <span className="tk-num">{a.backlinks}</span><span className="tk-op">↩</span> <span className="tk-num">{a.outlinks}</span><span className="tk-op">↪</span></div>
        <div><span className="tk-kw">caps</span>  <span className="tk-op">=</span> {a.indicators.svg > 0 && <span className="tk-cap">svg</span>}{a.indicators.interactive > 0 && <span className="tk-cap">interactive</span>}{a.indicators.longread > 0 && <span className="tk-cap">long</span>}</div>
        <div><span className="tk-prompt" style={{color: accent}}>▸</span> <span className="tk-cursor"/></div>
      </div>
    </div>
  );
}

// ── Variant 5: Abstract gradient by tag ─────────────────────────────────────
function PreviewAbstract({ a }) {
  const c1 = TAG_COLORS[a.tags[0]] || 'oklch(0.7 0.1 240)';
  const c2 = TAG_COLORS[a.tags[1]] || c1;
  const c3 = TAG_COLORS[a.tags[2]] || c2;
  const seed = a.id.charCodeAt(1) * 17 + a.id.charCodeAt(2);
  const angle = seed % 360;
  return (
    <div className="prev prev-abstract">
      <div className="prev-abs-base" style={{
        background: `linear-gradient(${angle}deg, ${c1}, ${c2})`,
      }}/>
      <div className="prev-abs-blob1" style={{ background: c3, transform: `translate(${seed % 30}%, ${(seed*3) % 25}%)` }}/>
      <div className="prev-abs-blob2" style={{ background: c1, transform: `translate(${(seed*2) % 40 - 10}%, ${(seed*7) % 35 - 5}%)` }}/>
      <div className="prev-abs-glyph">{a.title.charAt(0)}</div>
    </div>
  );
}

// ── Variant 6: Hybrid — big title + a slim collage strip ────────────────────
function PreviewHybrid({ a }) {
  const ind = a.indicators;
  const items = [];
  if (ind.svg > 0) items.push({ I: Icon.Spark, n: ind.svg });
  if (ind.code > 0) items.push({ I: Icon.Code, n: ind.code });
  if (ind.tables > 0) items.push({ I: Icon.Table, n: ind.tables });
  if (ind.interactive > 0) items.push({ I: Icon.Spark, n: ind.interactive });
  if (ind.longread > 0) items.push({ I: Icon.BookOpen, n: ind.longread });
  const accent = TAG_COLORS[a.accentTag];
  return (
    <div className="prev prev-hybrid" style={{ '--accent-tag': accent }}>
      <div className="prev-hyb-title">{a.title}</div>
      <div className="prev-hyb-strip">
        <div className="prev-hyb-glyphs">
          {items.slice(0, 4).map((it, i) => (
            <span key={i} className="prev-hyb-glyph"><it.I/>{it.n > 1 && <span className="prev-hyb-n">{it.n}</span>}</span>
          ))}
        </div>
        <div className="prev-hyb-words">{a.words.toLocaleString()}w · {a.backlinks}↩</div>
      </div>
    </div>
  );
}

const PREVIEWS = {
  hybrid: PreviewHybrid,
  collage: PreviewCollage,
  bigtitle: PreviewBigTitle,
  screenshot: PreviewScreenshot,
  terminal: PreviewTerminal,
  abstract: PreviewAbstract,
};

function Card({ a, style = 'collage', density = 'comfortable', focused, onClick }) {
  const Prev = PREVIEWS[style] || PreviewCollage;
  const titleInPreview = style === 'bigtitle' || style === 'hybrid';
  return (
    <button
      className={`card card-${density} ${focused ? 'is-focused' : ''}`}
      onClick={onClick}
      data-style={style}
    >
      {a.isNew && <span className="card-new-dot" aria-label="new"/>}
      {a.pages && a.pages.length > 1 && (
        <span className="card-pages-badge" title={`${a.pages.length}-file artifact`}>
          <svg viewBox="0 0 12 12" fill="none"><rect x="2.5" y="0.5" width="7" height="9" rx="1" stroke="currentColor"/><rect x="0.5" y="2.5" width="7" height="9" rx="1" stroke="currentColor" fill="var(--bg-2)"/></svg>
          {a.pages.length} files
        </span>
      )}
      <Prev a={a}/>
      <div className="card-body">
        {!titleInPreview && <div className="card-title">{a.title}</div>}
        <div className="card-summary">{a.summary}</div>
        <div className="card-meta">
          <div className="card-tags">
            {a.tags.slice(0, 3).map(t => <TagPill key={t} tag={t} accent={t === a.accentTag}/>)}
          </div>
          <span className="card-age">{a.age}</span>
        </div>
      </div>
    </button>
  );
}

function ListRow({ a, onClick }) {
  return (
    <button className="list-row" onClick={onClick}>
      <span className="list-row-icon"><Icon.Doc/></span>
      <span className="list-row-title">{a.title}{a.isNew && <span className="card-new-dot inline"/>}{a.pages && a.pages.length > 1 && <span className="card-pages-badge" style={{marginLeft:8,verticalAlign:1}}>{a.pages.length} files</span>}</span>
      <span className="list-row-summary">{a.summary}</span>
      <span className="list-row-tags">
        {a.tags.slice(0, 3).map(t => <TagPill key={t} tag={t} accent={t === a.accentTag}/>)}
      </span>
      <span className="list-row-words">{a.words.toLocaleString()}w</span>
      <span className="list-row-age">{a.age}</span>
    </button>
  );
}

window.Card = Card;
window.ListRow = ListRow;
window.TagPill = TagPill;
window.PREVIEWS = PREVIEWS;
