import { useState } from "react";
import { ViewBar } from "@kb/ui/ViewBar";
import { useUrl } from "../hooks/useUrl";
import { useIsMobile } from "../hooks/useIsMobile";
import { useQueryStats } from "./chrome/queryStats";
import SortControl from "./SortControl";
import GroupControl from "./GroupControl";
import FilterChips, { type FilterToken } from "./FilterChips";
import { Icon } from "@kb/ui/icons";
import { cycleDensity, loadPrefs } from "../api/prefs";
import { buildGalleryClusters, parseGalleryView } from "../lib/galleryControls";
import { defaultDir, parseSortParams, type GroupKey, type SortDir, type SortKey } from "../lib/sort";

const csv = (v: string | null) => (v || "").split(",").filter(Boolean);

// The Library's one quiet row under the Spine: where you are (scope, match
// count), the labelled View choice, and this view's controls. Active filters
// ride a second line only while there are any (or the query warned).
export default function GalleryViewBar({
  scope,
  chromeless,
  notesOpen = false,
  onToggleNotes,
  shown,
}: {
  scope: string | null;
  chromeless: boolean;
  /** Notes panel state; the toggle is the bar's right-end control. */
  notesOpen?: boolean;
  onToggleNotes?: () => void;
  /** Rows the Library actually lists; the count reads "N of M" when it differs from the query's total. */
  shown?: number;
}) {
  const { params, set, setMany } = useUrl();
  const isMobile = useIsMobile();
  const stats = useQueryStats();
  const [density, setDensity] = useState(() => loadPrefs().density);

  const view = parseGalleryView(params.get("view"));
  const { sort, dir } = parseSortParams(params);
  const group: GroupKey = params.get("group") === "folder" ? "folder" : "none";

  const tags = csv(params.get("tags"));
  const caps = csv(params.get("caps"));
  const since = params.get("since") || "";
  const indexOnly = params.get("index") === "1";

  const tokens: FilterToken[] = [
    ...tags.map((t) => ({
      k: "tag",
      value: t,
      onRemove: () => set("tags", tags.filter((x) => x !== t).join(",") || null),
      onChange: (n: string) => set("tags", tags.map((x) => (x === t ? n : x)).join(",")),
    })),
    // No folder chip: the folder header's breadcrumb already names (and
    // navigates) the folder, so a chip would say it twice.
    ...caps.map((c) => ({
      k: "cap",
      value: c,
      onRemove: () => set("caps", caps.filter((x) => x !== c).join(",") || null),
      onChange: (n: string) => set("caps", caps.map((x) => (x === c ? n : x)).join(",")),
    })),
    ...(since ? [{ k: "since", value: since, onRemove: () => set("since", null), onChange: (n: string) => set("since", n) }] : []),
    ...(indexOnly ? [{ k: "index", value: "true", onRemove: () => set("index", null) }] : []),
  ];

  // Changing the key resets the direction to that key's default, so the
  // dropdown matches a deep link to the same sort.
  const onSort = (s: SortKey, d: SortDir) =>
    setMany({ sort: s === "recent" ? null : s, dir: d === defaultDir(s) ? null : d });
  const onGroup = (g: GroupKey) => set("group", g === "none" ? null : g);

  const clusters = buildGalleryClusters({
    view,
    sort,
    dir,
    group,
    density,
    // `grid` is the default and stays out of the URL, as before.
    onView: (v) => set("view", v === "grid" ? null : v),
    onSort,
    onGroup,
    onDensity: () => setDensity(cycleDensity()),
  });

  const showStats = !chromeless && stats !== undefined;
  const count = showStats
    ? `${
        shown !== undefined && shown !== stats.total
          ? `${shown.toLocaleString()} of ${stats.total.toLocaleString()}`
          : stats.total.toLocaleString()
      } match${stats.total === 1 ? "" : "es"} · ${stats.ms} ms`
    : undefined;
  const warnings = showStats ? stats.warnings : [];

  return (
    <div className="kb-gallery-viewbar" role="search">
      <div className="kb-gallery-viewbar__row">
      <ViewBar
        title={scope || "all corpora"}
        ariaLabel="Library view"
        count={count}
        clusters={clusters}
        // Phones show the View choice and fold the rest behind More.
        maxVisible={isMobile ? 1 : 9}
        renderControl={(c, node) => {
          if (c.id === "sort") return <SortControl sort={sort} dir={dir} onSort={onSort} />;
          if (c.id === "group") return <GroupControl group={group} onGroup={onGroup} />;
          return node;
        }}
      />
      {onToggleNotes && !chromeless && (
        <button
          type="button"
          className={`gallery-notes-toggle ${notesOpen ? "is-on" : ""}`}
          onClick={onToggleNotes}
          aria-pressed={notesOpen}
          title="notes for the current folder / kb"
        >
          <Icon.Note /> Notes
        </button>
      )}
      </div>
      {(tokens.length > 0 || warnings.length > 0) && !chromeless && (
        <div className="kb-gallery-viewbar__filters">
          <FilterChips tokens={tokens} />
          {warnings.length > 0 && (
            <span className="kb-gallery-viewbar__warn" title={warnings.join("\n")} role="status">
              query warnings
            </span>
          )}
        </div>
      )}
    </div>
  );
}
