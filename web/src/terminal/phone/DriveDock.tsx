// T18 — the phone Drive dock: everything below the Watch grid that gets
// text and keys INTO the session. Choice bar (only while the session is
// blocked), key bar, composer, and the explicit "Fit to phone".
//
// ONE grid per session: watching, the composer and the soft keyboard never
// resize the pty. Only the Fit button calls take-size (the shared helper in
// `../takeGrid.ts`: the web outranks a local attach, so it asks only for an
// owner of any other class), and Release gives the grid back.

import { useMemo, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useConfirm } from "../../components/ConfirmProvider";
import type { TermRow } from "../../api/terms";
import { useLiveSessions } from "../../hooks/useLiveSessions";
import { ChoiceBar } from "./ChoiceBar";
import { Composer } from "./Composer";
import type { Drive } from "./driveApi";
import { ownerClass } from "../gridOwner";
import { releaseGrid, takeGrid, type GridCtx } from "../takeGrid";
import { fitDims, type PhoneMeasure } from "./fitPhone";
import { KeyBar } from "./KeyBar";
import "./drive.css";

export interface DriveDockProps {
  termId: string;
  harness: string;
  /** The viewer id the tap's `hello` frame named for this panel (null from an older tap). */
  viewer: string | null;
  /** The terminal's list row (`bracket_paste_flag`, `grid_owner`, `session_id`), if loaded. */
  row: TermRow | undefined;
  /** Measure the visible Watch area and one cell; null when not measurable yet. */
  measure: () => PhoneMeasure | null;
  /** The operator typed (the client half of the OSC 52 typing clock). */
  onTyped?: () => void;
  /**
   * The owner the runner last named in a take/release answer, when it is newer than
   * the list row (`{owner: null}` = released). Absent: the row's `grid_owner` rules.
   */
  ownerHint?: { owner: string | null } | null;
  /** Report the runner's answer to a take/release up to the panel that owns the hint. */
  onOwner?: (owner: string | null) => void;
}

export function DriveDock(props: DriveDockProps) {
  const { termId, harness, viewer, row, measure, onTyped, ownerHint, onOwner } = props;
  const confirm = useConfirm();
  const queryClient = useQueryClient();
  const { rows: live } = useLiveSessions();
  const [caption, setCaption] = useState<string | null>(null);
  const [fitting, setFitting] = useState(false);

  const drive = useMemo<Drive>(() => ({ termId, harness, viewer, confirm }), [termId, harness, viewer, confirm]);
  const liveRow = live.find((r) => r.term === termId);
  const bound = (row?.session_id ?? null) !== null || liveRow !== undefined;
  const blocked = liveRow?.blocked === true;
  const owner = ownerHint ? ownerHint.owner : (row?.grid_owner ?? null);

  const grid = useMemo<GridCtx>(
    () => ({
      drive,
      owner,
      say: setCaption,
      owned: onOwner,
      refresh: () => void queryClient.invalidateQueries({ queryKey: ["terms"] }),
    }),
    [drive, owner, onOwner, queryClient],
  );

  // The shared take helper owns the ruling (no confirm for a free, web or
  // local-attach owner; one for any other class) and says every outcome.
  const fit = async (): Promise<void> => {
    const dims = fitDims(measure());
    if (dims === null) {
      setCaption("Cannot measure the screen yet.");
      return;
    }
    setFitting(true);
    setCaption(null);
    try {
      if (await takeGrid(grid, dims, { title: "Fit the terminal to this phone?", confirmLabel: "Fit to phone" })) onTyped?.();
    } finally {
      setFitting(false);
    }
  };

  const release = async (): Promise<void> => {
    setFitting(true);
    setCaption(null);
    try {
      await releaseGrid(grid);
    } finally {
      setFitting(false);
    }
  };

  return (
    <div className="kb-drive" data-testid="kb-drive">
      <div className="kb-drive__head">
        <button
          type="button"
          className="kb-drive__key kb-drive__key--wide"
          data-testid="kb-drive-fit"
          disabled={fitting}
          onPointerDown={(e) => e.preventDefault()}
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => void fit()}
        >
          Fit to phone
        </button>
        {ownerClass(owner) === "web" ? (
          <>
            <button
              type="button"
              className="kb-drive__key kb-drive__key--wide"
              data-testid="kb-drive-release"
              disabled={fitting}
              onPointerDown={(e) => e.preventDefault()}
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => void release()}
            >
              Release grid
            </button>
            <span className="kb-drive__chip" data-testid="kb-drive-grid-chip">
              Grid: phone
            </span>
          </>
        ) : null}
      </div>
      <ChoiceBar drive={drive} onTyped={onTyped} />
      <KeyBar drive={drive} onCaption={setCaption} onTyped={onTyped} />
      <Composer
        drive={drive}
        bracketPaste={row?.bracket_paste_flag ?? null}
        blocked={blocked}
        bound={bound}
        caption={caption}
        onCaption={setCaption}
        onTyped={onTyped}
      />
    </div>
  );
}
