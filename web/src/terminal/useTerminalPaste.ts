// FUTERMPASTE — picture paste and drop on the desktop panel.
//
// A capture-phase listener on the panel host (the xterm textarea is its
// descendant, so the host sees the event first). It acts ONLY on a paste that
// carries a picture file and no `text/plain`; every text paste, xterm's
// bracketed paste and Ctrl+Shift+V stay untouched. A dropped file goes the
// same way. The upload is `upload.ts` (one home); the returned path is pasted
// into the prompt and NEVER followed by Enter. Nothing here touches the grid.

import { useEffect, useRef } from "react";
import { toast } from "../lib/toast";
import { DriveError } from "./phone/driveApi";
import {
  dragHasFiles,
  droppedFiles,
  pastedPictureFiles,
  uploadImage,
  uploadRefusal,
} from "./upload";

export const PASTE_NOTE =
  "Picture path pasted, not sent. Whether the program attaches it depends on the program.";

export interface TerminalPasteOptions {
  /** The panel host element ref. */
  hostRef: { current: HTMLElement | null };
  termId: string;
  /** Listen only while a desktop terminal is mounted on the host. */
  active: boolean;
  /** Put text into the prompt as a paste (never Enter). */
  paste: (text: string) => void;
  /** The in-panel status line. */
  say: (message: string) => void;
}

export function useTerminalPaste(opts: TerminalPasteOptions): void {
  const ref = useRef(opts);
  ref.current = opts;
  const { active, hostRef, termId } = opts;

  useEffect(() => {
    const host = hostRef.current;
    if (!active || !host) return;
    let busy = false;

    const run = async (files: File[]): Promise<void> => {
      if (busy) {
        toast.err("Still uploading the previous picture");
        return;
      }
      const ok: File[] = [];
      for (const f of files) {
        const refusal = uploadRefusal(f);
        if (refusal !== null) toast.err(refusal);
        else ok.push(f);
      }
      if (ok.length === 0) return;
      busy = true;
      try {
        const paths: string[] = [];
        for (const [i, f] of ok.entries()) {
          ref.current.say(`Uploading picture ${i + 1} of ${ok.length}…`);
          paths.push((await uploadImage(termId, f)).path);
        }
        ref.current.paste(paths.join(" "));
        ref.current.say(PASTE_NOTE);
      } catch (e) {
        ref.current.say("");
        toast.err(e instanceof DriveError ? e.detail : "Could not send the picture");
      } finally {
        busy = false;
      }
    };

    const onPaste = (e: Event): void => {
      const files = pastedPictureFiles((e as ClipboardEvent).clipboardData);
      if (files.length === 0) return;
      e.preventDefault();
      e.stopPropagation();
      void run(files);
    };
    const onDragOver = (e: Event): void => {
      if (dragHasFiles((e as DragEvent).dataTransfer)) e.preventDefault();
    };
    const onDrop = (e: Event): void => {
      const files = droppedFiles((e as DragEvent).dataTransfer);
      if (files.length === 0) return;
      e.preventDefault();
      e.stopPropagation();
      void run(files);
    };

    host.addEventListener("paste", onPaste, true);
    host.addEventListener("dragover", onDragOver, true);
    host.addEventListener("drop", onDrop, true);
    return () => {
      host.removeEventListener("paste", onPaste, true);
      host.removeEventListener("dragover", onDragOver, true);
      host.removeEventListener("drop", onDrop, true);
    };
  }, [active, hostRef, termId]);
}
