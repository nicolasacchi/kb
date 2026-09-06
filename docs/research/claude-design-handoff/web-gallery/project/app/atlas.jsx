// Atlas view — pan/zoom spatial canvas of the same artifacts.
// Coordinates are deterministic per id, but clustered by accentTag.

const { useRef: useARef, useEffect: useAE, useState: useAS, useMemo: useAM } = React;

// Cluster centers in canvas space (px from origin)
const ATLAS_CLUSTERS = {
  rust:     { x: -360, y: -240, label: 'rust · core' },
  memory:   { x: -440, y:  140, label: 'memory · ownership' },
  async:    { x:  340, y: -240, label: 'async · runtimes' },
  errors:   { x:  120, y:  280, label: 'errors' },
  systems:  { x:  440, y:  240, label: 'systems · html' },
  types:    { x:  -40, y: -360, label: 'types · traits' },
  perf:     { x: -160, y:  340, label: 'perf' },
  pin:      { x:  520, y: -100, label: 'pin' },
  default:  { x:    0, y:    0, label: 'misc' },
};

// Deterministic hash → angle/radius offset within a cluster
function hashOf(id) {
  let h = 0;
  for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) | 0;
  return Math.abs(h);
}

function placeArtifact(a) {
  const c = ATLAS_CLUSTERS[a.accentTag] || ATLAS_CLUSTERS.default;
  const h = hashOf(a.id);
  const angle = (h % 360) * Math.PI / 180;
  const radius = 60 + (h % 110);
  return { x: c.x + Math.cos(angle) * radius, y: c.y + Math.sin(angle) * radius };
}

// Build edges: connect each artifact to up to 2 others sharing a tag
function buildEdges(arts) {
  const edges = [];
  const seen = new Set();
  arts.forEach(a => {
    let count = 0;
    for (const b of arts) {
      if (b.id === a.id || count >= 2) continue;
      const shared = a.tags.filter(t => b.tags.includes(t));
      if (shared.length >= 2) {
        const key = [a.id, b.id].sort().join('|');
        if (!seen.has(key)) { seen.add(key); edges.push([a.id, b.id]); count++; }
      }
    }
  });
  return edges;
}

