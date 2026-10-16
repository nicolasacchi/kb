import { useCallback, useState } from "react";

// OL R2 — Topics: the artifact's own headings as a jump list, collapsed by
// default behind a "Topics (N)" header. The open state persists in
// localStorage. Data still comes from the `kb:toc` postMessage subscription in
// PreviewInspector (origin-guarded there); this component only renders.

export type TopicHeading = { id: string; text: string; level: number };

export const TOPICS_OPEN_KEY = "kb:inspector-topics-open";

function readOpen(): boolean {
  if (typeof localStorage === "undefined") return false;
  try {
    return localStorage.getItem(TOPICS_OPEN_KEY) === "1";
  } catch {
    return false;
  }
}

export default function TopicsSection({
  items,
  activeId,
  onJump,
}: {
  items: readonly TopicHeading[];
  activeId: string | null;
  onJump: (id: string) => void;
}) {
  const [open, setOpen] = useState(readOpen);
  const toggle = useCallback(() => {
    setOpen((o) => {
      const next = !o;
      try {
        localStorage.setItem(TOPICS_OPEN_KEY, next ? "1" : "0");
      } catch {
        // localStorage denied — state still updates this session.
      }
      return next;
    });
  }, []);
  // No headings (or the runtime hasn't posted `kb:toc` yet): render nothing,
  // never a bare header.
  if (items.length === 0) return null;
  return (
    <>
      <h4 className="kb-pinsp__topics-h">
        <button
          type="button"
          className="kb-pinsp__topics-toggle"
          aria-expanded={open}
          aria-controls="kb-pinsp-topics"
          data-kb-act="topics-toggle"
          onClick={toggle}
        >
          Topics ({items.length})
        </button>
      </h4>
      {open && (
        <div className="kb-pinsp__toc" id="kb-pinsp-topics">
          {items.map((h) => (
            <button
              key={h.id}
              type="button"
              className={`kb-pinsp__toc-row kb-pinsp__toc-row--l${h.level}${
                h.id === activeId ? " is-on" : ""
              }`}
              onClick={() => onJump(h.id)}
              title={h.text}
            >
              {h.text}
            </button>
          ))}
        </div>
      )}
    </>
  );
}
