// T21 - the Images strip of `/sessions/:sid`.
//
// Two kinds of picture, merged by time and NEVER deduplicated by sha (an
// omp terminal picture is a re-encode, so its sha never equals the
// transcript's blob sha; two entries with one sha are two pictures):
//
//   * terminal pictures: the kitty images the open terminal panel decoded
//     (`TerminalImage`). They exist only on an admitted (terminal tier on)
//     browser and are passed in by the page that owns the panel.
//   * transcript references: `Item::Image` rows of the session view.
//
// kb keeps references, not pixels. A transcript picture shows its bytes only
// when the terminal tier is admitted AND the media route can serve that
// sha (an omp blob the runner finds on the host); every other case is a
// plain caption tile ("image not stored by kb"). An unadmitted browser
// makes NO request to the media route. Bytes are fetched as blob URLs by
// T15's `MediaCache` (header-carrying `fetch`, never a bare `<img src>`),
// held in memory only and revoked on unmount; nothing here enters the
// TanStack cache (#23).

import { useEffect, useMemo, useRef, useState } from "react";
import { useIdentity } from "../hooks/useArtifactHost";
import { Lightbox } from "./Lightbox";
import type { OpenPicture } from "./gfx/attach";
import { MediaCache, type MediaCacheOptions, type MediaResult } from "./gfx/media";
import { useSessionImageRefs, type TranscriptImageRef } from "./sessionImages";
import "./imagesStrip.css";

/** A picture the open terminal panel decoded. `seenAt` is epoch milliseconds. */
export interface TerminalImage {
  sha256: string;
  w: number;
  h: number;
  seenAt: number;
}

export interface Tile {
  key: string;
  source: "terminal" | "transcript";
  sha: string | null;
  mime: string;
  w: number;
  h: number;
  /** Epoch ms, or null when the source gave no usable time. */
  at: number | null;
  turnId: string | null;
}

export const NOT_STORED_CAPTION = "image not stored by kb";

function parseTime(ts: string | null): number | null {
  if (ts === null) return null;
  const n = Date.parse(ts);
  return Number.isNaN(n) ? null : n;
}

/**
 * The strip's tiles in chronological order (stable; tiles without a time
 * sort last, in their given order). Terminal pictures are dropped unless
 * the browser is admitted to the terminal tier. Nothing is deduplicated.
 */
export function buildTiles(
  terminal: readonly TerminalImage[],
  refs: readonly TranscriptImageRef[],
  admitted: boolean,
): Tile[] {
  const tiles: Tile[] = [];
  if (admitted) {
    terminal.forEach((t, i) => {
      tiles.push({
        key: `terminal-${i}`,
        source: "terminal",
        sha: t.sha256,
        mime: "image/png",
        w: t.w,
        h: t.h,
        at: t.seenAt,
        turnId: null,
      });
    });
  }
  refs.forEach((r, i) => {
    tiles.push({
      key: `transcript-${i}`,
      source: "transcript",
      sha: r.sha256,
      mime: r.mime,
      w: 0,
      h: 0,
      at: parseTime(r.ts),
      turnId: r.turnId,
    });
  });
  return tiles
    .map((tile, order) => ({ tile, order }))
    .sort((a, b) => {
      const x = a.tile.at;
      const y = b.tile.at;
      if (x === null && y === null) return a.order - b.order;
      if (x === null) return 1;
      if (y === null) return -1;
      return x - y || a.order - b.order;
    })
    .map((e) => e.tile);
}

