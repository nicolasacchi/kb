import { useEffect, useState, useMemo } from "react";
import { matchPath, useLocation } from "react-router-dom";
import { useUrl } from "../../hooks/useUrl";
import { useQueryStats } from "./queryStats";
import { useIsMobile } from "../../hooks/useIsMobile";
import SortControl from "../SortControl";
import GroupControl from "../GroupControl";
import ViewOptionsSheet from "./ViewOptionsSheet";
import { Icon } from "../icons";
import { cycleDensity, loadPrefs, type Density } from "../../api/prefs";
import {
  defaultDir,
  type GroupKey,
  type SortDir,
  type SortKey,
} from "../../lib/sort";

// kb2 redesign — Band 2 "context line" (replaces the v0.10 QueryRibbon).
//
//   scope ▸ filters · N matches · in M ms          [per-view controls]
//
// Left: the active scope (kb) + a compact, removable filter summary +
// the match count/timing reported by the route (queryStats context).
// Right: per-view controls — gallery hosts sort + group (lifted out of
// the gallery header so they live in the chrome); atlas shows the
// cluster/layout segments (inert until the atlas track wires them);
// memory shows its salience-sort segment.
//
// The container keeps the `.kb-qribbon` classname so the immersive +
// mobile CSS selectors and the X2 grid-children placeholder contract
// keep matching.
type Variant = "gallery" | "atlas" | "memory" | "context" | null;

export default function ContextLine() {
  const loc = useLocation();
  const { params } = useUrl();

  const variant = useMemo<Variant>(() => {
    if (matchPath({ path: "/a/:kb/*" }, loc.pathname)) return "context";
    if (loc.pathname === "/memory") return "memory";
    if (loc.pathname === "/") {
      const view = params.get("view");
      if (view === "history") return null; // timeline isn't a structured query
      if (view === "atlas") return "atlas";
      return "gallery";
    }
    return null;
  }, [loc.pathname, params]);

  // The .app grid expects 4 children (topbar / auto-ribbon / 1fr body /
  // statusbar). Returning React null drops a child and slides the grid
  // (v0.13 X2 hotfix), so render a zero-height placeholder on no-line
  // routes (Detail / Settings / History / 404).
  if (variant === null || variant === "context") {
    return <div className="kb-qribbon-placeholder" aria-hidden />;
  }
  return <ContextBody variant={variant} />;
}

