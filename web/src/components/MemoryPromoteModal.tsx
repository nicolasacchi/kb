import { useEffect, useRef, useState } from "react";
import { fetchKbs, promoteMemory, type KbSummary } from "../api/client";

// v0.13 D7 — confirm modal for promoting a memory to a non-memory kb.
// The design called for a confirm-before-destructive dialog: this
// surfaces the destination picker (non-memory kbs only) + the plain-
// language description of what promotion does ("becomes a permanent
// HTML file in the workspace") + a primary action that fires the
// daemon route.
export default function MemoryPromoteModal({
  srcKb,
  artifactId,
  title,
  onClose,
  onPromoted,
}: {
  srcKb: string;
  artifactId: string;
  title: string;
  onClose: () => void;
  onPromoted: (dest: { kb: string; id: string; path: string }) => void;
}) {
  const dlgRef = useRef<HTMLDialogElement | null>(null);
  const [kbs, setKbs] = useState<KbSummary[]>([]);
  const [destKb, setDestKb] = useState<string>("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const dlg = dlgRef.current;
    if (dlg && !dlg.open) dlg.showModal();
    fetchKbs()
      .then((all) => {
        // Non-memory kbs only — the daemon also refuses sideways promotion
        // (memory → memory) with a 400, so filtering client-side is just
        // a nicer UX.
        const eligible = all.filter((k) => !k.memory_scope);
        setKbs(eligible);
        if (eligible.length > 0) setDestKb(eligible[0].name);
      })
      .catch((e) => setError(String(e)));
  }, []);

  const submit = async () => {
    if (!destKb) return;
    setBusy(true);
    setError(null);
    try {
      const r = await promoteMemory(srcKb, artifactId, destKb);
      onPromoted({ kb: r.kb, id: r.id, path: r.path });
      onClose();
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
      setBusy(false);
    }
  };

  return (
    <dialog
      ref={dlgRef}
      className="kb-promote"
      onCancel={(e) => {
        e.preventDefault();
        onClose();
      }}
    >
      <header className="kb-promote__head">
        <h2>Promote memory</h2>
      </header>
      <div className="kb-promote__body">
        <p>
          Copy <b>{title}</b> into a regular kb as a permanent HTML
          artifact. The memory-specific frontmatter
          (<code>kb-salience</code>, <code>kb-decay</code>,{" "}
          <code>kb-pinned</code>, <code>kb-supersedes</code>) is
          stripped. The original memory stays put — use{" "}
          <b>forget</b> separately if you want to remove it.
        </p>
        <label className="kb-promote__field">
          <span>Destination kb</span>
          <select
            value={destKb}
            onChange={(e) => setDestKb(e.target.value)}
            disabled={busy || kbs.length === 0}
          >
            {kbs.length === 0 ? (
              <option value="">no eligible kb</option>
            ) : (
              kbs.map((k) => (
                <option key={k.name} value={k.name}>
                  {k.name} ({k.doc_count})
                </option>
              ))
            )}
          </select>
        </label>
        {error && (
          <div className="kb-promote__error" role="alert">
            {error}
          </div>
        )}
      </div>
      <footer className="kb-promote__foot">
        <button
          type="button"
          className="kb-promote__btn"
          onClick={onClose}
          disabled={busy}
        >
          cancel
        </button>
        <button
          type="button"
          className="kb-promote__btn kb-promote__btn--primary"
          onClick={submit}
          disabled={busy || !destKb}
        >
          {busy ? "promoting…" : "↗ promote"}
        </button>
      </footer>
    </dialog>
  );
}
