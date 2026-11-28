// CMDSHEET - the phone Commands sheet: the effective harness catalogue (shipped
// defaults plus synced edits) one tap from the key bar.
//
// A floating, NON-modal panel anchored above the key bar (like the old slash
// menu it replaces): it overlays, so opening or closing it never resizes the
// Watch pane or the grid. The list is the palette's own `CommandList` (variant
// "sheet"), so there is no second send rule: paste is one paste without Enter,
// text is flattened to one line, an entry with args shows its form first, a run
// entry that the runner refuses toasts and keeps the text, and a typed-command
// guard rides `drive.confirm` inside `sendText`.
//
// It never blurs the input proxy: its own buttons act on pointerdown with
// `preventDefault`, rows already prevent mousedown, and nothing here calls
// `blur()`. The only deliberate focus move is the args form (or the search
// field); after a send from inside the sheet focus goes back to the proxy.
// A command pasted over REST does not feed the direct-typing line mirror
// (`directLine`): that is the accepted lossy convention for pasted entries.

import { useRef, type KeyboardEvent } from "react";
import { useCloseRequest } from "@kb/ui/useCloseRequest";
import { CommandList, refocusTermInput as refocusProxy } from "../commands/palette/CommandList";
import type { Drive } from "./driveApi";
import { useLandscapePhone } from "./useLandscapePhone";

export interface CommandSheetProps {
  drive: Drive;
  onClose: () => void;
  /** An entry reached the session (the OSC 52 typing clock). */
  onTyped?: () => void;
}

export function CommandSheet({ drive, onClose, onTyped }: CommandSheetProps) {
  const root = useRef<HTMLDivElement | null>(null);
  // BPHONE: Close, Escape and the platform close request all end here, so the soft keyboard
  // never drops (a no-op without direct typing: there is no proxy element).
  const close = (): void => {
    refocusProxy();
    onClose();
  };
  // Android Back closes the overlay before it leaves the page (a no-op without CloseWatcher).
  useCloseRequest(true, close);
  // A landscape phone has no room for the search field next to three rows: it sits behind a toggle.
  const landscape = useLandscapePhone();

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>): void => {
    if (e.key !== "Escape") return;
    e.preventDefault();
    e.stopPropagation();
    close();
  };

  return (
    // eslint-disable-next-line jsx-a11y/no-noninteractive-element-interactions
    <div
      ref={root}
      className="kb-drive__cmdsheet"
      data-testid="kb-drive-cmdsheet"
      role="dialog"
      aria-modal="false"
      aria-label="Commands"
      onKeyDown={onKeyDown}
    >
      <div className="kb-drive__cmdsheet-head">
        <p className="kb-drive__cmdsheet-title">Commands</p>
        <button
          type="button"
          tabIndex={-1}
          className="kb-drive__key"
          data-testid="kb-drive-cmdsheet-close"
          aria-label="Close commands"
          onPointerDown={(e) => {
            e.preventDefault();
            close();
          }}
          onMouseDown={(e) => e.preventDefault()}
        >
          Close
        </button>
      </div>
      <CommandList
        termId={drive.termId}
        harness={drive.harness}
        viewer={drive.viewer}
        variant="sheet"
        searchToggle={landscape}
        onNavigate={onClose}
        onSent={() => {
          onTyped?.();
          // Focus is only ever inside the sheet after the args form or the search field took it.
          const inside = root.current?.contains(document.activeElement) ?? false;
          if (inside) refocusProxy();
          onClose();
        }}
      />
    </div>
  );
}