function ContextBody({
  variant,
}: {
  variant: Exclude<Variant, null | "context">;
}) {
  const { params, set, setMany } = useUrl();
  const stats = useQueryStats();
  const isMobile = useIsMobile();
  // D4 — ≤860px the gallery's Sort/Group/Density row overflows the
  // ribbon; folded into one "view options" bottom sheet. Desktop DOM is
  // unchanged — this state (and the button/sheet mount below) only exists
  // when `isMobile`, mirroring app.tsx's BottomTabBar mount-gate convention
  // (a mobile-only subtree is never rendered on desktop, not CSS-hidden).
  const [viewOptsOpen, setViewOptsOpen] = useState(false);

  const kb = params.get("kb") || "";
  const tags = (params.get("tags") || "").split(",").filter(Boolean);
  const caps = (params.get("caps") || "").split(",").filter(Boolean);
  const folder = params.get("folder") || "";
  const since = params.get("since") || "";
  const indexOnly = params.get("index") === "1";
  const scope = variant === "memory" ? params.get("scope") || "all" : "";

  const sort: SortKey = ((): SortKey => {
    const v = params.get("sort");
    return v === "indexed" || v === "created" || v === "title" || v === "words"
      ? v
      : "recent";
  })();
  const dir: SortDir = ((): SortDir => {
    const v = params.get("dir");
    return v === "asc" || v === "desc" ? v : defaultDir(sort);
  })();
  const group: GroupKey = params.get("group") === "folder" ? "folder" : "none";

  const hasAny =
    tags.length || caps.length || folder || since || indexOnly;

  const removeTag = (t: string) =>
    set("tags", tags.filter((x) => x !== t).join(",") || null);
  const removeCap = (c: string) =>
    set("caps", caps.filter((x) => x !== c).join(",") || null);
  const editTag = (oldT: string) => (newT: string) =>
    set("tags", tags.map((x) => (x === oldT ? newT : x)).join(","));
  const editCap = (oldC: string) => (newC: string) =>
    set("caps", caps.map((x) => (x === oldC ? newC : x)).join(","));

  // Shared by both the desktop-inline SortControl/GroupControl and the
  // mobile "view options" sheet's copies (D4) — one implementation, two
  // mount points, never forked.
  const onSortChange = (s: SortKey, d: SortDir) =>
    setMany({
      sort: s === "recent" ? null : s,
      dir: d === defaultDir(s) ? null : d,
    });
  const onGroupChange = (g: GroupKey) => set("group", g === "none" ? null : g);

  return (
    <div className={`kb-qribbon kb-qribbon--${variant}`} role="search">
      <span className="kb-qribbon__scope">{kb || "all"}</span>
      <span className="kb-qribbon__prompt" aria-hidden>
        ▸
      </span>

      <span className="kb-qribbon__q">
        {!hasAny && variant !== "memory" && (
          <span className="kb-qribbon__hint">
            no filters · click sidebar to refine
          </span>
        )}

        {tags.map((t, i) => (
          <FilterChip
            key={`t${i}`}
            k="tag"
            value={t}
            onRemove={() => removeTag(t)}
            onChange={editTag(t)}
          />
        ))}
        {folder && (
          <FilterChip
            k="folder"
            value={folder}
            onRemove={() => set("folder", null)}
            onChange={(next) => set("folder", next)}
          />
        )}
        {caps.map((c, i) => (
          <FilterChip
            key={`c${i}`}
            k="cap"
            value={c}
            onRemove={() => removeCap(c)}
            onChange={editCap(c)}
          />
        ))}
        {since && (
          <FilterChip
            k="since"
            value={since}
            onRemove={() => set("since", null)}
            onChange={(next) => set("since", next)}
          />
        )}
        {indexOnly && (
          <FilterChip k="index" value="true" onRemove={() => set("index", null)} />
        )}
        {variant === "memory" && (
          <span className="kb-qribbon__pair">
            <span className="kb-qribbon__tok k">scope</span>
            <span className="kb-qribbon__op">:</span>
            <span className="kb-qribbon__tok v">{scope}</span>
          </span>
        )}

        {stats !== undefined && (
          <span
            className="kb-qribbon__cnt"
            title={stats.warnings.length ? stats.warnings.join("\n") : undefined}
          >
            {stats.total.toLocaleString()} match
            {stats.total === 1 ? "" : "es"} · in {stats.ms} ms
            {stats.warnings.length > 0 && (
              <span className="kb-qribbon__warn-dot" aria-label="query warnings">
                {" "}
                ⚠
              </span>
            )}
          </span>
        )}
      </span>

      <span className="kb-qribbon__right">
        {variant === "gallery" && (
          <>
            <SortControl sort={sort} dir={dir} onSort={onSortChange} />
            <GroupControl group={group} onGroup={onGroupChange} />
            <DensityToggle />
            {isMobile && (
              <>
                <button
                  type="button"
                  className="kb-qribbon__viewopts"
                  data-kb-act="view-opts"
                  aria-haspopup="dialog"
                  aria-expanded={viewOptsOpen}
                  onClick={() => setViewOptsOpen(true)}
                  title="Sort, group & density"
                >
                  <Icon.Settings aria-hidden />
                  view
                </button>
                <ViewOptionsSheet
                  open={viewOptsOpen}
                  onClose={() => setViewOptsOpen(false)}
                >
                  <SortControl sort={sort} dir={dir} onSort={onSortChange} />
                  <GroupControl group={group} onGroup={onGroupChange} />
                  <DensityToggle />
                </ViewOptionsSheet>
              </>
            )}
          </>
        )}
        {variant === "atlas" && (
          <>
            <SegmentLabeled label="cluster">
              <b>tag</b>
              <span title="available later">folder</span>
              <span title="available later">cap</span>
            </SegmentLabeled>
            <SegmentLabeled label="layout">
              <b>force</b>
              <span title="available later">radial</span>
              <span title="available later">year</span>
            </SegmentLabeled>
          </>
        )}
        {variant === "memory" && (
          <SegmentLabeled label="sort">
            <b>salience ▾</b>
            <span title="coming soon">recency</span>
          </SegmentLabeled>
        )}
      </span>
    </div>
  );
}

