import type { Cluster } from "@kb/ui/ViewBar";
import type { Density } from "../api/prefs";
import { defaultDir, type GroupKey, type SortDir, type SortKey } from "./sort";

// The Library's `?view=` values, in the order the View control lists them,
// with the label each carries. The URL values are unchanged (`press` is the
// Front page, `grid` the Shelf, `list` the Index, `history` the Timeline).
export const GALLERY_VIEWS = [
  { value: "press", label: "Front page" },
  { value: "grid", label: "Shelf" },
  { value: "list", label: "Index" },
  { value: "atlas", label: "Atlas" },
  { value: "history", label: "Timeline" },
  { value: "canvas", label: "Canvas" },
] as const;

export type GalleryView = (typeof GALLERY_VIEWS)[number]["value"];

/// `?view=` parsed once; anything unknown is the default Shelf.
export function parseGalleryView(raw: string | null): GalleryView {
  return GALLERY_VIEWS.some((v) => v.value === raw) ? (raw as GalleryView) : "grid";
}

/// Sort / Group / Density only mean something on the two row-set views.
export function hasArrange(view: GalleryView): boolean {
  return view === "grid" || view === "list";
}

export const DENSITY_LABEL: Record<Density, string> = {
  comfy: "comfortable",
  compact: "compact",
  spacious: "spacious",
};

export type GalleryControlsArgs = {
  view: GalleryView;
  sort: SortKey;
  dir: SortDir;
  group: GroupKey;
  density: Density;
  onView: (v: GalleryView) => void;
  onSort: (s: SortKey, d: SortDir) => void;
  onGroup: (g: GroupKey) => void;
  onDensity: () => void;
};

export const SORT_OPTIONS: { value: SortKey; label: string }[] = [
  { value: "recent", label: "recent (modified)" },
  { value: "indexed", label: "recent (indexed)" },
  { value: "created", label: "recent (created)" },
  { value: "title", label: "title A→Z" },
  { value: "words", label: "words" },
];

// The Library's controls as ONE ViewBar: a labelled View choice always, then
// Arrange (sort, group) and Density on the Shelf and the Index.
export function buildGalleryClusters(a: GalleryControlsArgs): Cluster[] {
  const clusters: Cluster[] = [
    {
      label: "View",
      controls: [
        {
          id: "view",
          label: "View",
          kind: "select",
          value: a.view,
          options: GALLERY_VIEWS.map((v) => ({ value: v.value, label: v.label })),
          onSelect: (v) => v && a.onView(parseGalleryView(v)),
        },
      ],
    },
  ];
  if (hasArrange(a.view)) {
    clusters.push(
      {
        label: "Arrange",
        controls: [
          {
            id: "sort",
            label: "Sort",
            kind: "select",
            value: a.sort,
            options: SORT_OPTIONS,
            onSelect: (v) => v && a.onSort(v as SortKey, defaultDir(v as SortKey)),
          },
          {
            id: "group",
            label: "Group",
            kind: "segment",
            value: a.group,
            options: [
              { value: "none", label: "none" },
              { value: "folder", label: "folder" },
            ],
            onSelect: (v) => v && a.onGroup(v as GroupKey),
          },
        ],
      },
      {
        label: "Density",
        controls: [
          {
            id: "density",
            label: DENSITY_LABEL[a.density],
            kind: "button",
            attrs: { "aria-label": `density: ${DENSITY_LABEL[a.density]}` },
            onSelect: () => a.onDensity(),
          },
        ],
      },
    );
  }
  return clusters;
}
