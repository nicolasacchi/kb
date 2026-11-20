// FUTERMFIT — the ONE take/release helper, shared by the desktop bar and the
// phone dock (invariant #30: one home per action).
//
// The grid ruling: the web outranks a local `kb-term attach`, and between two
// web clients the latest explicit Fit wins. So there is NO confirm when the
// grid is free, owned by the web, or owned by a local attach; a confirm is
// kept only for an owner of any other class. Every outcome is said once, to
// the caller's status line; a failure also raises a toast (#32).

import { toast } from "../lib/toast";
import { DriveError, releaseSize, takeSize, type Drive } from "./phone/driveApi";
import { ownerClass } from "./gridOwner";

export interface GridCtx {
  drive: Drive;
  /** The row's `grid_owner` right now (null: free). */
  owner: string | null;
  /** The status line (an `aria-live` element inside the panel; the phone caption). */
  say(message: string): void;
  /** The row changed: refresh the `["terms"]` list. */
  refresh(): void;
}

export interface ConfirmCopy {
  title: string;
  confirmLabel: string;
}

export const TAKE_CONFIRM_BODY = "Resizing rebuilds the screen for every viewer.";

function fail(ctx: GridCtx, e: unknown): void {
  // A 409 `grid-owned` is the runner's own sentence: shown verbatim, no toast.
  if (e instanceof DriveError && e.status === 409) {
    ctx.say(e.detail);
    return;
  }
  const detail = e instanceof DriveError ? e.detail : "Could not reach the terminal";
  ctx.say(detail);
  toast.err(detail);
}

/** Take the grid at `dims`. True when the runner accepted it. */
export async function takeGrid(
  ctx: GridCtx,
  dims: { cols: number; rows: number },
  copy: ConfirmCopy,
): Promise<boolean> {
  const cls = ownerClass(ctx.owner);
  if (cls === "other") {
    const ok = await ctx.drive.confirm({ title: copy.title, body: TAKE_CONFIRM_BODY, confirmLabel: copy.confirmLabel });
    if (!ok) return false;
  }
  try {
    const grid = await takeSize(ctx.drive, dims.cols, dims.rows);
    // The runner's answer is authoritative (it may clamp); ours is the fallback.
    const size = `${grid.cols ?? dims.cols}x${grid.rows ?? dims.rows}`;
    ctx.say(cls === "local" ? `Took the grid from the local attach; it now watches (${size}).` : `Grid set to ${size}.`);
    ctx.refresh();
    return true;
  } catch (e) {
    fail(ctx, e);
    // A refusal may still mean the owner changed under us.
    ctx.refresh();
    return false;
  }
}

/** Give the grid back. Only meaningful while the web owns it (the caller shows the control then). */
export async function releaseGrid(ctx: GridCtx): Promise<boolean> {
  try {
    await releaseSize(ctx.drive);
    ctx.say("Released the grid.");
    ctx.refresh();
    return true;
  } catch (e) {
    fail(ctx, e);
    ctx.refresh();
    return false;
  }
}
