import { useEffect, useState } from "react";
import {
  fetchFacets,
  fetchFolders,
  fetchKbs,
  fetchTags,
  isAbortError,
  type FacetBucket,
  type FolderNode,
  type KbSummary,
  type TagSummary,
} from "../../api/client";
import { useUrl } from "../../hooks/useUrl";
import { DRAFT_TAG } from "../../lib/draft";
import { galleryUrl } from "../../lib/galleryUrl";
import FolderTree from "../FolderTree";

// v0.10 D4 — refined-cartographic Sidebar.
//
// Replaces v0.6 LeftRail. Same data sources (tags / folders /
// capabilities / date), but a tighter monospace look with explicit
// section headers, tag color dots, folder chevrons, and a date segment
// segmented control. The tag color seed + FolderTree component carry
// forward unchanged; only the chrome around them is new.
//
// X1 retires the old LeftRail file once every consumer (gallery,
// memory, stale-anchors) has switched to <Sidebar/>.

const CAP_LIST: { key: string; label: string }[] = [
  { key: "svg", label: "visual / svg" },
  { key: "interactive", label: "interactive" },
  { key: "longread", label: "long-read" },
  { key: "code", label: "code-heavy" },
];

const DATE_LIST: { value: string; label: string }[] = [
  { value: "7d", label: "7d" },
  { value: "30d", label: "30d" },
  { value: "all", label: "all" },
];

// W1.gallery — the antilibrary read-state filter, riding the server's
// `read=` gate (W1.A; grammar golden-pinned in lib/galleryUrl.ts). Only
// these three states surface as chips (a bare "unread" chip wasn't asked
// for in the design — the URL param still accepts it directly).
const READING_LIST: { value: string; label: string }[] = [
  { value: "", label: "all" },
  { value: "never-opened", label: "never opened" },
  { value: "in_progress", label: "in progress" },
  { value: "read", label: "read" },
];

function hueFromSeed(seed: number): number {
  return seed % 360;
}