function AtlasView({ artifacts, activeTags, onOpenArtifact, kb }) {
  const canvasRef = useARef(null);
  const surfaceRef = useARef(null);
  const edgesRef = useARef(null);
  const gridRef = useARef(null);
  const [zoom, setZoom] = useAS(1);
  const [pan, setPan] = useAS({ x: 0, y: 0 });
  const [hoverId, setHoverId] = useAS(null);

  const placed = useAM(() => artifacts.map(a => ({ a, ...placeArtifact(a) })), [artifacts]);
  const placedById = useAM(() => Object.fromEntries(placed.map(p => [p.a.id, p])), [placed]);
  const edges = useAM(() => buildEdges(artifacts), [artifacts]);
  const clusters = useAM(() => {
    const present = new Set(artifacts.map(a => a.accentTag));
    return Object.entries(ATLAS_CLUSTERS)
      .filter(([k]) => present.has(k) && k !== 'default')
      .map(([k, c]) => ({ tag: k, ...c }));
  }, [artifacts]);

  // Apply transform
  useAE(() => {
    const t = `translate(${pan.x}px, ${pan.y}px) scale(${zoom})`;
    if (surfaceRef.current) surfaceRef.current.style.transform = t;
    if (edgesRef.current) edgesRef.current.style.transform = t;
    if (gridRef.current) {
      gridRef.current.style.transform = `translate(${pan.x % 32}px, ${pan.y % 32}px) scale(${zoom})`;
    }
  }, [pan, zoom]);

  // Pan
  useAE(() => {
    const el = canvasRef.current; if (!el) return;
    let dragging = false, sx = 0, sy = 0, px = 0, py = 0;
    const down = (e) => {
      if (e.target.closest('.atlas-node')) return;
      dragging = true; sx = e.clientX; sy = e.clientY; px = pan.x; py = pan.y;
      el.setPointerCapture(e.pointerId);
      el.style.cursor = 'grabbing';
    };
    const move = (e) => {
      if (!dragging) return;
      setPan({ x: px + (e.clientX - sx), y: py + (e.clientY - sy) });
    };
    const up = () => { dragging = false; el.style.cursor = 'grab'; };
    const wheel = (e) => {
      e.preventDefault();
      const f = e.deltaY > 0 ? 0.92 : 1.08;
      const r = el.getBoundingClientRect();
      const cx = e.clientX - r.left - r.width / 2;
      const cy = e.clientY - r.top - r.height / 2;
      setZoom(z => {
        const nz = Math.max(0.35, Math.min(2.4, z * f));
        setPan(p => ({ x: cx - (cx - p.x) * (nz / z), y: cy - (cy - p.y) * (nz / z) }));
        return nz;
      });
    };
    el.addEventListener('pointerdown', down);
    el.addEventListener('pointermove', move);
    el.addEventListener('pointerup', up);
    el.addEventListener('pointercancel', up);
    el.addEventListener('wheel', wheel, { passive: false });
    return () => {
      el.removeEventListener('pointerdown', down);
      el.removeEventListener('pointermove', move);
      el.removeEventListener('pointerup', up);
      el.removeEventListener('pointercancel', up);
      el.removeEventListener('wheel', wheel);
    };
  }, [pan]);

  const reset = () => { setZoom(1); setPan({ x: 0, y: 0 }); };

  return (
    <div className="atlas-host">
      <div className="atlas-canvas" ref={canvasRef}>
        <div className="atlas-grid" ref={gridRef}/>
        <svg className="atlas-edges" ref={edgesRef} width="3000" height="3000" style={{ left: -1500, top: -1500 }}>
          {edges.map(([a, b], i) => {
            const pa = placedById[a], pb = placedById[b];
            if (!pa || !pb) return null;
            const ax = 1500 + pa.x, ay = 1500 + pa.y;
            const bx = 1500 + pb.x, by = 1500 + pb.y;
            const mx = (ax + bx) / 2 + (by - ay) * 0.12;
            const my = (ay + by) / 2 - (bx - ax) * 0.12;
            const isHi = hoverId === a || hoverId === b;
            return <path key={i} d={`M ${ax} ${ay} Q ${mx} ${my} ${bx} ${by}`}
              stroke={isHi ? 'var(--accent)' : 'var(--atlas-edge)'}
              strokeWidth={isHi ? 1.5 : 0.8}
              opacity={isHi ? 0.9 : 0.5}
              fill="none"/>;
          })}
        </svg>
        <div className="atlas-surface" ref={surfaceRef}>
          {clusters.map(c => (
            <div key={c.tag} className="atlas-cluster-label"
              style={{ left: c.x, top: c.y - 220, '--tag-color': window.TAG_COLORS[c.tag] || 'var(--muted)' }}>
              {c.label}
            </div>
          ))}
          {placed.map(({ a, x, y }) => {
            const dim = activeTags.size > 0 && !a.tags.some(t => activeTags.has(t));
            const tagColor = window.TAG_COLORS[a.accentTag] || 'var(--muted)';
            const big = a.realSrc || a.backlinks >= 14;
            return (
              <div key={a.id}
                className={`atlas-node ${dim ? 'is-dim' : ''} ${big ? 'is-big' : ''} ${hoverId === a.id ? 'is-hover' : ''}`}
                style={{ left: x, top: y, '--tag-color': tagColor }}
                onMouseEnter={() => setHoverId(a.id)}
                onMouseLeave={() => setHoverId(h => h === a.id ? null : h)}
                onClick={() => onOpenArtifact(a.id)}>
                <div className="atlas-node-top">
                  <span className="atlas-node-id">{a.id}</span>
                  <span className="atlas-node-tag">
                    <span className="atlas-node-dot"/>
                    {a.accentTag}
                  </span>
                </div>
                <h3 className="atlas-node-title">{a.title}</h3>
                <p className="atlas-node-summary">{a.summary}</p>
                <div className="atlas-node-feet">
                  <span className="atlas-pill">{a.age}</span>
                  <span className="atlas-pill">{a.words ? a.words.toLocaleString() + 'w' : 'interactive'}</span>
                  {a.indicators.interactive ? <span className="atlas-pill atlas-pill-int">live</span> : null}
                </div>
              </div>
            );
          })}
        </div>
      </div>

      <div className="atlas-readout">
        <span>{Math.round(zoom * 100)}%</span>
        <span className="atlas-readout-sep">·</span>
        <span>{placed.length} nodes</span>
        <span className="atlas-readout-sep">·</span>
        <span>drag · scroll</span>
      </div>
      <div className="atlas-controls">
        <button onClick={() => setZoom(z => Math.min(2.4, z * 1.2))} title="Zoom in">+</button>
        <button onClick={() => setZoom(z => Math.max(0.35, z * 0.83))} title="Zoom out">−</button>
        <button onClick={reset} title="Recenter">⊡</button>
      </div>
    </div>
  );
}

window.AtlasView = AtlasView;
