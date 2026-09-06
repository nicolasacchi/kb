import { useState } from "react";
import { forgetMemory } from "../api/client";

// Net-new memory mutations (not part of CommentsPanel, which is
// iframe-coupled). M7 ships `forget`. MI-W3.2b added the one daemon-owned
// rewrite route memory metadata now has (salience — see
// `routes/memory.tsx`'s `SalienceEdit`, wired inline on the /memory row,
// not here); merge-via-supersede is still a follow-up.
export default function MemoryActions({
  kb,
  id,
  onForgotten,
}: {
  kb: string;
  id: string;
  onForgotten: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  return (
    <div className="memory-actions">
      <button
        type="button"
        className="btn-ghost memory-forget"
        data-testid="memory-forget"
        disabled={busy}
        title="Forget this memory (deletes the artifact)"
        onClick={async () => {
          setBusy(true);
          setErr(null);
          try {
            await forgetMemory(kb, id);
            onForgotten();
          } catch (e) {
            setErr(String(e));
            setBusy(false);
          }
        }}
      >
        {busy ? "Forgetting…" : "Forget"}
      </button>
      {err && <span className="memory-actions__err">{err}</span>}
    </div>
  );
}