const DENSITY_LABEL: Record<Density, string> = {
  comfy: "comfortable",
  compact: "compact",
  spacious: "spacious",
};

// Inline density cycle (kb2). Writes the same pref Settings → Preferences
// owns (via cycleDensity), so localStorage + the daemon copy stay in sync.
function DensityToggle() {
  const [density, setDensity] = useState<Density>(() => loadPrefs().density);
  return (
    <button
      type="button"
      className="kb-qribbon__density"
      title={`density: ${DENSITY_LABEL[density]} — click to cycle`}
      aria-label={`density: ${DENSITY_LABEL[density]}`}
      onClick={() => setDensity(cycleDensity())}
    >
      ⊟ {DENSITY_LABEL[density]}
    </button>
  );
}

function SegmentLabeled({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <span className="kb-qribbon__seg-wrap">
      <span className="kb-qribbon__seg-lab">{label}</span>
      <span className="kb-qribbon__seg">{children}</span>
    </span>
  );
}

// Compact filter token: `k:value` with a hover ✕ (click-to-remove) and
// click-to-edit on the value (when onChange is given). Ported from the
// old ribbon's TokenChip so the power-user remove/edit affordances carry
// into the context line.
function FilterChip({
  k,
  value,
  onRemove,
  onChange,
}: {
  k: string;
  value: string;
  onRemove: () => void;
  onChange?: (next: string) => void;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(value);
  useEffect(() => {
    if (!editing) setDraft(value);
  }, [value, editing]);

  const editable = !!onChange;
  return (
    <span className="kb-qribbon__pair">
      <span className="kb-qribbon__tok k">{k}</span>
      <span className="kb-qribbon__op">:</span>
      {editing && onChange ? (
        <input
          ref={(el) => {
            if (el && document.activeElement !== el) {
              el.focus();
              el.select();
            }
          }}
          className="kb-qribbon__tok v kb-qribbon__tok-edit"
          value={draft}
          size={Math.max(4, draft.length + 1)}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              const next = draft.trim();
              if (next && next !== value) onChange(next);
              setEditing(false);
            } else if (e.key === "Escape") {
              setDraft(value);
              setEditing(false);
            }
          }}
          onBlur={() => {
            setDraft(value);
            setEditing(false);
          }}
          aria-label={`edit ${value}`}
        />
      ) : (
        <span
          className={`kb-qribbon__tok v kb-qribbon__tok--removable${editable ? " kb-qribbon__tok--editable" : ""}`}
        >
          <span
            className="kb-qribbon__tok-val"
            role={editable ? "button" : undefined}
            tabIndex={editable ? 0 : undefined}
            onClick={(e) => {
              if (editable) {
                e.stopPropagation();
                setEditing(true);
              }
            }}
            onKeyDown={(e) => {
              if (editable && (e.key === "Enter" || e.key === " ")) {
                e.preventDefault();
                setEditing(true);
              }
            }}
          >
            {value}
          </span>
          <button
            type="button"
            className="kb-qribbon__tok-x"
            onClick={(e) => {
              e.stopPropagation();
              onRemove();
            }}
            title="remove filter"
            aria-label={`remove ${value}`}
          >
            <Icon.X />
          </button>
        </span>
      )}
    </span>
  );
}
