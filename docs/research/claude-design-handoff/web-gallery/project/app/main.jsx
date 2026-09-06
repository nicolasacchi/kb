// Main app — routes between gallery and detail, hosts cmd+k and tweaks

const { useState: useS, useEffect: useE, useCallback: useCB } = React;

const TWEAK_DEFAULTS = /*EDITMODE-BEGIN*/{
  "theme": "dark",
  "accent": "#7c8cff",
  "density": "comfortable",
  "cardStyle": "hybrid",
  "cols": 4,
  "showRail": true,
  "pillVariant": "rail",
  "kbName": "work"
}/*EDITMODE-END*/;

const ACCENT_OPTIONS = ['#7c8cff', '#e89a4a', '#5db59a', '#d56b8a', '#9b6bd5'];
const CARD_STYLES = [
  { value: 'hybrid', label: 'hybrid (title + glyphs)' },
  { value: 'collage', label: 'collage' },
  { value: 'screenshot', label: 'screenshot' },
  { value: 'bigtitle', label: 'big title' },
  { value: 'terminal', label: 'terminal' },
  { value: 'abstract', label: 'abstract' },
];
const PILL_VARIANTS = [
  { value: 'capsule', label: 'capsule (BR)' },
  { value: 'dock', label: 'dock (BC)' },
  { value: 'topright', label: 'top right' },
  { value: 'rail', label: 'right rail' },
];
const DENSITIES = [
  { value: 'compact', label: 'compact' },
  { value: 'comfortable', label: 'comfy' },
  { value: 'spacious', label: 'spacious' },
];

function App() {
  const [tweaks, setTweak] = useTweaks(TWEAK_DEFAULTS);
  const [openId, setOpenId] = useS(null);
  const [cmdkOpen, setCmdkOpen] = useS(false);

  // Apply theme + accent to root
  useE(() => {
    const root = document.documentElement;
    root.dataset.theme = tweaks.theme;
    root.style.setProperty('--accent', tweaks.accent);
  }, [tweaks.theme, tweaks.accent]);

  // Cmd+K shortcut
  useE(() => {
    const onKey = (e) => {
      if ((e.metaKey || e.ctrlKey) && e.key === 'k') { e.preventDefault(); setCmdkOpen(true); }
      if (e.key === '/') { if (document.activeElement?.tagName !== 'INPUT') { e.preventDefault(); setCmdkOpen(true); } }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  // Cross-artifact backlinks — iframes can postMessage({type:'open-artifact', id})
  useE(() => {
    const onMsg = (e) => {
      const d = e.data;
      if (d && d.type === 'open-artifact' && typeof d.id === 'string') {
        if (window.ARTIFACTS.some(a => a.id === d.id)) setOpenId(d.id);
      }
    };
    window.addEventListener('message', onMsg);
    return () => window.removeEventListener('message', onMsg);
  }, []);

  const artifact = openId ? window.ARTIFACTS.find(a => a.id === openId) : null;

  return (
    <div className="app" data-screen-label={artifact ? `Detail · ${artifact.title}` : 'Gallery'}>
      {artifact ? (
        <ArtifactDetail
          a={artifact}
          onClose={() => setOpenId(null)}
          onOpen={(id) => setOpenId(id)}
          onCmdK={() => setCmdkOpen(true)}
          onGraph={() => {}}
          pillVariant={tweaks.pillVariant}
        />
      ) : (
        <Gallery
          tweaks={tweaks}
          onOpenArtifact={(id) => setOpenId(id)}
          onOpenCmdK={() => setCmdkOpen(true)}
          onSettings={() => {}}
        />
      )}
      <CmdK open={cmdkOpen} onClose={() => setCmdkOpen(false)} onOpen={(id) => setOpenId(id)}/>
      <StatusPill/>

      <TweaksPanel title="Tweaks">
        <TweakSection label="theme">
          <TweakRadio label="mode" value={tweaks.theme} options={[{value:'dark',label:'dark'},{value:'light',label:'light'}]} onChange={v => setTweak('theme', v)}/>
          <TweakColor label="accent" value={tweaks.accent} options={ACCENT_OPTIONS} onChange={v => setTweak('accent', v)}/>
        </TweakSection>
        <TweakSection label="gallery">
          <TweakSelect label="card style" value={tweaks.cardStyle} options={CARD_STYLES} onChange={v => setTweak('cardStyle', v)}/>
          <TweakRadio label="density" value={tweaks.density} options={DENSITIES} onChange={v => setTweak('density', v)}/>
          <TweakSlider label="columns" value={tweaks.cols} min={2} max={5} onChange={v => setTweak('cols', v)}/>
          <TweakToggle label="left rail" value={tweaks.showRail} onChange={v => setTweak('showRail', v)}/>
        </TweakSection>
        <TweakSection label="detail">
          <TweakSelect label="floating chrome" value={tweaks.pillVariant} options={PILL_VARIANTS} onChange={v => setTweak('pillVariant', v)}/>
        </TweakSection>
        <TweakSection label="actions">
          <TweakButton label="open cmd+k" onClick={() => setCmdkOpen(true)}/>
          {!artifact && <TweakButton label="open sample artifact" onClick={() => setOpenId('a01')}/>}
          {artifact && <TweakButton label="back to gallery" onClick={() => setOpenId(null)}/>}
        </TweakSection>
      </TweaksPanel>
    </div>
  );
}

ReactDOM.createRoot(document.getElementById('root')).render(<App/>);
