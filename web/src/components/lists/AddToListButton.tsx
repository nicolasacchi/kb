import { useEffect, useRef, useState } from "react";
import { useQueries, useQueryClient } from "@tanstack/react-query";
import {
  addListEntry,
  fetchList,
  removeListEntry,
  type ListEntry,
  type ListSummary,
} from "../../api/client";
// The wire Anchor (required-nullable tag/snippet) — the create body and
// the entries we compare against both speak this shape; the hand-written
// client Anchor is looser (optional fields) and doesn't satisfy it.
import type { Anchor } from "../../api/generated/Anchor";
import { useLists } from "../../hooks/useLists";
import { Icon } from "../icons";

// RL-track — the "add to reading list" affordance. Two faces:
//   bar   the ContextBar / inspector action (Icon.Tasks + "list"); also
//         listens for the Cmdk-dispatched `kb:add-to-list.open` event.
//   icon  a tiny + button (TocSpy rows) that adds a SECTION entry.
//
// The popover lists this kb's lists with membership checkboxes. The
// membership truth is each list's detail (["list", kb, id]) — fetched
// on first open via useQueries (enabled: open), shared with the detail
// route's cache so nothing is fetched twice.

function targetMatches(
  e: ListEntry,
  artifactId: string,
  anchor?: Anchor,
): boolean {
  if (e.artifact_id !== artifactId) return false;
  if (!anchor) return !e.anchor; // whole-artifact slot
  if (anchor.kind === "section") {
    return e.anchor?.kind === "section" && e.anchor.id === anchor.id;
  }
  // W2.16 — selection membership. A selection anchor has no stable id (no
  // heading/section to key on), so equality is structural over the three
  // fields that round-trip through `anchor_to_json` (invariant #25):
  // css_path + offset + snippet. Without this branch a selection entry
  // always read as "not a member" — the checkbox never lit for an
  // already-saved highlight even though the server-side anchor_json
  // dedupe already prevented storing a duplicate.
  if (anchor.kind === "selection") {
    return (
      e.anchor?.kind === "selection" &&
      e.anchor.css_path === anchor.css_path &&
      e.anchor.offset === anchor.offset &&
      e.anchor.snippet === anchor.snippet
    );
  }
  return false;
}

