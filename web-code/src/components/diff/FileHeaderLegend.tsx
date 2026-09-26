// V80-R4 — the per-file header's cue legend. The wrapper header
// (`DiffSections.tsx`'s `LazyDiffSection`, and the single-file focus
// header in `ReviewDiffCenter.tsx`) carries status+±/viewed/focus cues
// by ADJACENCY only — a reader has to infer what a bare checkbox or a
// bare "+245 −14" means from position alone. This is the one explanatory
// door: a small `?` button, a one-line popover naming every cue that can
// appear on a file row (this header's own, AND the nested
// `DiffFileHeader`'s findings/diagnostics/impact chips, which render one
// level down once the file expands — from the reader's perspective it is
// ONE composite row split across two lines, so the legend covers both).
//
// Same click-toggle + outside-click/Escape pattern `ReviewDiffToolbar
// .tsx`'s `Cluster` uses — local, ephemeral UI state, never the review's
// own domain state.
import { useEffect, useRef, useState } from "react";

export default function FileHeaderLegend() {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLSpanElement | null>(null);

  useEffect(() => {
    if (!open) return;
    function onDocClick(e: MouseEvent) {
      if (!ref.current?.contains(e.target as Node)) setOpen(false);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setOpen(false);
    }
    document.addEventListener("mousedown", onDocClick);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDocClick);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <span className="kbc-rdiff__file-legend" ref={ref}>
      <button
        type="button"
        className="kbc-rdiff__file-legend-btn"
        aria-haspopup="true"
        aria-expanded={open}
        aria-label="what these cues mean"
        onClick={(e) => {
          e.stopPropagation();
          setOpen((o) => !o);
        }}
        data-kbc-rdiff-file-legend-btn
      >
        ?
      </button>
      {open && (
        <div className="kbc-rdiff__file-legend-pop" role="tooltip" data-kbc-rdiff-file-legend>
          <b>M/A/D/R</b> status · <b>±</b> lines changed · <b>☑</b> mark viewed ·{" "}
          <b>focus</b> single-file view · <b>●N</b> findings (worst severity) ·{" "}
          <b>diagnostics</b> errors/warnings chip · <b>impact</b> callers touched by this
          change
        </div>
      )}
    </span>
  );
}
