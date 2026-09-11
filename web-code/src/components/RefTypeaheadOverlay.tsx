import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { fetchRefsTypeahead } from "../api/client";
import RefTypeahead from "./RefTypeahead";

export interface RefTypeaheadOverlayProps {
  repo: string;
  open: boolean;
  currentRef?: string;
  onClose: () => void;
  /// `undefined` clears to the working tree.
  onPick: (insert: string | undefined) => void;
}

/// V76-R3c — `Space @` / TopBar chip door onto `GET /api/refs/typeahead`.
export default function RefTypeaheadOverlay({
  repo,
  open,
  currentRef,
  onClose,
  onPick,
}: RefTypeaheadOverlayProps) {
  const [q, setQ] = useState(currentRef ?? "");
  useEffect(() => {
    if (open) setQ(currentRef ?? "");
  }, [open, currentRef]);

  const { data } = useQuery({
    queryKey: ["refs-typeahead", repo, q],
    queryFn: () => fetchRefsTypeahead(repo, q),
    enabled: open && !!repo,
  });

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;

  const items = (data?.hits ?? []).map((h) => h.insert);

  return (
    <div className="kbc-ref-overlay" role="dialog" aria-label="pick a ref" data-kbc-ref-overlay>
      <div className="kbc-ref-overlay__card">
        <RefTypeahead
          value={q}
          onChange={setQ}
          onSelect={(name) => {
            onPick(name);
            onClose();
          }}
          items={items}
          placeholder="branch, tag, HEAD~n, sha…"
          aria-label="ref typeahead"
        />
        <button
          type="button"
          className="kbc-ref-overlay__wt"
          data-kbc-ref-overlay-wt
          onClick={() => {
            onPick(undefined);
            onClose();
          }}
        >
          working tree
        </button>
        {data?.truncated && (
          <p className="kbc-ref-overlay__cap">
            showing {data.returned} of {data.total}
          </p>
        )}
      </div>
    </div>
  );
}
