import { intentLabel } from "../../lib/annotations";

/// Phase D — one small pill per annotation showing its intent, shared by
/// the reader's `AnnotationsPanel` and the Commit/Compare diff-comment
/// strip (`DiffFileAnnotations`) so the two surfaces read as the same
/// vocabulary. `flag-for-agent` gets a visually DISTINCT (solid, not
/// tinted) treatment in `styles/provenance.css` — it's an instruction to
/// the agent, not a passive label, and should read that way at a glance.
export default function IntentChip({ intent }: { intent: string }) {
  return (
    <span className={`kbc-intent-chip kbc-intent-chip--${intent}`} data-kbc-intent-chip={intent}>
      {intentLabel(intent)}
    </span>
  );
}