function TileView({
  tile,
  media,
  admitted,
  onOpen,
}: {
  tile: Tile;
  media: MediaCache | null;
  admitted: boolean;
  onOpen: (p: OpenPicture) => void;
}) {
  const cellRef = useRef<HTMLDivElement | null>(null);
  // Without IntersectionObserver (old engines, jsdom) every tile counts as visible.
  const [visible, setVisible] = useState(() => typeof IntersectionObserver === "undefined");
  const [res, setRes] = useState<MediaResult | undefined>(undefined);
  const wantsBytes = admitted && tile.sha !== null;

  useEffect(() => {
    if (visible || !wantsBytes) return;
    const el = cellRef.current;
    if (el === null || typeof IntersectionObserver === "undefined") return;
    // One screen of look-ahead, no more: a long transcript never loads everything.
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          setVisible(true);
          io.disconnect();
        }
      },
      { rootMargin: "100% 0px" },
    );
    io.observe(el);
    return () => io.disconnect();
  }, [visible, wantsBytes]);

  const sha = tile.sha;
  useEffect(() => {
    if (!wantsBytes || !visible || media === null || sha === null) return;
    let live = true;
    const known = media.peek(sha);
    if (known !== undefined) setRes(known);
    else {
      void media.load(sha).then((r) => {
        if (live) setRes(r);
      });
    }
    return () => {
      live = false;
    };
  }, [wantsBytes, visible, media, sha]);

  const when = tile.at === null ? null : new Date(tile.at);
  const loaded = res !== undefined && res !== "missing" ? res : null;
  const caption = !wantsBytes || res === "missing";

  return (
    <div
      ref={cellRef}
      className="kb-imgs__tile"
      data-testid="kb-imgs-tile"
      data-source={tile.source}
    >
      {loaded !== null && sha !== null ? (
        <button
          type="button"
          className="kb-imgs__open"
          data-testid="kb-imgs-open"
          aria-label={`Open ${tile.source} picture`}
          onClick={() =>
            onOpen({ id: 0, sha, mime: tile.mime, w: tile.w, h: tile.h })
          }
        >
          <img className="kb-imgs__thumb" src={loaded.url} alt={`${tile.source} picture`} />
        </button>
      ) : caption ? (
        <div className="kb-imgs__missing" data-testid="kb-imgs-caption">
          {NOT_STORED_CAPTION}
        </div>
      ) : (
        <div className="kb-imgs__missing" data-testid="kb-imgs-pending">
          loading
        </div>
      )}
      <div className="kb-imgs__meta">
        <span>{tile.source}</span>
        {when !== null && (
          <time dateTime={when.toISOString()}>
            {when.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })}
          </time>
        )}
        {tile.turnId !== null && (
          <a href={`#${tile.turnId}`} data-testid="kb-imgs-turn">
            turn
          </a>
        )}
      </div>
    </div>
  );
}

export interface ImagesStripViewProps {
  /** `terminal_admitted` from `GET /api/identity`. */
  admitted: boolean;
  refs: readonly TranscriptImageRef[];
  terminalImages?: readonly TerminalImage[];
  /** Test seam: fetch/URL/decode injection for the owned `MediaCache`. */
  mediaOptions?: MediaCacheOptions;
}

export function ImagesStripView({
  admitted,
  refs,
  terminalImages = [],
  mediaOptions,
}: ImagesStripViewProps) {
  const [media, setMedia] = useState<MediaCache | null>(null);
  const [picture, setPicture] = useState<OpenPicture | null>(null);
  const optsRef = useRef(mediaOptions);

  // The cache is created after mount and disposed on unmount, which revokes
  // every blob URL it made (pins included).
  useEffect(() => {
    if (!admitted) return;
    const m = new MediaCache({ compact: true, ...optsRef.current });
    setMedia(m);
    return () => {
      m.dispose();
      setMedia(null);
    };
  }, [admitted]);

  const tiles = useMemo(
    () => buildTiles(terminalImages, refs, admitted),
    [terminalImages, refs, admitted],
  );

  if (tiles.length === 0) {
    return (
      <p className="kb-imgs__empty" data-testid="kb-imgs-empty">
        no images in this session
      </p>
    );
  }
  return (
    <>
      <div className="kb-imgs" data-testid="kb-imgs">
        {tiles.map((t) => (
          <TileView
            key={t.key}
            tile={t}
            media={media}
            admitted={admitted}
            onOpen={setPicture}
          />
        ))}
      </div>
      {media !== null && (
        <Lightbox picture={picture} media={media} onClose={() => setPicture(null)} />
      )}
    </>
  );
}

/**
 * The page-facing strip: its own `<details>` section (A4's markup classes),
 * fetching the view's picture refs only once the section is open.
 */
export default function ImagesStrip({
  sid,
  open,
  terminalImages,
}: {
  sid: string;
  open: boolean;
  terminalImages?: readonly TerminalImage[];
}) {
  const [expanded, setExpanded] = useState(false);
  const active = open || expanded;
  const identity = useIdentity();
  const admitted = identity?.terminal_admitted === true;
  const { refs, loading, error } = useSessionImageRefs(sid, active);
  const count = active && !loading && error === null ? refs.length : null;
  return (
    <details
      id="sec-images"
      data-testid="session-sec-images"
      open={open}
      className="kb-sp__sec"
      onToggle={(e) => setExpanded(e.currentTarget.open)}
    >
      <summary className="kb-sp__sum">{count === null ? "Images" : `Images · ${count}`}</summary>
      <div className="kb-sp__body">
        {!active ? null : loading ? (
          <p className="kb-imgs__empty">loading</p>
        ) : error !== null ? (
          <p className="kb-imgs__empty">unavailable</p>
        ) : (
          <ImagesStripView admitted={admitted} refs={refs} terminalImages={terminalImages} />
        )}
      </div>
    </details>
  );
}
