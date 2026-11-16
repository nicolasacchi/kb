// T18 — the phone Drive dock: everything below the Watch grid that gets
// text and keys INTO the session. Choice bar (only while the session is
// blocked), key bar, composer, and the explicit "Fit to phone".
//
// ONE grid per session: watching, the composer and the soft keyboard never
// resize the pty. Only the Fit button calls take-size, and it asks first when
// another viewer owns the grid ("omp rebuilds in every viewer").

import { useMemo, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { useConfirm } from "../../components/ConfirmProvider";
import { toast } from "../../lib/toast";
import type { TermRow } from "../../api/terms";
import { useLiveSessions } from "../../hooks/useLiveSessions";
import { ChoiceBar } from "./ChoiceBar";
import { Composer } from "./Composer";
import { DriveError, takeSize, type Drive } from "./driveApi";
import { fitDims, type PhoneMeasure } from "./fitPhone";
import { KeyBar } from "./KeyBar";
import "./drive.css";

/** The relay's grid-owner name for a grid the phone took (`routes/terms.rs`: `"kb-term"`). */
const PHONE_OWNER = "kb-term";

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
}

export function DriveDock(props: DriveDockProps) {
  const { termId, harness, viewer, row, measure, onTyped } = props;
  const confirm = useConfirm();
  const queryClient = useQueryClient();
  const { rows: live } = useLiveSessions();
  const [caption, setCaption] = useState<string | null>(null);
  const [fitting, setFitting] = useState(false);

  const drive = useMemo<Drive>(() => ({ termId, harness, viewer, confirm }), [termId, harness, viewer, confirm]);
  const liveRow = live.find((r) => r.term === termId);
  const bound = (row?.session_id ?? null) !== null || liveRow !== undefined;
  const blocked = liveRow?.blocked === true;
  const owner = row?.grid_owner ?? null;

  const fit = async (): Promise<void> => {
    const dims = fitDims(measure());
    if (dims === null) {
      setCaption("Cannot measure the screen yet.");
      return;
    }
    if (owner !== null && owner !== PHONE_OWNER) {
      const ok = await confirm({
        title: "Fit the terminal to this phone?",
        body: "Resizing rebuilds the screen for every viewer.",
        confirmLabel: "Fit to phone",
      });
      if (!ok) return;
    }
    setFitting(true);
    setCaption(null);
    try {
      const grid = await takeSize(drive, dims.cols, dims.rows);
      setCaption(`Grid set to ${grid.cols}x${grid.rows}.`);
      onTyped?.();
      void queryClient.invalidateQueries({ queryKey: ["terms"] });
    } catch (e) {
      // A 409 `grid-owned` is the runner's own sentence: shown verbatim.
      if (e instanceof DriveError && e.status === 409) setCaption(e.detail);
      else toast.err(e instanceof DriveError ? e.detail : "Could not reach the terminal");
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
        {owner === PHONE_OWNER ? (
          <span className="kb-drive__chip" data-testid="kb-drive-grid-chip">
            Grid: phone
          </span>
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
