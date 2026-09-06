import { useEffect } from "react";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import { Icon } from "../icons";
import type { ListEntry } from "../../api/client";
import { useListDetail } from "../../hooks/useLists";
import { entryTrailHref, nextReadToggle } from "../../lib/listTrail";
import { READ_DOT } from "./EntryRow";

// RLs5 — trail navigation. When an artifact was opened FROM a reading
// list (?list=&entry= ride the URL — refresh-safe, shareable), this bar
// mounts under the ContextBar and turns the list into a guided path:
//
//   ⟨List title⟩ · entry 3/7 · ~31m left · [● mark] ← prev · next → ✕
//
// prev/next move the ENTRY POINTER, not the artifact: the target is the
// neighbouring entry's trail href, so a hop can land on another section
// of the SAME artifact (params-only navigation — the RLs1 [sec] effect
// scrolls the live iframe, no remount) or on a different artifact.
// Tombstoned entries are skipped. No wrap — a reading queue has a
// direction. `n`/`p` keys mirror the buttons while the bar is mounted.

function nextReadable(
  entries: ListEntry[],
  from: number,
  dir: -1 | 1,
): ListEntry | null {
  for (let i = from + dir; i >= 0 && i < entries.length; i += dir) {
    const e = entries[i];
    if (!e.tombstone && e.source_relative) return e;
  }
  return null;
}

export default function QueueBar({
  kb,
  listId,
  entryId,
}: {
  kb: string;
  listId: string;
  entryId: string | null;
}) {
  const navigate = useNavigate();
  const [, setSearchParams] = useSearchParams();
  const { list, entries, loading, setReadOverride } = useListDetail(
    kb,
    listId,
  );

  const idx = entries.findIndex((e) => e.id === entryId);
  const current = idx >= 0 ? entries[idx] : null;
  const prev = idx >= 0 ? nextReadable(entries, idx, -1) : null;
  const next = idx >= 0 ? nextReadable(entries, idx, 1) : null;

  const go = (target: ListEntry | null) => {
    if (!target) return;
    const href = entryTrailHref(kb, listId, target);
    if (href) navigate(href);
  };

  const exitTrail = () => {
    setSearchParams(
      (sp) => {
        const out = new URLSearchParams(sp);
        out.delete("list");
        out.delete("entry");
        out.delete("sec");
        return out;
      },
      { replace: true },
    );
  };

  // n/p — next/prev entry while the bar is mounted (input-guarded, no
  // modifiers; mirrors the detail route's [/] sibling-nav guards).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "n" && e.key !== "p") return;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const t = e.target as HTMLElement | null;
      if (
        t &&
        (t.tagName === "INPUT" ||
          t.tagName === "TEXTAREA" ||
          t.isContentEditable)
      ) {
        return;
      }
      e.preventDefault();
      go(e.key === "n" ? next : prev);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [prev?.id, next?.id, kb, listId]);

  if (loading || !list) return null;

  return (
    <div className="kb-queuebar" data-testid="queue-bar">
      <Link className="kb-queuebar__list" to={`/lists/${encodeURIComponent(kb)}/${encodeURIComponent(listId)}`}>
        ≡ {list.title}
      </Link>
      {current ? (
        <>
          <span className="kb-queuebar__pos">
            entry {idx + 1}/{entries.length}
          </span>
          {list.remaining_minutes > 0 && (
            <span className="kb-queuebar__left">
              ~{list.remaining_minutes}m left
            </span>
          )}
          <button
            type="button"
            className={`kb-queuebar__mark is-${current.read_state} ${current.read_override ? "is-override" : ""}`}
            onClick={() =>
              void setReadOverride(current.id, nextReadToggle(current))
            }
            title={
              current.read_state === "read"
                ? "mark unread"
                : "mark read"
            }
          >
            {READ_DOT[current.read_state] ?? "○"}{" "}
            {current.read_state === "read" ? "read" : "mark read"}
          </button>
        </>
      ) : (
        <span className="kb-queuebar__pos kb-queuebar__gone">
          entry removed from the list
        </span>
      )}
      <span className="kb-queuebar__spacer" />
      <button
        type="button"
        className="kb-queuebar__nav"
        onClick={() => go(prev)}
        disabled={!prev}
        title="previous entry (p)"
      >
        ← prev
      </button>
      <button
        type="button"
        className="kb-queuebar__nav"
        onClick={() => go(next)}
        disabled={!next}
        title="next entry (n)"
        data-testid="queue-next"
      >
        next →
      </button>
      <button
        type="button"
        className="kb-queuebar__exit"
        onClick={exitTrail}
        title="leave the reading trail"
        aria-label="exit trail"
      >
        <Icon.X />
      </button>
    </div>
  );
}
