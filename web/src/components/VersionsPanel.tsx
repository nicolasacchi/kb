// Track V — the artifact Versions/Diff right-rail panel. Lists the version
// timeline (working tree + git commits and/or index snapshots, per the kb's
// mode) and diffs the selected older version against the working tree.
// Text (rendered prose, markup-agnostic) by default, with a raw-bytes toggle.

import { useEffect, useMemo, useState } from "react";
import { useVersions } from "../hooks/useVersions";
import { fetchDiff, type DiffHunk, type Version } from "../api/versions";
import { fetchArtifactHtml, isAbortError } from "../api/client";
import { bucketHunksBySection, extractHeadings, type HeadingEntry } from "../lib/galley";
import { resolveMemento } from "../lib/memento";

const WORKING = "WORKING";

function fmtTs(ts: number): string {
  if (!ts) return "";
  const d = new Date(ts * 1000);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function sourceGlyph(s: Version["source"]): string {
  return s === "working" ? "✎" : s === "git" ? "●" : "○";
}

function versionDesc(v: Version): string {
  if (v.source === "working") return "working tree (uncommitted)";
  if (v.source === "index") return "indexed snapshot";
  const label = v.label || "(no message)";
  return v.author ? `${label} — ${v.author}` : label;
}

export default function VersionsPanel({
  kb,
  id,
  at = null,
}: {
  kb: string;
  id: string;
  // CT-F6 — an optional Memento coordinate (`?at=<unix>`): read this
  // artifact as it stood at that instant. Resolved LOCALLY over the
  // timeline already in the query cache (lib/memento.ts, lock-step with
  // `kb_core::versions::resolve_memento`) — no extra fetch. Per-artifact
  // only: it selects a version, it does not filter or rank anything.
  at?: number | null;
}) {
  const { versions, mode, loading, error } = useVersions(kb, id, true);
  const [from, setFrom] = useState<string | null>(null);
  // W2.14 — `viewMode` is the UI-level toggle (three positions); `galley` is
  // a display SKIN over text-mode data, never its own wire mode (the recon's
  // honest-join finding: raw mode's line numbers are exact only for `.html`
  // artifacts fetched over loopback, so galley stays text-mode-only
  // regardless of which of `text`/`raw` was last selected).
  const [viewMode, setViewMode] = useState<"text" | "raw" | "galley">("text");
  const wireMode: "text" | "raw" = viewMode === "raw" ? "raw" : "text";
  const [hunks, setHunks] = useState<DiffHunk[]>([]);
  const [diffLoading, setDiffLoading] = useState(false);
  const [diffError, setDiffError] = useState<string | null>(null);
  // Galley's section headings — the served artifact HTML's ordered
  // `h1..h6[id]` list (lib/galley.ts). Independent fetch from the diff
  // itself; only kicked off while `viewMode === "galley"`.
  const [headings, setHeadings] = useState<HeadingEntry[]>([]);
  const [headingsLoading, setHeadingsLoading] = useState(false);
  const [headingsError, setHeadingsError] = useState<string | null>(null);

  // CT-F6 — the Memento resolution for `?at=`, or null when the reader
  // wasn't opened at an instant. `versions` is the façade's own
  // newest-first list, which is exactly `resolveMemento`'s precondition.
  const memento = useMemo(
    () => (at === null ? null : resolveMemento(versions, at)),
    [versions, at],
  );

  // Reset selection whenever the artifact — or the `?at=` coordinate —
  // changes. `at` is in the deps because a new instant is a new question:
  // the default selection below must re-derive rather than keep pointing at
  // the version the previous coordinate picked.
  useEffect(() => {
    setFrom(null);
    setHunks([]);
    setDiffError(null);
  }, [kb, id, at]);

  // Default `from` = the most recent version older than the working tree, so
  // opening the panel shows "what changed last". CT-F6: with a resolved
  // `?at=`, that instant's version is the selection instead — the panel then
  // reads "everything that changed SINCE the artifact stood that way".
  // A memento that resolved to the WORKING tree (an instant at or after the
  // file's mtime) falls through to the default: the working tree is the
  // diff's `to` side and can never also be its `from`.
  useEffect(() => {
    if (from !== null) return;
    const resolved = memento?.version ?? null;
    const target =
      resolved && resolved.source !== "working"
        ? resolved
        : versions.find((v) => v.source !== "working");
    if (target) setFrom(target.ref);
  }, [versions, from, memento]);

  // (Re)fetch the diff whenever the selected version or wire mode changes.
  useEffect(() => {
    if (!from) {
      setHunks([]);
      return;
    }
    const ctl = new AbortController();
    setDiffLoading(true);
    setDiffError(null);
    fetchDiff(kb, id, from, WORKING, wireMode, ctl.signal)
      .then((r) => {
        if (ctl.signal.aborted) return;
        setHunks(r.hunks);
        setDiffLoading(false);
      })
      .catch((e) => {
        if (isAbortError(e) || ctl.signal.aborted) return;
        setDiffError(e instanceof Error ? e.message : String(e));
        setDiffLoading(false);
      });
    return () => ctl.abort();
  }, [kb, id, from, wireMode]);

  // Galley's heading list — fetched only while the skin is active. No
  // AbortController here: `fetchArtifactHtml` doesn't take a signal (the
  // wider client doesn't support aborting this route today), so a stale
  // response is dropped via the `cancelled` flag instead.
  useEffect(() => {
    if (viewMode !== "galley") return;
    let cancelled = false;
    setHeadingsLoading(true);
    setHeadingsError(null);
    fetchArtifactHtml(kb, id)
      .then((html) => {
        if (cancelled) return;
        setHeadings(extractHeadings(html));
        setHeadingsLoading(false);
      })
      .catch((e) => {
        if (cancelled) return;
        setHeadingsError(e instanceof Error ? e.message : String(e));
        setHeadingsLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [viewMode, kb, id]);

  const gallerySections = useMemo(
    () => (viewMode === "galley" ? bucketHunksBySection(hunks, headings) : []),
    [viewMode, hunks, headings],
  );

  const fromShort = versions.find((v) => v.ref === from)?.short ?? from;

  return (
    <aside className="versions-panel" aria-label="versions">
      <header className="vp__head">
        <span className="vp__title">Versions</span>
        {mode && <span className="vp__mode">{mode}</span>}
        <div className="vp__toggle" role="group" aria-label="diff mode">
          <button
            type="button"
            className={viewMode === "text" ? "is-on" : ""}
            onClick={() => setViewMode("text")}
            aria-pressed={viewMode === "text"}
          >
            text
          </button>
          <button
            type="button"
            className={viewMode === "raw" ? "is-on" : ""}
            onClick={() => setViewMode("raw")}
            aria-pressed={viewMode === "raw"}
          >
            raw
          </button>
          <button
            type="button"
            data-kb-act="galley-toggle"
            className={viewMode === "galley" ? "is-on" : ""}
            onClick={() => setViewMode("galley")}
            aria-pressed={viewMode === "galley"}
            title="galley proof — the same text diff, grouped by section"
          >
            galley
          </button>
        </div>
        {/* v0.22 — the redundant ✕ is gone: the rail's versions icon toggles
            the panel (click again to close). */}
      </header>

      {/* CT-F6 — the Memento banner. Renders only for a `?at=` reader and
          only once the timeline has loaded (resolving against an empty list
          would read as "nothing is that old" while it's merely pending).
          It states the relation explicitly — "nearest prior", never a bare
          version name — so an approximate answer can't be read as exact. */}
      {memento && !loading && !error && (
        <p
          className={`vp__memento${memento.version ? "" : " vp__memento--miss"}`}
          data-testid="versions-memento"
        >
          <span className="vp__memento-lab">as it stood</span>{" "}
          <time dateTime={new Date(memento.atUnix * 1000).toISOString()}>
            {fmtTs(memento.atUnix)}
          </time>
          {memento.version ? (
            <>
              {" → "}
              <span className="vp__memento-hit">
                {sourceGlyph(memento.version.source)} {memento.version.short}
              </span>{" "}
              <span className="vp__memento-rel">
                ({memento.exact ? "exact match" : "nearest prior version"},{" "}
                {fmtTs(memento.version.ts_unix)})
              </span>
            </>
          ) : (
            <>
              {" — "}
              <span className="vp__memento-rel">
                {memento.oldestTsUnix === null
                  ? "no recorded versions to resolve against"
                  : `no version is that old; the oldest is ${fmtTs(memento.oldestTsUnix)}`}
              </span>
            </>
          )}
        </p>
      )}

      <ol className="vp__timeline">
        {loading && <li className="vp__hint">loading…</li>}
        {error && <li className="vp__err">{error}</li>}
        {!loading && !error && versions.length === 0 && (
          <li className="vp__hint">no version history</li>
        )}
        {versions.map((v) => {
          const working = v.source === "working";
          const selected = v.ref === from;
          // CT-F6 — mark the row the `?at=` coordinate landed on, so the
          // answer is visible in the timeline rather than only asserted in
          // the banner above it. Narrowed to a local (rather than a `!`
          // re-assertion below) so the title text can't outlive the match.
          const mementoHit = memento?.version?.ref === v.ref ? memento : null;
          const isMemento = mementoHit !== null;
          return (
            <li
              key={v.ref}
              className={`vp__ver vp__ver--${v.source} ${selected ? "is-sel" : ""}${isMemento ? " is-memento" : ""}`}
            >
              <button
                type="button"
                className="vp__verbtn"
                disabled={working}
                onClick={() => setFrom(v.ref)}
                title={
                  mementoHit
                    ? `the version that stood at ${fmtTs(mementoHit.atUnix)}${mementoHit.exact ? "" : " (nearest prior)"}`
                    : working
                      ? "working tree (current)"
                      : `diff ${v.short} → working tree`
                }
              >
                <span className="vp__glyph">{sourceGlyph(v.source)}</span>
                <span className="vp__short">{v.short}</span>
                <span className="vp__when">{fmtTs(v.ts_unix)}</span>
                <span className="vp__lbl">{versionDesc(v)}</span>
              </button>
            </li>
          );
        })}
      </ol>

      <div className="vp__diff">
        {from && (
          <div className="vp__diffhead">
            {fromShort} → working tree
          </div>
        )}
        {diffLoading && <div className="vp__hint">diffing…</div>}
        {diffError && <div className="vp__err">{diffError}</div>}
        {viewMode === "galley" ? (
          <>
            {headingsLoading && (
              <div className="vp__hint">loading section headings…</div>
            )}
            {headingsError && <div className="vp__err">{headingsError}</div>}
            {!diffLoading &&
              !diffError &&
              !headingsLoading &&
              from &&
              gallerySections.length === 0 && (
                <div className="vp__hint">no changes</div>
              )}
            {!headingsLoading &&
              gallerySections.map((sec, si) => (
                <section
                  className="vp__galley-sec"
                  key={sec.id ?? `unplaced-${si}`}
                >
                  <header className="vp__galley-head">
                    <span className="vp__galley-rule" aria-hidden="true" />
                    <h4 className="vp__galley-title">
                      {sec.title ?? "Unplaced changes"}
                    </h4>
                    <span className="vp__galley-rule" aria-hidden="true" />
                  </header>
                  {sec.hunks.map((h, hi) => (
                    <div
                      className="vp__galley-hunk"
                      key={`${h.old_start}-${h.new_start}-${hi}`}
                    >
                      {h.lines
                        .filter((ln) => ln.tag !== "equal")
                        .map((ln, li) => (
                          <div
                            className={`vp__proof vp__proof--${ln.tag}`}
                            key={li}
                          >
                            <span className="vp__proof-mark" aria-hidden="true">
                              {ln.tag === "insert" ? "‸" : "—"}
                            </span>
                            <span className="vp__proof-text">{ln.text}</span>
                          </div>
                        ))}
                    </div>
                  ))}
                </section>
              ))}
          </>
        ) : (
          <>
        {!diffLoading && !diffError && from && hunks.length === 0 && (
          <div className="vp__hint">no changes</div>
        )}
        {hunks.map((h, hi) => (
          <div className="vp__hunk" key={`${h.old_start}-${h.new_start}-${hi}`}>
            <div className="vp__hunkhead">
              @@ -{h.old_start} +{h.new_start} @@
            </div>
            {h.lines.map((ln, li) => (
              <div className={`vp__line vp__line--${ln.tag}`} key={li}>
                <span className="vp__ln">{ln.old_lineno ?? ""}</span>
                <span className="vp__ln">{ln.new_lineno ?? ""}</span>
                <span className="vp__sign">
                  {ln.tag === "insert" ? "+" : ln.tag === "delete" ? "-" : " "}
                </span>
                <span className="vp__text">{ln.text}</span>
              </div>
            ))}
          </div>
        ))}
          </>
        )}
      </div>
    </aside>
  );
}
