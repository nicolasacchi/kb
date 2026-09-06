import { useEffect, useMemo, useRef, useState } from "react";
import {
  fetchKbs,
  setMemoryLinks,
  type KbSummary,
  type RecallHit,
} from "../api/client";
import { Icon } from "./icons";

/// L9 — chip strip + popover editor for one memory's link set.
///
/// Renders a ★ pill when the memory is `global`, and one chip per
/// linked kb. Clicking the cell opens a popover with:
///  - a "Global" toggle (★)
///  - one checkbox per non-memory kb in the daemon
///  - Save / Cancel buttons
///
/// `onChanged` is fired after a successful PUT so the parent can clear
/// any optimistic state; the SSE subscription on `useMemories` will
/// also refresh the list independently.
export default function MemoryLinkChips({
  hit,
  onChanged,
}: {
  hit: RecallHit;
  onChanged?: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [draftGlobal, setDraftGlobal] = useState(!!hit.global);
  const [draftLinks, setDraftLinks] = useState<string[]>(hit.linked_kbs ?? []);
  const [kbs, setKbs] = useState<KbSummary[] | null>(null);
  const rootRef = useRef<HTMLDivElement | null>(null);

  // Reset draft to the hit's actual state whenever the popover opens
  // (a stale draft after an external SSE refresh would clobber).
  useEffect(() => {
    if (!open) return;
    setDraftGlobal(!!hit.global);
    setDraftLinks(hit.linked_kbs ?? []);
    setErr(null);
  }, [open, hit.global, hit.linked_kbs]);

  // Lazy-fetch the kb list the first time the popover opens. Memoised
  // — the daemon's kb set is stable within a session, so one fetch per
  // mount of this component is fine.
  useEffect(() => {
    if (!open || kbs !== null) return;
    let cancelled = false;
    fetchKbs()
      .then((arr) => {
        if (!cancelled) setKbs(arr);
      })
      .catch((e) => {
        if (!cancelled) setErr(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [open, kbs]);

  // Click-outside dismiss. Capture-phase so a click on an outside
  // button still closes us before its onClick fires.
  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      const el = rootRef.current;
      if (el && !el.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onDoc, true);
    return () => document.removeEventListener("mousedown", onDoc, true);
  }, [open]);

  // Non-memory kbs only — linking to another memory corpus doesn't
  // make sense (and the recall fan-out already covers memory corpora).
  const targetKbs = useMemo(
    () => (kbs ?? []).filter((k) => !k.memory_scope),
    [kbs],
  );

  const toggle = (name: string) => {
    setDraftLinks((prev) =>
      prev.includes(name) ? prev.filter((x) => x !== name) : [...prev, name],
    );
  };

  const save = async () => {
    setBusy(true);
    setErr(null);
    try {
      await setMemoryLinks(hit.kb, hit.id, {
        global: draftGlobal,
        linked_kbs: draftLinks,
      });
      setOpen(false);
      onChanged?.();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  const chipCount = (hit.linked_kbs?.length ?? 0) + (hit.global ? 1 : 0);

  return (
    <div className="kb-mem__links" ref={rootRef}>
      <button
        type="button"
        className="kb-mem__links-trigger"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        aria-haspopup="dialog"
        title="edit kb links"
      >
        {chipCount === 0 ? (
          <span className="kb-mem__chip kb-mem__chip--empty">none</span>
        ) : (
          <>
            {hit.global && (
              <span
                className="kb-mem__chip kb-mem__chip--global"
                title="visible to every kb"
              >
                <Icon.Star aria-hidden="true" />
              </span>
            )}
            {(hit.linked_kbs ?? []).slice(0, 3).map((k) => (
              <span key={k} className="kb-mem__chip" title={k}>
                {k}
              </span>
            ))}
            {(hit.linked_kbs?.length ?? 0) > 3 && (
              <span className="kb-mem__chip kb-mem__chip--more">
                +{(hit.linked_kbs?.length ?? 0) - 3}
              </span>
            )}
          </>
        )}
      </button>

      {open && (
        <div
          className="kb-mem__links-pop"
          role="dialog"
          aria-label={`edit links for ${hit.title}`}
        >
          <label className="kb-mem__links-row">
            <input
              type="checkbox"
              checked={draftGlobal}
              onChange={(e) => setDraftGlobal(e.target.checked)}
            />
            <span className="kb-mem__chip kb-mem__chip--global">
              <Icon.Star aria-hidden="true" />
            </span>
            <span>Global — recallable from every kb</span>
          </label>
          <div className="kb-mem__links-sep" />
          {kbs === null ? (
            <div className="kb-mem__links-loading">loading kbs…</div>
          ) : targetKbs.length === 0 ? (
            <div className="kb-mem__links-empty">
              No non-memory kbs configured.
            </div>
          ) : (
            <div className="kb-mem__links-list">
              {targetKbs.map((k) => (
                <label key={k.name} className="kb-mem__links-row">
                  <input
                    type="checkbox"
                    checked={draftLinks.includes(k.name)}
                    onChange={() => toggle(k.name)}
                  />
                  <span className="kb-mem__chip">{k.name}</span>
                </label>
              ))}
            </div>
          )}
          {err && (
            <div className="kb-mem__links-err" role="alert">
              {err}
            </div>
          )}
          <div className="kb-mem__links-actions">
            <button
              type="button"
              onClick={() => setOpen(false)}
              disabled={busy}
            >
              cancel
            </button>
            <button
              type="button"
              className="is-primary"
              onClick={save}
              disabled={busy}
            >
              {busy ? "saving…" : "save"}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
