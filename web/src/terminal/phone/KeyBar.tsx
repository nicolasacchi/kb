// T18 — the key bar: the keys a phone keyboard does not have.
//
// It NEVER takes focus (a focus move pops the soft keyboard and drops the
// composer): every button acts on `pointerdown` and calls `preventDefault()`
// on both `pointerdown` and `mousedown`, and carries `tabIndex={-1}`. Keys are
// sent as NAMES only (`keys.ts`). Ctrl-C and Ctrl-D must be HELD (600 ms) so a stray tap
// cannot interrupt the harness; a held `/` opens the harness's slash menu,
// whose entries are PASTED and never entered.

import { useCallback, useEffect, useState } from "react";
import { toast } from "../../lib/toast";
import { CAPTION } from "./planSend";
import { DriveError, pressEnter, sendKey, sendText, type Drive } from "./driveApi";
import { KEYBAR_KEYS, slashMenuFor, type KeyName } from "./keys";
import { useHoldConfirm } from "./useHoldConfirm";

export const CTRL_C_HOLD_MS = 600;
export const SLASH_HOLD_MS = 450;

export interface KeyBarProps {
  drive: Drive;
  /** The composer's caption line (shared so a refused Enter shows where the operator is looking). */
  onCaption: (text: string | null) => void;
  /** A key or menu entry went to the session (the OSC 52 typing clock). */
  onTyped?: () => void;
}

/** More keys sit past the right edge of the scrolling row (a pixel of slack). */
export function moreToRight(el: { scrollLeft: number; clientWidth: number; scrollWidth: number }): boolean {
  return el.clientWidth > 0 && el.scrollLeft + el.clientWidth < el.scrollWidth - 1;
}

/** Report a failed Drive call: a toast (invariant #32), never a silent catch. */
function failed(e: unknown): void {
  toast.err(e instanceof DriveError ? e.detail : "Could not reach the terminal");
}

/** A button that never moves focus; `act` runs on pointerdown. */
function NoFocusButton(props: {
  label: string;
  aria: string;
  testId: string;
  act: () => void;
  wide?: boolean;
}) {
  return (
    <button
      type="button"
      tabIndex={-1}
      className={`kb-drive__key${props.wide ? " kb-drive__key--wide" : ""}`}
      data-testid={props.testId}
      aria-label={props.aria}
      onPointerDown={(e) => {
        e.preventDefault();
        props.act();
      }}
      onMouseDown={(e) => e.preventDefault()}
    >
      {props.label}
    </button>
  );
}

export function KeyBar({ drive, onCaption, onTyped }: KeyBarProps) {
  const [menuOpen, setMenuOpen] = useState(false);
  const menu = slashMenuFor(drive.harness);
  // The row scrolls (11 keys do not fit a phone): say so with a fade on the
  // side that has more, instead of a key cut in half with no hint.
  const [keysEl, setKeysEl] = useState<HTMLDivElement | null>(null);
  const [more, setMore] = useState(false);
  const measureMore = useCallback((): void => {
    if (keysEl) setMore(moreToRight(keysEl));
  }, [keysEl]);
  useEffect(() => {
    measureMore();
    if (!keysEl || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(measureMore);
    ro.observe(keysEl);
    return () => ro.disconnect();
  }, [keysEl, measureMore]);

  const send = (key: KeyName): void => {
    onCaption(null);
    sendKey(drive, key).then(
      (r) => {
        if (r.kind === "sent") onTyped?.();
      },
      failed,
    );
  };

  const enter = (): void => {
    onCaption(null);
    pressEnter(drive).then(
      () => onTyped?.(),
      (e: unknown) => {
        // A 409 is the runner saying "not now" (blocked, paste-only, busy):
        // an answer, not a failure — shown where the composer shows its own.
        if (e instanceof DriveError && e.status === 409) onCaption(CAPTION.enterRefused(e.detail));
        else failed(e);
      },
    );
  };

  const ctrlC = useHoldConfirm(CTRL_C_HOLD_MS, {
    onHold: () => send("C-c"),
    onTap: () => onCaption(CAPTION.holdCtrlC),
  });

  // Ctrl-D ends a shell or a harness: a hold, like Ctrl-C.
  const ctrlD = useHoldConfirm(CTRL_C_HOLD_MS, {
    onHold: () => send("C-d"),
    onTap: () => onCaption(CAPTION.holdCtrlD),
  });

  const slash = useHoldConfirm(SLASH_HOLD_MS, {
    onHold: () => setMenuOpen(true),
    onTap: () => send("Slash"),
  });

  const pick = (entry: string): void => {
    setMenuOpen(false);
    onCaption(null);
    // An entry is pasted, never entered: a menu or file picker would take an Enter blindly.
    sendText(drive, entry, { enter: false }).then(
      (r) => {
        if (r.kind === "sent") {
          onTyped?.();
          onCaption(CAPTION.pastedOnly);
        }
      },
      failed,
    );
  };

  return (
    <div className="kb-drive__keybar" data-testid="kb-drive-keybar" role="toolbar" aria-label="Terminal keys">
      {menuOpen && menu.length > 0 ? (
        <div className="kb-drive__slashmenu" data-testid="kb-drive-slashmenu" role="menu" aria-label="Commands">
          {menu.map((entry) => (
            <NoFocusButton
              key={entry}
              label={entry}
              aria={`Paste ${entry}`}
              testId={`kb-drive-slash-${entry.slice(1)}`}
              act={() => pick(entry)}
              wide
            />
          ))}
          <NoFocusButton
            label="Close"
            aria="Close commands"
            testId="kb-drive-slash-close"
            act={() => setMenuOpen(false)}
          />
        </div>
      ) : null}
      <div
        className="kb-drive__keys"
        data-testid="kb-drive-keys"
        data-more={more ? "right" : undefined}
        ref={setKeysEl}
        onScroll={measureMore}
      >
        {KEYBAR_KEYS.map((k) => {
          if (k.name === "C-c" || k.name === "C-d") {
            return (
              <button
                key={k.name}
                type="button"
                tabIndex={-1}
                className="kb-drive__key"
                data-testid={`kb-drive-key-${k.name}`}
                aria-label={k.aria}
                {...(k.name === "C-c" ? ctrlC : ctrlD)}
              >
                {k.label}
              </button>
            );
          }
          if (k.name === "Slash" && menu.length > 0) {
            return (
              <button
                key={k.name}
                type="button"
                tabIndex={-1}
                className="kb-drive__key"
                data-testid="kb-drive-key-Slash"
                aria-label={k.aria}
                {...slash}
              >
                {k.label}
              </button>
            );
          }
          return (
            <NoFocusButton
              key={k.name}
              label={k.label}
              aria={k.aria}
              testId={`kb-drive-key-${k.name}`}
              act={k.name === "Enter" ? enter : () => send(k.name)}
            />
          );
        })}
      </div>
    </div>
  );
}
