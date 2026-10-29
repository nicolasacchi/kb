import { useNavigate } from "react-router-dom";
import { excludeArtifact, type DocSummary } from "../api/client";
import { useConfirm } from "./ConfirmProvider";
import { toast } from "../lib/toast";
import { artifactHref } from "../lib/artifactHref";
import { artifactDownloadUrl, triggerDownload } from "../lib/download";

export type CardActions = {
  peek: () => void;
  download: () => void;
  /** Resolves once the confirm prompt is answered (and the request fired). */
  exclude: () => Promise<void>;
};

/// The three per-card actions, shared by the desktop hover buttons and the
/// touch "More actions" sheet so both surfaces run exactly one implementation.
export function useCardActions(kb: string, doc: DocSummary): CardActions {
  const navigate = useNavigate();
  const confirm = useConfirm();
  return {
    peek: () => {
      navigate(artifactHref(kb, doc.source_relative));
    },
    download: () => {
      triggerDownload(artifactDownloadUrl(kb, doc.id));
    },
    exclude: () => {
      // The confirm is the invariant-#32 destructive prompt and the card's
      // disappearance rides SSE (artifact.removed → docsGate).
      return (async () => {
        const ok = await confirm({
          title: "Exclude from index?",
          body: `Exclude “${doc.title || doc.source_relative}” from search and the gallery? The file stays on disk and its comments + reading history survive — re-include it any time from Settings → Excluded.`,
          confirmLabel: "Exclude",
        });
        if (!ok) return;
        excludeArtifact(kb, doc.source_relative)
          .then(() => toast.ok("excluded from index"))
          .catch((err) =>
            toast.err(
              `exclude failed: ${err instanceof Error ? err.message : String(err)}`,
            ),
          );
      })();
    },
  };
}

export function EyeIcon() {
  return (
    <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor"
      strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <path d="M1 12s4-7 11-7 11 7 11 7-4 7-11 7-11-7-11-7z" />
      <circle cx="12" cy="12" r="3" />
    </svg>
  );
}

/// eye-off — the peek eye's counterpart: "hide this from the index"
export function EyeOffIcon() {
  return (
    <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor"
      strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94" />
      <path d="M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19" />
      <path d="M14.12 14.12a3 3 0 1 1-4.24-4.24" />
      <line x1="1" y1="1" x2="23" y2="23" />
    </svg>
  );
}
