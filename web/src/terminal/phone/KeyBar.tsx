// T18 — the key bar: the keys a phone keyboard does not have.
//
// It NEVER takes focus (a focus move pops the soft keyboard and drops the
// composer): every button acts on `pointerdown` and calls `preventDefault()`
// on both `pointerdown` and `mousedown`, and carries `tabIndex={-1}`. Keys are
// sent as NAMES only (`keys.ts`). Ctrl-C and Ctrl-D must be HELD (600 ms) so a stray tap
// cannot interrupt the harness. CMDSHEET: the Cmds key and a held `/` both open the
// command sheet (the effective catalogue, `CommandSheet`), whose entries ride the
// palette's own send paths; a tapped `/` still sends the literal key.

import { useCallback, useEffect, useState, useSyncExternalStore } from "react";
import { toast } from "../../lib/toast";
import { CAPTION } from "./planSend";
import { DriveError, pressEnter, sendKey, type Drive } from "./driveApi";
import { CommandSheet } from "./CommandSheet";
import { KEYBAR_KEYS, ctrlArrowBytes, keyBytes, type KeyName } from "./keys";
import { useDirectInput } from "./directInput";
import { needsConfirm } from "./guards";
import type { StickyMods } from "./stickyMods";
import { useHoldConfirm } from "./useHoldConfirm";

export const CTRL_C_HOLD_MS = 600;
export const SLASH_HOLD_MS = 450;

export interface KeyBarProps {
  drive: Drive;
  /** The composer's caption line (shared so a refused Enter shows where the operator is looking). */
  onCaption: (text: string | null) => void;
  /** A key or menu entry went to the session (the OSC 52 typing clock). */
  onTyped?: () => void;
  /** POLISH2 — receives the span the landscape quick replies render into (end of the key row). */
  repliesRef?: (el: HTMLElement | null) => void;
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

/**
 * PHIN2 - the sticky Ctrl button (direct typing only): one tap arms it for the next key from the
 * bar or the soft keyboard, a double tap locks it, another tap releases. Like every bar button it
 * acts on pointerdown and never takes focus.
 */
function StickyCtrl({ mods }: { mods: StickyMods }) {
  const state = useSyncExternalStore(mods.subscribe, mods.getState, mods.getState);
  return (
    <button
      type="button"
      tabIndex={-1}
      className="kb-drive__key kb-drive__key--mod"
      data-testid="kb-drive-key-ctrl"
      data-mod={state === "off" ? undefined : state}
      aria-pressed={state !== "off"}
      aria-label={state === "locked" ? "Control, locked" : "Control, applies to the next key"}
      onPointerDown={(e) => {
        e.preventDefault();
        mods.tap();
      }}
      onMouseDown={(e) => e.preventDefault()}
    >
      {state === "locked" ? "Ctrl lock" : "Ctrl"}
    </button>
  );
}

export function KeyBar({ drive, onCaption, onTyped, repliesRef }: KeyBarProps) {
  // PHIN2: non-null = direct typing is on, so keys ride the proxy's WS input path (REST otherwise).
  const direct = useDirectInput();
  const [menuOpen, setMenuOpen] = useState(false);
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
    // The reply host grows when the chips land in it (or the list is edited).
    const host = keysEl.querySelector(".kb-drive__replies-host");
    if (host) ro.observe(host);
    return () => ro.disconnect();
  }, [keysEl, measureMore]);

  /** Direct typing: the key's raw bytes over the shared WS path, a latched Ctrl applied. */
  const sendDirect = (d: NonNullable<typeof direct>, key: KeyName): void => {
    onCaption(null);
    // The latch belongs to the key tapped now, whether or not the Left guard then asks.
    const ctrl = d.mods.consume();
    const go = (): void => {
      const bytes = (ctrl ? ctrlArrowBytes(key) : null) ?? keyBytes(key, d.appCursor());
      if (!d.send(bytes)) {
        failed(new Error("not live"));
        return;
      }
      d.line.feed(bytes);
      onTyped?.();
    };
    const copy = needsConfirm(drive.harness, { kind: "key", key });
    if (copy === null) go();
    else void drive.confirm(copy).then((ok) => (ok ? go() : undefined), failed);
  };

  const send = (key: KeyName): void => {
    if (direct) {
      sendDirect(direct, key);
      return;
    }
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
    if (direct) {
      // The same Enter the proxy runs: the shared line mirror decides whether a typed /bg asks first.
      direct.mods.consume();
      direct.line.enter(() => {
        if (!direct.send("\r")) {
          failed(new Error("not live"));
          return false;
        }
        onTyped?.();
        return true;
      });
      return;
    }
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

  return (
    // BPHONE: the sheet is a sibling of the toolbar (a dialog is not a toolbar item); the wrap is
    // the positioning box the sheet floats above.
    <div className="kb-drive__keybar-wrap">
      {menuOpen ? <CommandSheet drive={drive} onClose={() => setMenuOpen(false)} onTyped={onTyped} /> : null}
      <div className="kb-drive__keybar" data-testid="kb-drive-keybar" role="toolbar" aria-label="Terminal keys">
        <div
          className="kb-drive__keys"
          data-testid="kb-drive-keys"
          data-more={more ? "right" : undefined}
          ref={setKeysEl}
          onScroll={measureMore}
        >
          {direct ? <StickyCtrl mods={direct.mods} /> : null}
          <button
            type="button"
            tabIndex={-1}
            className="kb-drive__key kb-drive__key--wide"
            data-testid="kb-drive-key-cmds"
            aria-label="Cmds, open commands"
            aria-haspopup="dialog"
            aria-expanded={menuOpen}
            onPointerDown={(e) => {
              e.preventDefault();
              setMenuOpen((o) => !o);
            }}
            onMouseDown={(e) => e.preventDefault()}
          >
            Cmds
          </button>
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
            if (k.name === "Slash") {
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
          <span className="kb-drive__replies-host" ref={repliesRef} />
        </div>
      </div>
    </div>
  );
}
