// FUTERMFIT — the Fit / Release controls of the desktop panel bar. Both are
// explicit clicks: opening the panel, resizing the window and focusing it
// never reach `take-size` (the panel's own resize observer refits the FONT
// only). Outcomes go to the panel's status line through `say`.

import { useMemo, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useConfirm } from "../components/ConfirmProvider";
import { fitWindowDims, type WindowMeasure } from "./fitWindow";
import { ownerClass } from "./gridOwner";
import { releaseGrid, takeGrid, type GridCtx } from "./takeGrid";
import type { Drive } from "./phone/driveApi";

export interface GridControlsProps {
  termId: string;
  harness: string;
  /** The viewer id the tap's hello named for this socket (null from an older tap). */
  viewer: string | null;
  /** The row's `grid_owner` (null/absent: free). */
  owner: string | null;
  /** The host's box at the loaded font; null while it cannot be measured. */
  measureWindow: () => WindowMeasure | null;
  say: (message: string) => void;
  onTook?: () => void;
}

export function GridControls(props: GridControlsProps) {
  const { termId, harness, viewer, owner, measureWindow, say, onTook } = props;
  const confirm = useConfirm();
  const queryClient = useQueryClient();
  const [busy, setBusy] = useState(false);
  const ctx = useMemo<GridCtx>(
    () => ({
      drive: { termId, harness, viewer, confirm } as Drive,
      owner,
      say,
      refresh: () => void queryClient.invalidateQueries({ queryKey: ["terms"] }),
    }),
    [termId, harness, viewer, confirm, owner, say, queryClient],
  );

  const fit = async (): Promise<void> => {
    const dims = fitWindowDims(measureWindow());
    if (dims === null) {
      say("Cannot measure the window yet.");
      return;
    }
    setBusy(true);
    try {
      const took = await takeGrid(ctx, dims, { title: "Fit the terminal to this window?", confirmLabel: "Fit to window" });
      if (took) onTook?.();
    } finally {
      setBusy(false);
    }
  };

  const release = async (): Promise<void> => {
    setBusy(true);
    try {
      await releaseGrid(ctx);
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <button type="button" className="kb-term__btn" data-testid="kb-term-fit" disabled={busy} onClick={() => void fit()}>
        Fit to window
      </button>
      {ownerClass(owner) === "web" ? (
        <button
          type="button"
          className="kb-term__btn"
          data-testid="kb-term-release"
          disabled={busy}
          onClick={() => void release()}
        >
          Release grid
        </button>
      ) : null}
    </>
  );
}
