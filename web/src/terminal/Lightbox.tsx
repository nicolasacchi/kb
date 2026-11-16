// T15 - the picture lightbox: a tapped terminal picture, full size.
//
// Esc closes through an explicit keydown handler on the dialog (focus is
// inside it), so it works in every browser. `useCloseRequest` is added for
// Android Back where the CloseWatcher API exists; both paths are
// idempotent. React 18.3 and 19 compatible (common APIs only).

import { useCallback, useEffect, useRef, useState, type KeyboardEvent } from "react";
import { Icon } from "@kb/ui/icons";
import { useCloseRequest } from "@kb/ui/useCloseRequest";
import type { MediaCache } from "./gfx/media";
import type { OpenPicture } from "./gfx/attach";
import "./gfx/lightbox.css";

export interface LightboxProps {
  picture: OpenPicture | null;
  media: MediaCache;
  onClose: () => void;
}

export function Lightbox({ picture, media, onClose }: LightboxProps) {
  const open = picture !== null;
  const sha = picture?.sha ?? null;
  const dialogRef = useRef<HTMLDivElement | null>(null);
  const [url, setUrl] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);

  const close = useCallback(() => {
    onClose();
  }, [onClose]);
  useCloseRequest(open, close);

  // Pin the cache entry while open, then focus the dialog and restore focus on close.
  useEffect(() => {
    if (sha === null) return;
    const before = document.activeElement as HTMLElement | null;
    media.pin(sha);
    dialogRef.current?.focus();
    let live = true;
    setUrl(null);
    setFailed(false);
    const known = media.peek(sha);
    if (known !== undefined && known !== "missing") setUrl(known.url);
    else {
      void media.load(sha).then((r) => {
        if (!live) return;
        if (r === "missing") setFailed(true);
        else setUrl(r.url);
      });
    }
    return () => {
      live = false;
      media.unpin(sha);
      if (before !== null && typeof before.focus === "function") before.focus();
    };
  }, [sha, media]);

  if (picture === null) return null;

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>): void => {
    if (e.key === "Escape") {
      e.stopPropagation();
      close();
    }
  };

  return (
    <div
      ref={dialogRef}
      className="kbgfx-lightbox"
      role="dialog"
      aria-modal="true"
      aria-label="Picture"
      tabIndex={-1}
      data-testid="kb-term-lightbox"
      onKeyDown={onKeyDown}
      onClick={(e) => {
        if (e.target === e.currentTarget) close();
      }}
    >
      {url !== null ? (
        <img className="kbgfx-lightbox__img" src={url} alt="Terminal picture" />
      ) : (
        <span className="kbgfx-lightbox__wait">{failed ? "image unavailable" : "loading"}</span>
      )}
      <button
        type="button"
        className="kbgfx-lightbox__close"
        aria-label="Close picture"
        onClick={close}
      >
        <Icon.X />
      </button>
    </div>
  );
}