export default function AddToListButton({
  kb,
  artifactId,
  anchor,
  variant = "bar",
  dataAct,
}: {
  kb: string;
  artifactId: string;
  /// When present the toggle manages a SECTION or SELECTION entry (TocSpy
  /// rows / W2.16's SelectionActions); absent = the whole artifact
  /// (ContextBar / inspector).
  anchor?: Anchor;
  variant?: "bar" | "icon";
  /// Optional stable e2e hook for the icon variant (which has none by
  /// default — TocSpy's per-section "+" doesn't need one); W2.16's
  /// SelectionActions passes "selection-list" so Playwright can drive the
  /// highlight chooser's add-to-list affordance directly. The bar variant
  /// ignores this — its `data-kb-act="add-to-list"` is already stable.
  dataAct?: string;
}) {
  const [open, setOpen] = useState(false);
  const [newTitle, setNewTitle] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const queryClient = useQueryClient();
  const { lists, create } = useLists();
  const kbLists = lists.filter((l) => l.kb === kb && !l.archived);

  // One detail subscription per list — fetched only while the popover
  // is open, but subscribed always so a warm cache lights the face.
  const details = useQueries({
    queries: kbLists.map((l) => ({
      queryKey: ["list", l.kb, l.id] as const,
      queryFn: ({ signal }: { signal: AbortSignal }) =>
        fetchList(l.kb, l.id, signal),
      enabled: open,
    })),
  });
  const memberIn = (i: number): ListEntry | undefined =>
    details[i]?.data?.entries.find((e) =>
      targetMatches(e, artifactId, anchor),
    );
  const knownCount = details.filter((_, i) => memberIn(i)).length;

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (!wrapRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    window.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      window.removeEventListener("keydown", onKey);
    };
  }, [open]);

  // Cmdk bridge — the palette's "add to a reading list…" command
  // dispatches globally; only the detail route's bar-variant listens.
  useEffect(() => {
    if (variant !== "bar") return;
    const onOpen = () => setOpen(true);
    window.addEventListener("kb:add-to-list.open", onOpen);
    return () => window.removeEventListener("kb:add-to-list.open", onOpen);
  }, [variant]);

  const refresh = (l: ListSummary) => {
    void queryClient.invalidateQueries({ queryKey: ["list", l.kb, l.id] });
    void queryClient.invalidateQueries({ queryKey: ["lists"] });
  };

  const toggle = async (l: ListSummary, i: number) => {
    if (busy) return;
    setBusy(l.id);
    try {
      const entry = memberIn(i);
      if (entry) {
        await removeListEntry(l.kb, l.id, entry.id);
      } else {
        await addListEntry(l.kb, l.id, {
          artifact_id: artifactId,
          anchor: anchor ?? null,
        });
      }
      refresh(l);
    } catch (e) {
      console.warn("[lists] toggle membership failed", e);
    } finally {
      setBusy(null);
    }
  };

  const createAndAdd = async () => {
    const t = newTitle.trim();
    if (!t || busy) return;
    setBusy("__new__");
    try {
      const l = await create(kb, t);
      await addListEntry(kb, l.id, {
        artifact_id: artifactId,
        anchor: anchor ?? null,
      });
      refresh(l);
      setNewTitle("");
    } catch (e) {
      console.warn("[lists] create+add failed", e);
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="kb-atl" ref={wrapRef}>
      {variant === "bar" ? (
        <button
          type="button"
          data-kb-act="add-to-list"
          className={`kb-ctxbar__act ${knownCount > 0 ? "kb-atl__btn--on" : ""}`}
          onClick={() => setOpen((v) => !v)}
          title="add to a reading list"
          aria-expanded={open}
        >
          <Icon.Tasks />
          <span className="kb-ctxbar__lbl">
            {" "}
            list{knownCount > 0 ? ` ·${knownCount}` : ""}
          </span>
        </button>
      ) : (
        <button
          type="button"
          className="kb-atl__iconbtn"
          data-kb-act={dataAct}
          onClick={() => setOpen((v) => !v)}
          title={
            anchor?.kind === "selection"
              ? "add this highlight to a reading list"
              : "add this section to a reading list"
          }
          aria-expanded={open}
        >
          +
        </button>
      )}
      {open && (
        <div className="kb-atl__pop" role="dialog" aria-label="reading lists">
          {anchor?.kind === "section" && (
            <div className="kb-atl__scope">§{anchor.id}</div>
          )}
          {anchor?.kind === "selection" && (
            <div className="kb-atl__scope">
              “{anchor.snippet.slice(0, 40)}
              {anchor.snippet.length > 40 ? "…" : ""}”
            </div>
          )}
          {kbLists.length === 0 && (
            <div className="kb-atl__empty">no lists in {kb} yet</div>
          )}
          {kbLists.map((l, i) => {
            const d = details[i];
            const entry = memberIn(i);
            return (
              <label key={l.id} className="kb-atl__row">
                <input
                  type="checkbox"
                  checked={!!entry}
                  disabled={busy === l.id || d?.isPending}
                  onChange={() => void toggle(l, i)}
                />
                <span className="kb-atl__name">{l.title}</span>
                <span className="kb-atl__count">{l.entry_count}</span>
              </label>
            );
          })}
          <div className="kb-atl__new">
            <input
              type="text"
              placeholder="New list…"
              value={newTitle}
              onChange={(e) => setNewTitle(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void createAndAdd();
              }}
              aria-label="new list title"
            />
            <button
              type="button"
              onClick={() => void createAndAdd()}
              disabled={!newTitle.trim() || busy !== null}
            >
              add
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