export default function Sidebar() {
  const { params, set } = useUrl();
  const [kbs, setKbs] = useState<KbSummary[]>([]);
  const [tags, setTags] = useState<TagSummary[]>([]);
  const [folders, setFolders] = useState<FolderNode[]>([]);
  const [categories, setCategories] = useState<FacetBucket[]>([]);

  useEffect(() => {
    const ctl = new AbortController();
    fetchKbs(ctl.signal)
      .then(setKbs)
      .catch((e) => {
        if (!isAbortError(e)) setKbs([]);
      });
    return () => ctl.abort();
  }, []);

  const activeKb = params.get("kb") || kbs[0]?.name || null;
  const activeKbSummary = kbs.find((k) => k.name === activeKb);

  useEffect(() => {
    if (!activeKb) return;
    const ctl = new AbortController();
    fetchTags(activeKb, ctl.signal)
      .then(setTags)
      .catch((e) => {
        if (!isAbortError(e)) setTags([]);
      });
    fetchFolders(activeKb, ctl.signal)
      .then((r) => setFolders(r.folders))
      .catch((e) => {
        if (!isAbortError(e)) setFolders([]);
      });
    fetchFacets(activeKb, ctl.signal)
      .then((r) => setCategories(r.categories))
      .catch((e) => {
        if (!isAbortError(e)) setCategories([]);
      });
    return () => ctl.abort();
  }, [activeKb]);

  const activeTags = new Set(
    (params.get("tags") || "").split(",").filter(Boolean),
  );
  const activeCaps = new Set(
    (params.get("caps") || "").split(",").filter(Boolean),
  );
  const activeSince = params.get("since") || "all";
  const activeFolder = params.get("folder") || null;
  const activeCategory = params.get("category") || null;
  const indexOnly = params.get("index") === "1";
  const activeRead = params.get("read") || "";
  // W2.8 — capture-to-draft (zero-daemon version; lib/draft.ts). The rail
  // already fetches every tag's count via `fetchTags`; the draft chip just
  // reads the one bucket named `draft`, so it costs no extra request.
  const draftTag = tags.find((t) => t.name === DRAFT_TAG);

  const toggleTag = (name: string) => {
    const next = new Set(activeTags);
    if (next.has(name)) next.delete(name);
    else next.add(name);
    set("tags", next.size ? [...next].join(",") : null);
  };

  const toggleCap = (k: string) => {
    const next = new Set(activeCaps);
    if (next.has(k)) next.delete(k);
    else next.add(k);
    set("caps", next.size ? [...next].join(",") : null);
  };

  // v0.22 — category is single-select (a doc has one kb-category): clicking
  // the active value clears the filter, any other value replaces it.
  const toggleCategory = (value: string) => {
    set("category", activeCategory === value ? null : value);
  };

  return (
    <aside className="kb-side" aria-label="filters">
      <div className="kb-side__sect">Tags</div>
      {tags.length === 0 ? (
        <div className="kb-side__empty" role="status">
          no tags yet
        </div>
      ) : (
        tags.map((t) => (
          <div
            key={t.name}
            className={`kb-side__row ${activeTags.has(t.name) ? "on" : ""}`}
            role="button"
            tabIndex={0}
            onClick={() => toggleTag(t.name)}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                toggleTag(t.name);
              }
            }}
            aria-pressed={activeTags.has(t.name)}
          >
            <span
              className="kb-side__dot"
              style={{ background: `hsl(${hueFromSeed(t.color_seed)}, 64%, 64%)` }}
              aria-hidden
            />
            <span className="kb-side__name">{t.name}</span>
            <span className="kb-side__ct">{t.count}</span>
          </div>
        ))
      )}

      <div className="kb-side__sect">Folders</div>
      {/* FolderTree carries forward unchanged — same data model + class
       * names. The new section header above visually wraps it; the
       * tree's existing rules still apply. */}
      <div className="kb-side__folders">
        <FolderTree
          nodes={folders}
          active={activeFolder}
          onSelect={(p) => set("folder", p)}
        />
      </div>

      {categories.length > 0 && (
        <>
          <div className="kb-side__sect">Category</div>
          {categories.map((c) => (
            <div
              key={c.value}
              className={`kb-side__row ${activeCategory === c.value ? "on" : ""}`}
              role="button"
              tabIndex={0}
              onClick={() => toggleCategory(c.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  toggleCategory(c.value);
                }
              }}
              aria-pressed={activeCategory === c.value}
            >
              <span className="kb-side__name">{c.value}</span>
              <span className="kb-side__ct">{c.count}</span>
            </div>
          ))}
        </>
      )}

      <div className="kb-side__sect">Capabilities</div>
      {CAP_LIST.map((c) => (
        <label
          key={c.key}
          className={`kb-side__chk ${activeCaps.has(c.key) ? "on" : ""}`}
        >
          <input
            type="checkbox"
            checked={activeCaps.has(c.key)}
            onChange={() => toggleCap(c.key)}
            aria-label={c.label}
          />
          <i aria-hidden />
          <span>{c.label}</span>
        </label>
      ))}

      <div className="kb-side__sect">Index</div>
      <label className={`kb-side__chk ${indexOnly ? "on" : ""}`}>
        <input
          type="checkbox"
          checked={indexOnly}
          onChange={() => set("index", indexOnly ? null : "1")}
          aria-label="index pages only"
        />
        <i aria-hidden />
        <span>index pages only</span>
      </label>

      <div className="kb-side__sect">Date</div>
      <div className="kb-side__dates">
        {DATE_LIST.map((d) => (
          <button
            key={d.value}
            type="button"
            className={activeSince === d.value ? "on" : ""}
            onClick={() => set("since", d.value === "all" ? null : d.value)}
            aria-pressed={activeSince === d.value}
          >
            {d.label}
          </button>
        ))}
      </div>

      {/* W1.gallery — antilibrary read-state filter. "never opened" is
       * framed as a research reserve (gallery.tsx's antilibrary-note header),
       * never a backlog/guilt trip — see the calm-computing contract. */}
      <div className="kb-side__sect">Reading</div>
      <div className="kb-side__dates">
        {READING_LIST.map((r) => (
          <button
            key={r.value || "all"}
            type="button"
            className={activeRead === r.value ? "on" : ""}
            onClick={() => set("read", r.value || null)}
            aria-pressed={activeRead === r.value}
            title={
              r.value === "never-opened"
                ? "your research reserve — not opened yet"
                : undefined
            }
          >
            {r.label}
          </button>
        ))}
      </div>

      {/* W2.8 — quick jump to the draft view (see lib/draft.ts). Hidden
       * entirely at zero — a staging count, never a standing backlog badge.
       * CaptureSheet's "save as draft" checkbox stamps the tag; the
       * existing tags editor clears it (files the draft). */}
      {draftTag && draftTag.count > 0 && (
        <a
          className="kb-side__draftlink"
          href={galleryUrl(activeKb, { tags: [DRAFT_TAG] })}
          title="artifacts captured as drafts — not yet filed"
        >
          <span>Drafts ({draftTag.count})</span>
        </a>
      )}

      {/* L9 — quick jump to a per-kb memory view. Only shown when the
       * active kb is NOT itself a memory corpus (a memory corpus IS
       * the memory view's data source; linking to itself is
       * meaningless). The /memory page reads `?kb=` and filters via
       * the new `for_kb` recall param. */}
      {activeKbSummary && !activeKbSummary.memory_scope && (
        <>
          <div className="kb-side__sect">Memory</div>
          <a
            className="kb-side__memlink"
            href={`/memory?kb=${encodeURIComponent(activeKbSummary.name)}`}
            title={`memories visible to ${activeKbSummary.name}`}
          >
            <span>memories for {activeKbSummary.name} →</span>
          </a>
        </>
      )}

      <div className="kb-side__foot">
        {activeKbSummary ? (
          <span>{activeKbSummary.doc_count.toLocaleString()} artifacts</span>
        ) : (
          <span>no kbs</span>
        )}
      </div>
    </aside>
  );
}
