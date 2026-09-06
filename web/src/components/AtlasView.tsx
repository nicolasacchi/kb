import { censusBump } from "../lib/census";
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type MouseEvent,
  type PointerEvent,
} from "react";
import { Link, useNavigate } from "react-router-dom";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ApiError,
  fetchAtlasLabels,
  putBoardCanvas,
  recomputeAtlas,
  reclusterAtlas,
  type AtlasEdge,
  type AtlasLabelTerm,
  type AtlasPoint,
  type AtlasPointsResponse,
  type DocSummary,
  type ListSummary,
} from "../api/client";
import MapSearchOverlay from "./MapSearchOverlay";
import { defaultLayoutFor, EMPTY_CANVAS, type CanvasDoc } from "../lib/canvas";
import {
  applyFieldTransform,
  fieldIslands,
  fieldPlacements,
  heatColor,
  heatT,
  putAtlasField,
  recoverFieldTransform,
  setPlacement,
  IDENTITY_FIELD_TRANSFORM,
} from "../api/atlasField";
import {
  useAtlasField,
  useAtlasFieldDisagreement,
} from "../hooks/useAtlasField";
import { stepStop, tourStops, type TourPoint } from "../lib/atlasTour";
import { entryTrailHref } from "../lib/listTrail";
import { currentDaemonBase } from "../api/base";
import {
  fetchSessions,
  fetchSessionTouches,
  type SessionRow,
} from "../api/sessions";
import { sse } from "../api/sse";
import { artifactHref } from "../lib/artifactHref";
import {
  deleteCamera,
  listCameras,
  saveCamera,
  type AtlasCamera,
  type AtlasCameraColorMode,
} from "../lib/atlasCameras";
import {
  MAX_CONTINENTS,
  mergeContinents,
  selectionFromLasso,
  type ClusterForMerge,
} from "../lib/atlasSelection";
import {
  decayBucketFillColor,
  hasMemoryPoints,
  mergeAtlasEdges,
  salienceFillColor,
  supersedeEdges,
} from "../lib/atlasMemory";
import {
  fitTransform,
  screenToLogical as fitScreenToLogical,
} from "../lib/atlasFit";
import {
  centroidsOf,
  remapClusters,
  remappedCluster,
} from "../lib/clusterRemap";
import { useAtlasFrame, useAtlasHistory } from "../hooks/useAtlasHistory";
import { tagColor, tagsFor } from "../lib/derive";
import { galleryUrl } from "../lib/galleryUrl";
import { sessionColorFor } from "../lib/sessionColor";
import { reducedMotionRequested } from "../lib/viewTransition";
import { useListDetail, useLists } from "../hooks/useLists";
import { useReadingProgress } from "../hooks/useReadingProgress";
import AtlasInspector from "./AtlasInspector";
import { useConfirm } from "./ConfirmProvider";
import { Icon } from "./icons";
import { toast } from "../lib/toast";

// v0.14 S7 — cap on how many recent sessions the overlay draws as
// polylines. More than ~10 becomes unreadable spaghetti even at
// modest zoom; the SPA can lift this later via a control if needed.
const ATLAS_SESSION_LIMIT = 10;

// v0.3 Atlas — reads atlas_x/atlas_y/atlas_cluster off the doc summaries.
// Brand-new artifacts that pre-date the most recent recompute fall back
// to the v0.1 hash placement so the route never renders an empty
// canvas — the note + recompute button in the header bar make the
// hybrid origin obvious and self-service.
//
// Layers (back to front): density heatmap → cross-artifact edges →
// cluster labels → dots → hovered dot's title. Edges come from
// /api/kb/{kb}/edges; cluster labels are derived client-side from the
// most-frequent tag in each cluster.
//
// S6 (S-milestone): renderer ported from SVG (one DOM node per dot +
// per edge) to a single <canvas> with a Canvas2D draw loop. At 50k
// dots the SVG version would have 50k+ DOM nodes and pan/zoom
// thrashed the layout engine; canvas paints the whole frame in <5ms
// at 50k. Hover/click hit testing is done in logical coords against
// `points`; click → react-router navigate to the artifact detail.
const W = 600;
const H = 360;
const INSET = 24;
const ZOOM_MIN = 0.5;
const ZOOM_MAX = 8;
// M-spa: pan bounds. Pre-fix the user could drag the atlas entirely
// off-screen with no recenter, leaving an empty canvas. Cap pan to
// the viewport extent — enough to inspect edges/corners, not enough
// to lose the layout.
const PAN_MAX_X = W;
const PAN_MAX_Y = H;

// Safety net for the case where POST succeeded but the SSE never arrives
// (daemon crash, dropped connection past the EventSource retry budget).
// Mirrors the kb-cli `kb atlas recompute` timeout at commands/atlas.rs:40.
const RECOMPUTE_TIMEOUT_MS = 60_000;

// W2.3b — semantic-zoom label LOD thresholds. Below ZOOM_LOD_CONTINENT the
// ≤12 cluster labels collapse into ≤MAX_CONTINENTS continent labels;
// between the two thresholds nothing changes from the pre-W2.3b behaviour;
// above ZOOM_LOD_DETAIL each cluster label expands to more terms + a
// count, since there's screen space to spend it. Named consts (not
// inlined) because the recon calls these out as the semantic-zoom
// contract — a future tuning pass changes one number here, not three
// call sites.
const ZOOM_LOD_CONTINENT = 1;
const ZOOM_LOD_DETAIL = 3;

// W2.3b — the same 500-id envelope cap the docs-list route already
// enforces (`MAX_ENVELOPE_LIMIT`, routes/docs.rs) applies to any
// `&ids=` gallery deep-link this view builds: a lasso or a cluster that
// spans more artifacts than that makes the URL impractically long (and
// exceeds what the route would ever page back in one response).
const MAX_FILTER_IDS = 500;

// 12-color cluster palette. Ordered for max chromatic spread; stays
// readable on both the light + dark themes the SPA ships.
const CLUSTER_PALETTE = [
  "#5b8def", // blue
  "#f48fb1", // pink
  "#81c784", // green
  "#ffb74d", // orange
  "#ba68c8", // purple
  "#4dd0e1", // cyan
  "#ffd54f", // amber
  "#a1887f", // brown
  "#7986cb", // indigo
  "#aed581", // light-green
  "#ff8a65", // deep-orange
  "#90a4ae", // blue-grey
];

// W1.atlas — tiny localStorage toggle persistence, mirroring the
// useInspectorCollapsed/useInspectorTab pattern (one key per toggle, baked-in
// default, best-effort — a denied/full localStorage just means the choice
// doesn't survive reload). Not lifted to a shared `hooks/` file: AtlasView is
// the only consumer today and this phase doesn't own new hook files.
const ATLAS_TOGGLE_PREFIX = "kb:atlas:toggle:";
function readAtlasToggle(key: string, fallback: boolean): boolean {
  if (typeof localStorage === "undefined") return fallback;
  try {
    const v = localStorage.getItem(ATLAS_TOGGLE_PREFIX + key);
    return v === null ? fallback : v === "true";
  } catch {
    return fallback;
  }
}
function writeAtlasToggle(key: string, value: boolean) {
  try {
    localStorage.setItem(ATLAS_TOGGLE_PREFIX + key, value ? "true" : "false");
  } catch {
    // localStorage denied/full — the toggle still works for this session.
  }
}

// W3.T-c — how long one time-lapse frame holds before the player steps to
// the next. Slow enough to read a 6-frame history without scrubbing, fast
// enough that a 24-frame one (DEFAULT_ATLAS_FRAMES_KEEP) doesn't outlast
// the operator's patience.
const TIMELAPSE_STEP_MS = 900;

// Unit (0..1) atlas coords → the logical W × H canvas box. Module-level so
// the live placement and the time-lapse frame placement provably share ONE
// affine mapping: that's what makes a frame's cluster centroids directly
// comparable with the live map's (`lib/clusterRemap.ts` works in whatever
// space it's handed, but both sides must be in the SAME one).
function toLogicalX(u: number): number {
  return INSET + u * (W - INSET * 2);
}
function toLogicalY(v: number): number {
  return INSET + v * (H - INSET * 2);
}
// W3.F-c — the inverses, for place mode: a drop point on the canvas is
// read back into the unit space the operator field stores. Same affine
// mapping as above, solved for `u`/`v` — never a second, hand-tuned one.
function unitFromLogicalX(x: number): number {
  return (x - INSET) / (W - INSET * 2);
}
function unitFromLogicalY(y: number): number {
  return (y - INSET) / (H - INSET * 2);
}

// W3.F-c — how long the sidecar write waits after the last drag before it
// goes out. Mirrors `BoardCanvas`'s `SAVE_DEBOUNCE_MS` (the other JSON
// Canvas sidecar this SPA writes): one whole-document PUT per gesture, not
// one per pointer move.
const FIELD_SAVE_DEBOUNCE_MS = 500;

// W3.F-c — a loci-tour stop's camera: zoom in enough that the artifact's
// neighbourhood reads, not so far that the rest of the map is gone (the
// same 1.6-2.5 band `panToCluster` lives in), and how long the flight
// takes. `prefers-reduced-motion` skips the flight entirely — see `flyTo`.
const TOUR_ZOOM = 2.2;
const TOUR_FLIGHT_MS = 520;

type Placed = {
  doc: DocSummary;
  x: number; // logical (W × H) coord space
  y: number;
  // The cluster id EXACTLY as the daemon recorded it — grouping, the
  // legend, the highlight filter and the hover tip all read this one.
  cluster: number;
  // W3.T-c — the id to PAINT with. Identical to `cluster` on the live map;
  // on a time-lapse frame it is that frame's cluster remapped onto the live
  // map's ids (`lib/clusterRemap.ts`), because k-means renumbers clusters
  // between recomputes and playing the raw ids strobes the palette. Colour
  // only — never written back, never used for grouping.
  colorCluster: number;
  fallback: boolean;
};

type ClusterLabel = {
  cluster: number;
  x: number;
  y: number;
  label: string;
  color: string;
};

// W2.3b — the draw loop's label pass only ever reads x/y/label/color
// (never `cluster`), so the three LOD levels (continent / per-cluster /
// detail) share this looser shape; `ClusterLabel[]` (which has an extra
// `cluster` field) is structurally assignable to it.
type LodLabel = {
  x: number;
  y: number;
  label: string;
  color: string;
};

export default function AtlasView({
  docs,
  kb,
  edges = [],
  points,
  docsTotal,
  onRecomputeDone,
  selection: selectionProp,
  onSelectionChange,
  onRowsChange,
  shell = false,
}: {
  docs: DocSummary[];
  kb: string;
  edges?: AtlasEdge[];
  // W3.M-c — the FULL-corpus atlas point set (`useAtlasPoints`, `GET
  // /api/kb/{kb}/atlas/points`). OPTIONAL: absent/loading/errored falls back
  // to `docs` (today's behaviour — whatever page the gallery loaded), so
  // nothing regresses if the endpoint is unavailable.
  points?: AtlasPointsResponse;
  // The gallery's own post-filter total (`useDocs`' `total`, NOT
  // `docs.length`) — lets the status line report an honest "N of M" when
  // `points` is absent and `docs` is a truncated page, rather than silently
  // claiming `docs.length` is everything.
  docsTotal?: number;
  onRecomputeDone?: () => void;
  // W3.M-d — the lasso working set as a CONTROLLED value. Absent (the
  // gallery's `?view=atlas`) keeps the pre-existing uncontrolled state;
  // present (MapShell) hands ownership to the shell, which projects the
  // same set as a list beside the map. Standard React controlled-input
  // shape: pass BOTH or neither.
  selection?: Set<string>;
  onSelectionChange?: (next: Set<string>) => void;
  // W3.M-d — the rows the atlas is actually drawing (the full-corpus
  // `points` merge when present, else the loaded `docs` page). Lifted so the
  // shell's projection panel can render the selection from rows ALREADY IN
  // HAND — it costs ZERO new fetches, which is the whole reason the panel is
  // allowed to exist beside a full-bleed map.
  onRowsChange?: (rows: DocSummary[]) => void;
  // W3.M-d — rendered inside MapShell: full-bleed (no card padding/border,
  // canvas fills its column instead of holding the 600/360 aspect ratio) and
  // the in-canvas WorkingSetBar is suppressed because the shell's projection
  // panel owns those verbs. Desktop-only by construction — MapShell itself
  // is gated on `!useIsMobile()`.
  shell?: boolean;
}) {
  const [hover, setHover] = useState<Placed | null>(null);
  // v0.11 A1 — selected node drives the AtlasInspector rail. Click on
  // a dot selects it; click on empty canvas clears the selection;
  // cmd/ctrl/middle-click opens in a new tab. The inspector's preview
  // button is the design-canonical navigate path.
  const [selected, setSelected] = useState<DocSummary | null>(null);
  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const [showDensity, setShowDensity] = useState(false);
  // v0.14 S7 — sessions overlay. When on, fetch the last
  // ATLAS_SESSION_LIMIT sessions and their touches; draw one polyline
  // per session through the touched-artifact dots. Polylines go
  // BENEATH the dots so dots stay clickable.
  const [showSessions, setShowSessions] = useState(false);
  const [sessionPolylines, setSessionPolylines] = useState<
    {
      id: string;
      color: string;
      ids: string[];
      // CT-A6 — the subset of `ids` the /touches scan found via a
      // path-substring match rather than a literal id hit (join-tier
      // `TouchesConfidence::Fuzzy`). Purely additive to `ids`: used only
      // to dash the polyline hop leading into a fuzzy-tier dot, never to
      // drop/reorder points.
      fuzzyIds: Set<string>;
      preview?: string;
    }[]
  >([]);
  // v0.14 T4 — when one session in the legend is focused, other
  // polylines dim to 8% so the focused path reads clearly even on a
  // dense corpus.
  const [focusedSession, setFocusedSession] = useState<string | null>(null);

  // W1.atlas — cluster legend highlight. Same temporary-dim shape as
  // `focusedSession` above: clicking a legend row dims every other
  // cluster's dots so the chosen one reads clearly, and (when server
  // c-TF-IDF terms are on the wire) doubles as the row's ScoreExplain-style
  // term-breakdown trigger — see `ClusterTermExplain` below.
  const [highlightCluster, setHighlightCluster] = useState<number | null>(null);

  // W2.3b — the atlas becomes an instrument: freehand lasso → a working
  // set. `lassoMode` toggles the toolbar mode (mouse-only — see the
  // handler comments below); `lassoPath` is the in-progress polygon (logical
  // coords, cleared once a drag ends); `selection` is the resulting id set,
  // independent of the single-dot `selected` above (a lasso'd working set
  // and a single inspected dot can coexist — e.g. clicking a true-neighbor
  // row re-centers `selected` without disturbing `selection`).
  const [lassoMode, setLassoMode] = useState(false);
  const [lassoPath, setLassoPath] = useState<{ x: number; y: number }[] | null>(
    null,
  );
  // W3.M-d — `selection` is now optionally CONTROLLED (MapShell owns it so
  // the projection panel and the map can't disagree). Uncontrolled is the
  // unchanged gallery behaviour; the local state below is simply unused when
  // a `selection` prop is supplied.
  const [ownSelection, setOwnSelection] = useState<Set<string>>(() => new Set());
  const selection = selectionProp ?? ownSelection;
  // The controlled/uncontrolled decision + the parent's setter ride a ref so
  // `setSelection` is stable for the app's lifetime — it is called from
  // effects (the kb-change reset) and pointer handlers whose dependency lists
  // must NOT churn when the parent re-renders with a fresh Set identity.
  const selectionCtl = useRef<{
    controlled: boolean;
    on?: (next: Set<string>) => void;
  }>({ controlled: false });
  selectionCtl.current = {
    controlled: selectionProp != null,
    on: onSelectionChange,
  };
  const setSelection = useCallback((next: Set<string>) => {
    const { controlled, on } = selectionCtl.current;
    if (controlled) on?.(next);
    else setOwnSelection(next);
  }, []);
  const lassoDrawing = useRef(false);

  // W1.atlas — view toggles, persisted per the pattern above. "dim read"
  // defaults OFF (nothing dims until asked); "size by links" defaults ON,
  // matching density/sessions' always-on-unless-toggled-off feel.
  const [dimRead, setDimReadState] = useState(() =>
    readAtlasToggle("dim-read", false),
  );
  const setDimRead = (next: boolean) => {
    setDimReadState(next);
    writeAtlasToggle("dim-read", next);
  };
  const [sizeByLinks, setSizeByLinksState] = useState(() =>
    readAtlasToggle("size-links", true),
  );
  const setSizeByLinks = (next: boolean) => {
    setSizeByLinksState(next);
    writeAtlasToggle("size-links", next);
  };

  // W1.atlas — read-state dimming reads the same per-artifact progress map
  // the gallery cards use. It's built from the newest 200 `kind: "open"`
  // history rows (useReadingProgress's FETCH_LIMIT) — an honest cap, not a
  // full-corpus read-state index, so on a kb with >200 recent opens the
  // oldest artifacts silently fall out of the dim/no-dim signal until
  // they're revisited.
  const progress = useReadingProgress(kb);

  // W3.M-c — the atlas's own "dim in place" search (MapSearchOverlay): a
  // matched-id set that dims every dot NOT in it, one more alpha multiplier
  // alongside dimRead/highlightCluster/selection above. `null` = no active
  // search (nothing dims on this signal). Distinct from the lasso's
  // `selection` — dimming is a search's reach, selection is a working set
  // the user has explicitly drawn.
  const [dimIds, setDimIds] = useState<Set<string> | null>(null);

  // W3.T-c — the corpus time-lapse. `timelapseOpen` gates BOTH the scrubber
  // chrome and the `["atlasHistory", kb]` fetch (the hook takes `undefined`
  // until then), so a plain atlas visit costs nothing extra. `frameIdx`
  // indexes the CHRONOLOGICAL frame list (oldest first — the wire is newest
  // first), so the slider runs left-to-right through time like every other
  // scrubber. `playing` drives the one timer in this component; it defaults
  // to false and the reduced-motion branch never starts it (see the player
  // effect below).
  const [timelapseOpen, setTimelapseOpen] = useState(false);
  const [frameIdx, setFrameIdx] = useState(0);
  const [playing, setPlaying] = useState(false);
  const historyQuery = useAtlasHistory(timelapseOpen ? kb : undefined);
  // Wire order is newest-first (the route's `ORDER BY created_at_unix DESC`);
  // reverse ONCE here and let every index below mean "position in time".
  const frames = useMemo(
    () => (historyQuery.data?.frames ?? []).slice().reverse(),
    [historyQuery.data],
  );
  // Clamp on every list change (a new frame landing, a prune, a kb switch)
  // so the slider can never point past the end.
  const clampedIdx =
    frames.length === 0 ? 0 : clamp(frameIdx, 0, frames.length - 1);
  const activeFrameMeta = timelapseOpen ? (frames[clampedIdx] ?? null) : null;
  const frameQuery = useAtlasFrame(kb, activeFrameMeta?.id ?? null);
  const frameData = timelapseOpen ? (frameQuery.data ?? null) : null;

  // W3.F-c — THE ONION SKIN: the operator's own field, drawn over the
  // machine's. Three switches, all OFF by default (the map you get by
  // arriving is still the machine's alone):
  //   `fieldOn`   the ghost pass — operator dots + a leader line to each
  //               doc's machine position, plus the operator's named islands.
  //   `fieldHeat` recolour the dots by how far the two fields disagree,
  //               using the SERVER's aligned distances (never a client-side
  //               re-fit — see api/atlasField.ts).
  //   `placeMode` drag a dot to say where YOU think it belongs. This writes
  //               the JSON Canvas sidecar ONLY; machine coordinates are
  //               read-only and nothing here ever writes one back.
  // Both persisted toggles use the same tiny localStorage helper the other
  // atlas toggles do; place mode is per-session (a drag mode that survived
  // a reload would be a trap) and resets on a kb switch below.
  const [fieldOn, setFieldOnState] = useState(() =>
    readAtlasToggle("field", false),
  );
  const setFieldOn = (next: boolean) => {
    setFieldOnState(next);
    writeAtlasToggle("field", next);
    if (next) censusBump("atlas.field.show");
    else setPlaceMode(false);
  };
  const [fieldHeat, setFieldHeatState] = useState(() =>
    readAtlasToggle("field-heat", false),
  );
  const setFieldHeat = (next: boolean) => {
    setFieldHeatState(next);
    writeAtlasToggle("field-heat", next);
    if (next) censusBump("atlas.field.heat");
  };
  const [placeMode, setPlaceMode] = useState(false);
  // The overlay's chrome lives inside the field bar, so heat only paints
  // while the overlay itself is up — one home for the operator half.
  const heatOn = fieldOn && fieldHeat;
  const fieldQuery = useAtlasField(fieldOn ? kb : undefined);
  const disagreementQuery = useAtlasFieldDisagreement(fieldOn ? kb : undefined);
  // Local unsaved edits. `null` means "the server's copy is the truth";
  // a drag sets it, and the debounced PUT's response (or its failure)
  // clears it — the same single-writer stance BoardCanvas takes, minus the
  // per-board remount, since this view is reused across kbs.
  const [fieldDraft, setFieldDraft] = useState<CanvasDoc | null>(null);
  const [fieldSaving, setFieldSaving] = useState(false);
  const fieldSaveTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const fieldSaveSeq = useRef(0);
  // The in-flight place-mode drag: which artifact, and where the pointer
  // has it right now (logical coords). Not committed until pointer-up.
  const [fieldDrag, setFieldDrag] = useState<{
    file: string;
    x: number;
    y: number;
  } | null>(null);
  const fieldDragRef = useRef<{ file: string } | null>(null);
  const queryClient = useQueryClient();

  // W3.F-c — loci tours. Pure navigation over an EXISTING reading list —
  // no tour store, no progress, no completion (see lib/atlasTour.ts).
  const [tourOpen, setTourOpen] = useState(false);

  // W1.atlas — forward-compatible color-mode slot; MI-W4.5 is the first
  // consumer beyond "clusters" (the cluster legend below stays gated on
  // `colorMode === "clusters"`). "salience"/"decay" paint dots via
  // `lib/atlasMemory.ts` and only ever appear in the toolbar switcher when
  // `hasMemoryColorModes` is true (a memory-scoped kb). Cameras persist
  // this alongside pan/zoom, so a saved camera from before this phase
  // (always "clusters") replays unchanged.
  const [colorMode, setColorMode] = useState<AtlasCameraColorMode>("clusters");

  // W1.atlas — bookmarkable cameras (client-view state only: pan/zoom/
  // color-mode, never corpus data). Reloaded whenever `kb` changes since
  // AtlasView is reused across kbs without remounting (see the recompute
  // cleanup effect below for the same caveat).
  const confirm = useConfirm();
  const [cameras, setCameras] = useState<AtlasCamera[]>(() => listCameras(kb));
  const [camerasOpen, setCamerasOpen] = useState(false);
  const [savingCamera, setSavingCamera] = useState(false);
  const [cameraNameDraft, setCameraNameDraft] = useState("");
  const camerasRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    setCameras(listCameras(kb));
    setCamerasOpen(false);
    setSavingCamera(false);
    setCameraNameDraft("");
  }, [kb]);

  useEffect(() => {
    if (!camerasOpen) return;
    const onDoc = (e: globalThis.MouseEvent) => {
      if (!camerasRef.current?.contains(e.target as Node)) {
        setCamerasOpen(false);
        setSavingCamera(false);
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setCamerasOpen(false);
        setSavingCamera(false);
      }
    };
    document.addEventListener("mousedown", onDoc);
    window.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      window.removeEventListener("keydown", onKey);
    };
  }, [camerasOpen]);

  // W1.atlas — census the atlas view being opened at all (evidence
  // for the "did anyone use this" question the plan's standing rule 3
  // asks). Empty deps: once per mount, not once per `kb` switch (Gallery
  // reuses one AtlasView instance across kbs — see the recompute cleanup
  // effect's comment below).
  useEffect(() => {
    censusBump("atlas.open");
  }, []);

  // W1.atlas — server c-TF-IDF cluster labels (W1.B). A fetch failure
  // (404 before the server phase ships, or any other error) leaves
  // `.data` undefined, which the `clusters` memo below treats identically
  // to "no rows yet" — same fallback path either way.
  const atlasLabelsQuery = useQuery({
    queryKey: ["atlasLabels", kb] as const,
    queryFn: ({ signal }) => fetchAtlasLabels(kb, signal),
  });
  const serverLabelsByCluster = useMemo(() => {
    const m = new Map<number, AtlasLabelTerm[]>();
    for (const c of atlasLabelsQuery.data?.clusters ?? []) {
      if (c.terms.length > 0) m.set(c.cluster, c.terms);
    }
    return m;
  }, [atlasLabelsQuery.data]);

  // v0.14 U2 — sessions whose touches set includes the selected dot.
  // Derived from the already-loaded `sessionPolylines` so the
  // inspector doesn't pay for an extra round-trip; updates whenever
  // selection or the overlay data changes.
  const touchedBySelected = useMemo(() => {
    if (!selected || !showSessions || sessionPolylines.length === 0) {
      return [];
    }
    return sessionPolylines
      .filter((poly) => poly.ids.includes(selected.id))
      .map((poly) => ({
        sessionId: poly.id,
        color: poly.color,
        preview: poly.preview,
      }));
  }, [selected, showSessions, sessionPolylines]);
  const dragState = useRef<{
    mx: number;
    my: number;
    px: number;
    py: number;
    moved: boolean;
  } | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  // MB4 — touch gesture state. `pointers` tracks active touch points by id;
  // `pinchRef` holds the gesture anchor (start distance + zoom + the logical
  // point under the initial midpoint) so a two-finger pinch zooms about the
  // fingers. Mouse keeps the existing handlers; the pointer handlers
  // early-return for `mouse`, so the desktop path is untouched.
  const pointers = useRef(new Map<number, { x: number; y: number }>());
  const pinchRef = useRef<{
    dist: number;
    zoom: number;
    midLogical: { x: number; y: number };
  } | null>(null);

  // v0.14 S7 — load sessions + their touches when the overlay
  // toggles on. Bounded by ATLAS_SESSION_LIMIT to keep the fan-out
  // tight; each session resolves its touches independently so a slow
  // transcript doesn't block the others. session.captured doesn't
  // refetch the overlay automatically (the user can toggle off+on
  // to refresh) to avoid surprise polyline reshuffles mid-zoom.
  useEffect(() => {
    if (!showSessions) {
      setSessionPolylines([]);
      return;
    }
    const ctrl = new AbortController();
    (async () => {
      try {
        const rows = await fetchSessions(ctrl.signal);
        const top: SessionRow[] = rows.slice(0, ATLAS_SESSION_LIMIT);
        const results = await Promise.all(
          top.map(async (r) => {
            try {
              const t = await fetchSessionTouches(r.session_id, ctrl.signal);
              // CT-A6 — `artifacts` (per-row confidence) is additive
              // alongside `artifact_ids`; fall back to an empty fuzzy set
              // on an older/cached response that lacks it.
              const fuzzyIds = new Set(
                (t.artifacts ?? [])
                  .filter((a) => a.confidence === "fuzzy")
                  .map((a) => a.id),
              );
              return {
                id: r.session_id,
                color: sessionColorFor(r.session_id),
                ids: t.artifact_ids,
                fuzzyIds,
                preview: r.first_user_prompt,
              };
            } catch {
              return {
                id: r.session_id,
                color: sessionColorFor(r.session_id),
                ids: [],
                fuzzyIds: new Set<string>(),
                preview: r.first_user_prompt,
              };
            }
          }),
        );
        setSessionPolylines(results.filter((r) => r.ids.length > 0));
      } catch {
        setSessionPolylines([]);
      }
    })();
    return () => ctrl.abort();
  }, [showSessions]);

  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const cleanupRef = useRef<(() => void) | null>(null);

  // Tear down any in-flight recompute subscription on unmount AND on a
  // `kb` change. v0.7.1 P2: Gallery reuses one `AtlasView` instance
  // across kbs (React reconciles by position — no remount), so without
  // the `kb` dependency a recompute started on kb A kept its SSE
  // subscription + 60s timer closed over A's `run`/`onRecomputeDone`,
  // and A's late completion bled a spurious refetch / error banner onto
  // whatever kb the user had since switched to.
  useEffect(() => {
    return () => {
      if (cleanupRef.current) {
        cleanupRef.current();
        cleanupRef.current = null;
      }
    };
  }, [kb]);

  useEffect(() => {
    setPending(false);
    setError(null);
  }, [kb]);

  // W2.3b — selection/lasso is client-view state, same "reused across kbs
  // without remounting" caution as the cameras effect above: without this,
  // a working set lasso'd on kb A would keep dimming/ringing dots on kb B
  // (ids from A coincidentally matching nothing, or worse, colliding).
  useEffect(() => {
    setSelection(new Set());
    setLassoPath(null);
    setLassoMode(false);
    lassoDrawing.current = false;
    // W3.M-c — same cross-kb bleed risk as the selection above: a dim
    // search run on kb A must not keep dimming kb B's dots.
    setDimIds(null);
    // W3.T-c — frame ids are per-kb, so an index (let alone a running
    // player) carried across a kb switch would scrub someone else's
    // history. Rewind to the oldest frame and stop.
    setFrameIdx(0);
    setPlaying(false);
    // W3.F-c — the operator field is per-kb too: an unsaved draft, a
    // half-finished drag or an open place mode carried across a kb switch
    // would write kb A's placement into kb B's sidecar. (The queries
    // themselves re-key on `kb`, so only this local state needs clearing.)
    setFieldDraft(null);
    setFieldDrag(null);
    fieldDragRef.current = null;
    setPlaceMode(false);
    setTourOpen(false);
  }, [kb]);

  // W3.F-c — one debounced whole-document PUT per gesture (the route has
  // no partial update by design: the sidecar is stored verbatim). The
  // response splices into the ["atlasField", kb] cache entry, and the
  // daemon's own `atlas.field.updated` event reconciles every other tab
  // through the ONE SSE bridge (#23/#24) — this component never subscribes.
  useEffect(
    () => () => {
      if (fieldSaveTimer.current) clearTimeout(fieldSaveTimer.current);
    },
    [],
  );
  const scheduleFieldSave = (next: CanvasDoc) => {
    // The draft is only handed back to the server's copy when the write it
    // came from is the LATEST one: a second drag landing while the first
    // PUT is in flight must not have its placement blink away when that
    // older response resolves.
    const seq = fieldSaveSeq.current + 1;
    fieldSaveSeq.current = seq;
    setFieldDraft(next);
    setFieldSaving(true);
    if (fieldSaveTimer.current) clearTimeout(fieldSaveTimer.current);
    fieldSaveTimer.current = setTimeout(() => {
      fieldSaveTimer.current = null;
      putAtlasField(kb, next)
        .then((saved) => {
          queryClient.setQueryData(["atlasField", kb], saved);
          if (fieldSaveSeq.current !== seq) return;
          setFieldDraft(null);
          setFieldSaving(false);
        })
        .catch((e) => {
          // Drop the draft rather than keep showing a placement the daemon
          // rejected (a read-only corpus answers 409) — the ghost snapping
          // back IS the error signal, with the toast naming why.
          if (fieldSaveSeq.current === seq) {
            setFieldDraft(null);
            setFieldSaving(false);
          }
          toast.err(
            `operator field save failed: ${e instanceof Error ? e.message : String(e)}`,
          );
        });
    }, FIELD_SAVE_DEBOUNCE_MS);
  };

  // W3.M-c — `docs` is a lookup table keyed by id for the RICH per-artifact
  // fields (`summary`/`tags`/`word_count`/`folder`/…) the paged gallery
  // fetch carries but the lean full-corpus `AtlasPoint` doesn't. Merged
  // below so a doc present in BOTH the loaded page and the full point set
  // keeps its rich card data; one present ONLY in the full set (past the
  // gallery's page) renders with the thin `AtlasPoint` fields and degrades
  // gracefully in the inspector (every extra `DocSummary` field is
  // optional).
  const richById = useMemo(() => {
    const m = new Map<string, DocSummary>();
    for (const d of docs) m.set(d.id, d);
    return m;
  }, [docs]);

  // W3.M-c — the render source: the full-corpus `points` prop when present
  // (the fix for the measured "atlas only draws the first page" defect),
  // falling back to `docs` (today's behaviour) when the endpoint is
  // absent/loading/errored. `hasFullPoints` and the "N of M" honesty check
  // below both key off this same `points` prop, not `sourceDocs.length`
  // alone, so a same-size coincidence never masquerades as "the full set".
  const hasFullPoints = points != null;
  const sourceDocs: DocSummary[] = useMemo(() => {
    if (!points) return docs;
    return points.points.map((p): DocSummary => {
      const rich = richById.get(p.id);
      if (rich) return rich;
      return {
        id: p.id,
        title: p.title,
        path: p.source_relative,
        folder: "",
        source_relative: p.source_relative,
        kb_category: p.kb_category ?? null,
        atlas_x: p.atlas_x ?? null,
        atlas_y: p.atlas_y ?? null,
        atlas_cluster: p.cluster ?? null,
      };
    });
  }, [points, docs, richById]);

  // MI-W4.5 — memory metadata keyed by id, read straight off the raw
  // `AtlasPoint` (never folded into `sourceDocs`'s `DocSummary` projection
  // above — that shape is shared with the paged-gallery fallback and every
  // other atlas consumer, and salience/decay/pinned/forgotten/supersedes are
  // memory-map-only fields). Empty when `points` is absent (docs-only
  // fallback) or the kb isn't memory-scoped (every point's four memory
  // fields absent server-side — `hasMemoryColorModes` below is then false).
  const memoryById = useMemo(() => {
    const m = new Map<string, AtlasPoint>();
    if (points) for (const p of points.points) m.set(p.id, p);
    return m;
  }, [points]);
  const hasMemoryColorModes = useMemo(
    () => (points ? hasMemoryPoints(points.points) : false),
    [points],
  );
  // A memory that supersedes another drawn point becomes one more edge on
  // the SAME curved-edge layer the `kind=link` graph already renders
  // (invariant #29) — merged in once here so `edgeLines`/`degreeById` below
  // need no separate memory-aware branch.
  const drawEdges = useMemo(() => {
    if (!points) return edges;
    return mergeAtlasEdges(edges, supersedeEdges(points.points));
  }, [edges, points]);

  // W3.M-d — hand the drawn row-set up to the shell (MapShell's projection
  // panel). Effect-scoped so the parent's setState happens after commit, and
  // memo-identity-gated so it fires once per row-set change, not per render.
  useEffect(() => {
    onRowsChange?.(sourceDocs);
  }, [sourceDocs, onRowsChange]);

  // Honest "N of M" check for the status line: `docsTotal` is the gallery's
  // post-filter total (`useDocs`' `total`, not `docs.length`) — when the
  // full point set isn't in play and it exceeds what's actually rendered,
  // the source is a truncated page and the copy says so (the defect this
  // phase fixes: the atlas used to draw a subset while claiming it was
  // everything).
  const shownCount = sourceDocs.length;
  const totalKnown = hasFullPoints ? points!.total : docsTotal;
  const isTruncatedPage = totalKnown != null && totalKnown > shownCount;

  // Honest "has the daemon ever computed a layout" check — independent
  // of the per-dot `fallback` flag, which only flags *stale* dots in an
  // otherwise-real atlas (the mixed case).
  const hasAtlasData = useMemo(
    () =>
      sourceDocs.some(
        (d) => typeof d.atlas_x === "number" && typeof d.atlas_y === "number",
      ),
    [sourceDocs],
  );

  const livePlaced: Placed[] = useMemo(() => {
    return sourceDocs.map((d) => {
      let u: number;
      let v: number;
      let fallback = false;
      if (typeof d.atlas_x === "number" && typeof d.atlas_y === "number") {
        u = d.atlas_x;
        v = d.atlas_y;
      } else {
        [u, v] = hashPoint(d.id);
        fallback = true;
      }
      const cluster = typeof d.atlas_cluster === "number" ? d.atlas_cluster : 0;
      return {
        doc: d,
        x: toLogicalX(u),
        y: toLogicalY(v),
        cluster,
        // Live map: nothing to remap onto — the ids ARE today's ids.
        colorCluster: cluster,
        fallback: fallback && hasAtlasData,
      };
    });
  }, [sourceDocs, hasAtlasData]);

  const docById = useMemo(() => {
    const m = new Map<string, DocSummary>();
    for (const d of sourceDocs) m.set(d.id, d);
    return m;
  }, [sourceDocs]);

  // W3.T-c — THE SOURCE SWAP. When a time-lapse frame is active, `placed`
  // is rebuilt from that frame's server-aligned points; every other draw
  // pass (edges, labels, LOD, dimming, lasso, hit-testing, the inspector's
  // stress points) reads `placed` and is untouched — one renderer, one
  // source, not a second canvas.
  //
  // Coordinates arrive ALREADY Procrustes-aligned into the newest frame's
  // space (`GET .../atlas/history/{id}`, default `align_to` = newest). We
  // deliberately do NOT re-align here: the fit is done once on the daemon so
  // `kb atlas show` and this view agree byte for byte.
  //
  // A frame names the corpus as it was, so it can list artifacts that have
  // since been deleted. Those are DROPPED (and counted — the scrubber says
  // how many) rather than drawn as phantom dots: every downstream affordance
  // — click-to-open, lasso → add-to-reading-list, the inspector — assumes a
  // dot is a live artifact, and a placeholder would hand the operator a
  // broken link and a list entry pointing at nothing.
  const framePlaced = useMemo((): {
    placed: Placed[];
    droppedCount: number;
  } | null => {
    if (!frameData) return null;
    const rows = frameData.points
      .map((p) => {
        const doc = docById.get(p.artifact_id);
        if (!doc) return null;
        return {
          doc,
          x: toLogicalX(p.x),
          y: toLogicalY(p.y),
          cluster: p.cluster,
        };
      })
      .filter((r): r is NonNullable<typeof r> => r !== null);
    // COLOUR ONLY: k-means renumbers clusters between recomputes (kb-core
    // atlas.rs's GC-B1 note), so frame cluster 3 and today's cluster 3 are
    // unrelated labels. Match this frame's centroids onto the live map's and
    // paint through that; `cluster` itself stays the daemon's own value.
    const remap = remapClusters(centroidsOf(rows), centroidsOf(livePlaced));
    return {
      placed: rows.map((r) => ({
        ...r,
        colorCluster: remappedCluster(remap, r.cluster),
        // A frame's points are recorded coordinates by definition — never
        // the hash-placement fallback the live map uses for un-embedded docs.
        fallback: false,
      })),
      droppedCount: frameData.points.length - rows.length,
    };
  }, [frameData, docById, livePlaced]);

  const placed: Placed[] = framePlaced ? framePlaced.placed : livePlaced;

  // W3.T-c — the ONE timer in this component, and the only one the atlas is
  // allowed: this is an EXPLICIT operator-initiated animation (they pressed
  // play), not autoplay-on-arrival. Three rules keep it that way:
  //   1. `playing` defaults to FALSE — arriving at the atlas, or opening the
  //      scrubber, never starts anything moving.
  //   2. It STOPS at the last frame (no wrap-around loop) — the time-lapse
  //      ends where the live map is, and stays there.
  //   3. `prefers-reduced-motion: reduce` disables the animation outright
  //      (the effect never schedules a step and the play control is hidden);
  //      the slider stays, so the whole history is still reachable by hand.
  // A per-step `setTimeout` (not a standing `setInterval`) means every tick
  // is re-derived from current state and nothing survives an unmount, a kb
  // switch, or a pause.
  const reducedMotion = useMemo(() => reducedMotionRequested(), []);
  // Keep the stored index inside the list (a prune, or a kb whose history is
  // shorter, can strand it past the end). `clampedIdx` already guards every
  // READ; this writes the clamp back so the player's `i + 1` steps from the
  // frame the operator can actually see.
  useEffect(() => {
    if (frames.length > 0 && frameIdx > frames.length - 1) {
      setFrameIdx(frames.length - 1);
    }
  }, [frames.length, frameIdx]);
  useEffect(() => {
    if (!playing || reducedMotion) return;
    if (frames.length === 0 || clampedIdx >= frames.length - 1) {
      setPlaying(false);
      return;
    }
    const t = setTimeout(() => {
      setFrameIdx((i) => Math.min(i + 1, frames.length - 1));
    }, TIMELAPSE_STEP_MS);
    return () => clearTimeout(t);
  }, [playing, reducedMotion, clampedIdx, frames.length]);

  const placedById = useMemo(() => {
    const m = new Map<string, Placed>();
    for (const p of placed) m.set(p.doc.id, p);
    return m;
  }, [placed]);

  // W2.3b — the lightweight (id, x, y) view of `placed` the true-neighbors
  // rail's client-side 2-D KNN (layout-stress badge) needs. Kept as its
  // own memo rather than passing `placed`/`Placed[]` straight through so
  // AtlasInspector's dependency is the pure `{id,x,y}[]` shape
  // `atlasSelection.ts` already tests against, not the `Placed` type.
  const stressPoints = useMemo(
    () => placed.map((p) => ({ id: p.doc.id, x: p.x, y: p.y })),
    [placed],
  );

  // W3.F-c — the operator field, joined onto what is actually drawn.
  //
  // The join key on the wire is the SOURCE-RELATIVE PATH (that is what a
  // JSON Canvas `file` node carries, and what `kb_core::atlas_field`
  // matches on), while `placed`/`placedById` are keyed by artifact id —
  // hence this second index rather than reusing `placedById`.
  const placedBySourceRel = useMemo(() => {
    const m = new Map<string, Placed>();
    for (const p of placed) {
      if (p.doc.source_relative) m.set(p.doc.source_relative, p);
    }
    return m;
  }, [placed]);

  // The sidecar as it stands right now: an unsaved drag wins over the
  // server's copy until the PUT lands (or fails).
  const fieldDoc: CanvasDoc = fieldDraft ?? fieldQuery.data ?? EMPTY_CANVAS;
  const localPlacements = useMemo(
    () => fieldPlacements(fieldDoc),
    [fieldDoc],
  );
  const disagreements = useMemo(
    () => disagreementQuery.data?.disagreements ?? [],
    [disagreementQuery.data],
  );
  // The wire is sorted largest-first, so the head IS the max — no scan,
  // and no chance of disagreeing with the server about which doc is worst.
  const maxDisagreement = disagreements.length > 0 ? disagreements[0].distance : 0;
  const disagreementBySourceRel = useMemo(() => {
    const m = new Map<string, number>();
    for (const d of disagreements) m.set(d.id, d.distance);
    return m;
  }, [disagreements]);
  // The daemon's OWN alignment, recovered from its own (raw → aligned)
  // pairs — not a second fit (see api/atlasField.ts). `null` when the field
  // is too small/degenerate to determine one; the islands pass says so
  // rather than guessing.
  const fieldTransform = useMemo(
    () => recoverFieldTransform(disagreements),
    [disagreements],
  );

  // Ghost dots: where the OPERATOR put each artifact, in the machine's
  // frame. Server rows first (aligned server-side, the authoritative
  // positions); an unsaved draft then overrides them for every locally
  // placed artifact, so a drag reads back instantly instead of waiting a
  // round-trip. A placement whose artifact isn't drawn on this map is
  // dropped — the same rule the daemon applies to the join.
  const fieldGhosts = useMemo(() => {
    if (!fieldOn) return [];
    const out = new Map<
      string,
      { file: string; x: number; y: number; mx: number; my: number; pending: boolean }
    >();
    for (const d of disagreements) {
      const p = placedBySourceRel.get(d.id);
      if (!p) continue;
      out.set(d.id, {
        file: d.id,
        x: toLogicalX(d.operator_x),
        y: toLogicalY(d.operator_y),
        mx: p.x,
        my: p.y,
        pending: false,
      });
    }
    // Placements the server rows DON'T cover: an unsaved drag, or (the
    // honest common case on a kb with no embeddings) an artifact the join
    // dropped because there is no machine coordinate to compare against.
    // Those are projected from the raw sidecar position — but only when
    // that projection is DEFINED:
    //   * the server's transform is recoverable ⇒ use it, exactly as the
    //     daemon would; or
    //   * nothing joined at all ⇒ there is no frame to convert into, so
    //     the raw position is the only position that exists (and the field
    //     bar says the field is unmatched).
    // With a partially-joined field and no recoverable transform, a raw
    // position would be a guess in someone else's coordinate frame — so it
    // is not drawn at all.
    const canProject = fieldTransform != null || disagreements.length === 0;
    if (canProject) {
      const t = fieldTransform ?? IDENTITY_FIELD_TRANSFORM;
      const pending = fieldDraft != null;
      for (const [file, pl] of localPlacements) {
        // A saved placement the server already scored keeps the server's
        // aligned position; an unsaved draft overrides it (that's the
        // whole point of a draft).
        if (!pending && out.has(file)) continue;
        const p = placedBySourceRel.get(file);
        if (!p) continue;
        const a = applyFieldTransform(t, pl.x, pl.y);
        out.set(file, {
          file,
          x: toLogicalX(a.x),
          y: toLogicalY(a.y),
          mx: p.x,
          my: p.y,
          pending,
        });
      }
    }
    return [...out.values()];
  }, [
    fieldOn,
    disagreements,
    placedBySourceRel,
    fieldDraft,
    localPlacements,
    fieldTransform,
  ]);

  // Islands — `type:"group"` nodes, the operator's hand-drawn continents.
  // Their labels are OPERATOR-TYPED strings, rendered verbatim: kb never
  // generates one (no in-daemon LLM, and no client-side LLM either).
  //
  // The wire carries no aligned coordinates for a group node, so an island
  // can only be drawn once the server's transform is recoverable; without
  // it the pass is SKIPPED and the field bar says why, rather than drawing
  // rectangles in a frame the ghost dots don't share. The rect is the
  // axis-aligned bounding box of the four transformed corners (a rotated
  // alignment turns a rectangle into a parallelogram; the bbox is the
  // honest, non-lying envelope of it).
  const fieldIslandRects = useMemo(() => {
    if (!fieldOn || !fieldTransform) return [];
    return fieldIslands(fieldDoc).map((isl) => {
      const corners = [
        applyFieldTransform(fieldTransform, isl.x, isl.y),
        applyFieldTransform(fieldTransform, isl.x + isl.w, isl.y),
        applyFieldTransform(fieldTransform, isl.x, isl.y + isl.h),
        applyFieldTransform(fieldTransform, isl.x + isl.w, isl.y + isl.h),
      ];
      const xs = corners.map((c) => toLogicalX(c.x));
      const ys = corners.map((c) => toLogicalY(c.y));
      return {
        nodeId: isl.nodeId,
        label: isl.label,
        x: Math.min(...xs),
        y: Math.min(...ys),
        w: Math.max(...xs) - Math.min(...xs),
        h: Math.max(...ys) - Math.min(...ys),
      };
    });
  }, [fieldOn, fieldTransform, fieldDoc]);

  // The onion skin compares the operator's field against TODAY's machine
  // layout, so it is hidden while a time-lapse frame is up — drawing it
  // over a past frame would silently compare two different maps.
  const fieldVisible = fieldOn && !framePlaced;

  // The selected dot's row in the disagreement list, for the inspector.
  // `rank` is 1-based over the SERVER's ordering (distance desc, id asc) —
  // "the 3rd biggest argument you have with the model", not a re-sort.
  const selectedFieldRow = useMemo(() => {
    if (!fieldOn || !selected?.source_relative) return null;
    const i = disagreements.findIndex((d) => d.id === selected.source_relative);
    if (i < 0) return null;
    return {
      distance: disagreements[i].distance,
      rank: i + 1,
      total: disagreements.length,
    };
  }, [fieldOn, selected, disagreements]);
  const islandCount = useMemo(
    () => (fieldOn ? fieldIslands(fieldDoc).length : 0),
    [fieldOn, fieldDoc],
  );

  const edgeLines = useMemo(() => {
    const seen = new Set<string>();
    const out: { a: Placed; b: Placed; aId: string; bId: string }[] = [];
    for (const { src, dst } of drawEdges) {
      if (src === dst) continue;
      const a = placedById.get(src);
      const b = placedById.get(dst);
      if (!a || !b) continue;
      const key = src < dst ? `${src}|${dst}` : `${dst}|${src}`;
      if (seen.has(key)) continue;
      seen.add(key);
      out.push({ a, b, aId: src, bId: dst });
    }
    return out;
  }, [drawEdges, placedById]);

  // W1.atlas — per-doc degree (backlinks + outlinks), counted straight off
  // `drawEdges` (the raw `edges` prop plus any MI-W4.5 supersede edges) —
  // NOT `edgeLines` above, which additionally drops directionless
  // duplicates for drawing; degree wants the raw per-endpoint count.
  const degreeById = useMemo(() => {
    const m = new Map<string, number>();
    for (const { src, dst } of drawEdges) {
      if (src === dst) continue;
      m.set(src, (m.get(src) ?? 0) + 1);
      m.set(dst, (m.get(dst) ?? 0) + 1);
    }
    return m;
  }, [drawEdges]);
  const maxDegreeSqrt = useMemo(() => {
    let max = 0;
    for (const d of degreeById.values()) if (d > max) max = d;
    return Math.sqrt(max);
  }, [degreeById]);

  // v0.11 A2/A3 — richer per-cluster summary. `clusters` carries the
  // centroid (mean x/y for region halos + the regions-strip pan target),
  // the count (for the strip's badge), the label + colour, and the
  // in-cluster topY for the label paint position.
  //
  // W1.B/W1.atlas — the label prefers the server's per-cluster c-TF-IDF top
  // terms (`serverLabelsByCluster`, deterministic + decomposable — see the
  // legend's term-breakdown popover) over the v0.11 dominant-tag heuristic;
  // a cluster with no server terms yet (endpoint not deployed, or this kb
  // hasn't recomputed since the label store shipped) falls back to the
  // original tag-frequency label verbatim. `labelSource` lets the legend
  // say which one it's showing.
  const clusters = useMemo(() => {
    if (!hasAtlasData) return [];
    const byCluster = new Map<number, Placed[]>();
    for (const p of placed) {
      const arr = byCluster.get(p.cluster);
      if (arr) arr.push(p);
      else byCluster.set(p.cluster, [p]);
    }
    const out: {
      cluster: number;
      // W3.T-c — the palette slot the members actually paint in (identical
      // to `cluster` off a time-lapse frame's remap); keeps the legend
      // swatch and the dots in agreement instead of the legend claiming a
      // colour the canvas isn't using.
      colorCluster: number;
      label: string;
      color: string;
      count: number;
      centroid: { x: number; y: number };
      topY: number;
      terms: AtlasLabelTerm[];
      labelSource: "keywords" | "tags";
    }[] = [];
    const used = new Set<string>();
    const clusterIds = [...byCluster.keys()].sort((a, b) => a - b);
    for (const cluster of clusterIds) {
      const pts = byCluster.get(cluster)!;
      if (pts.length < 2) continue;
      const cx = pts.reduce((s, p) => s + p.x, 0) / pts.length;
      const cy = pts.reduce((s, p) => s + p.y, 0) / pts.length;
      const topY = Math.min(...pts.map((p) => p.y));

      const serverTerms = serverLabelsByCluster.get(cluster);
      let label: string;
      let labelSource: "keywords" | "tags";
      if (serverTerms && serverTerms.length > 0) {
        label = serverTerms.slice(0, 2).map((t) => t.term).join(" · ");
        labelSource = "keywords";
      } else {
        const freq = new Map<string, number>();
        for (const p of pts) {
          for (const t of tagsFor(p.doc)) {
            freq.set(t, (freq.get(t) ?? 0) + 1);
          }
        }
        const ranked = [...freq.entries()].sort((a, b) => b[1] - a[1]);
        label =
          ranked.find(([t]) => !used.has(t))?.[0] ??
          ranked[0]?.[0] ??
          `cluster ${cluster}`;
        labelSource = "tags";
      }
      used.add(label);
      out.push({
        cluster,
        // Every member of one cluster shares a remap result, so the first
        // member's is the group's.
        colorCluster: pts[0].colorCluster,
        label,
        color: tagColor(label),
        count: pts.length,
        centroid: { x: cx, y: cy },
        topY,
        terms: serverTerms ?? [],
        labelSource,
      });
    }
    return out;
  }, [placed, hasAtlasData, serverLabelsByCluster]);

  // Back-compat — the draw loop reads labels from a flat array.
  const clusterLabels: ClusterLabel[] = useMemo(
    () =>
      clusters.map((c) => ({
        cluster: c.cluster,
        x: c.centroid.x,
        y: c.topY - 12,
        label: c.label,
        color: c.color,
      })),
    [clusters],
  );

  // W2.3b — semantic-zoom LOD, level "detail" (zoom > ZOOM_LOD_DETAIL):
  // there's screen space to spend, so each label expands from the
  // mid-zoom top-2 terms to top-3 + the cluster's artifact count.
  const detailClusterLabels: LodLabel[] = useMemo(
    () =>
      clusters.map((c) => {
        const topTerms = c.terms.slice(0, 3).map((t) => t.term);
        const label =
          topTerms.length > 0
            ? `${topTerms.join(" · ")} (${c.count})`
            : `${c.label} (${c.count})`;
        return { x: c.centroid.x, y: c.topY - 12, label, color: c.color };
      }),
    [clusters],
  );

  // W2.3b — cluster → member-id lookup for the legend's "open in gallery"
  // affordance (click-to-filter). Built off `placed` (not `clusters`,
  // which only carries the aggregate `count`) since the legend needs the
  // actual ids to build the `&ids=` deep-link.
  const clusterMemberIds = useMemo(() => {
    const m = new Map<number, string[]>();
    for (const p of placed) {
      const arr = m.get(p.cluster);
      if (arr) arr.push(p.doc.id);
      else m.set(p.cluster, [p.doc.id]);
    }
    return m;
  }, [placed]);

  const fallbackCount = placed.filter((p) => p.fallback).length;

  // W3.M-c — count of dots currently dimmed by the search overlay (every
  // dot NOT in `dimIds`), for the e2e hook below. `0` when no search is
  // active, matching the other `data-atlas-*-count` attrs' "0 = off" shape.
  const dimmedCount = dimIds
    ? placed.filter((p) => !dimIds.has(p.doc.id)).length
    : 0;

  // Wheel-to-zoom — same {passive: false} trick as the SVG version so
  // we can cancel page scroll.
  useEffect(() => {
    const node = canvasRef.current;
    if (!node) return;
    const handler = (e: globalThis.WheelEvent) => {
      e.preventDefault();
      // W3.F-c — a wheel means the operator is driving; drop any in-flight
      // tour animation rather than fighting it frame by frame.
      if (flyRef.current !== null) {
        cancelAnimationFrame(flyRef.current);
        flyRef.current = null;
      }
      const factor = e.deltaY < 0 ? 1.1 : 1 / 1.1;
      setZoom((z) => clamp(z * factor, ZOOM_MIN, ZOOM_MAX));
    };
    node.addEventListener("wheel", handler, { passive: false });
    return () => node.removeEventListener("wheel", handler);
  }, []);

  // Resolve a CSS-derived foreground color once per mount so the
  // canvas labels/edges respect the active theme.
  const themeColors = useMemo(() => {
    if (typeof window === "undefined") {
      return { fg: "#e8eaed", fgMuted: "#9aa0a6", bg: "#1f2024" };
    }
    const cs = getComputedStyle(document.documentElement);
    return {
      fg: cs.getPropertyValue("--fg").trim() || "#e8eaed",
      fgMuted: cs.getPropertyValue("--fg-muted").trim() || "#9aa0a6",
      bg: cs.getPropertyValue("--bg").trim() || "#1f2024",
    };
  }, []);

  // W2.3b — semantic-zoom LOD, level "continent" (zoom < ZOOM_LOD_CONTINENT):
  // merge the ≤12 cluster centroids into ≤MAX_CONTINENTS groups
  // (`mergeContinents`, pure — see atlasSelection.ts for the tie-break +
  // term-rerank rules). A continent blends multiple palette colors, so
  // painting it in any one cluster's color would claim a precision the
  // merge doesn't have — use the neutral foreground instead.
  const continentLabels: LodLabel[] = useMemo(() => {
    if (clusters.length === 0) return [];
    const forMerge: ClusterForMerge[] = clusters.map((c) => ({
      cluster: c.cluster,
      centroid: c.centroid,
      count: c.count,
      topY: c.topY,
      label: c.label,
      terms: c.terms.map((t) => ({ term: t.term, tf: t.tf })),
    }));
    return mergeContinents(forMerge, MAX_CONTINENTS).map((g) => ({
      x: g.centroid.x,
      y: g.topY - 12,
      label: g.label,
      color: themeColors.fgMuted,
    }));
  }, [clusters, themeColors]);

  // Main draw loop — runs whenever the data or transform changes.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const dpr = window.devicePixelRatio || 1;
    const cssW = canvas.clientWidth;
    const cssH = canvas.clientHeight;
    if (cssW === 0 || cssH === 0) return;
    if (canvas.width !== cssW * dpr || canvas.height !== cssH * dpr) {
      canvas.width = cssW * dpr;
      canvas.height = cssH * dpr;
    }

    // M-b — uniform-scale, letterboxed fit (device-pixel space): the logical
    // W × H box is "contain"-fit into the canvas, so a dot's `ctx.arc` stays
    // a circle in ANY container aspect (was independently scaled per axis —
    // see atlasFit.ts's header comment for the ellipse/hit-test defect this
    // replaces).
    const fit = fitTransform(cssW, cssH, dpr, W, H);

    ctx.save();
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    // Build the world transform: device-pixel scale + letterbox offset,
    // then pan, then zoom. Order matches the SVG version's <g
    // transform="translate(pan) scale(zoom)">.
    ctx.setTransform(
      fit.scale,
      0,
      0,
      fit.scale,
      fit.offsetX + pan.x * fit.scale,
      fit.offsetY + pan.y * fit.scale,
    );
    ctx.scale(zoom, zoom);

    // v0.11 A3 — faint cell grid (every 140 × 120 logical units).
    // Drawn first so everything else paints over it. The thin stroke
    // gives a cosine-similarity scale anchor without competing visually.
    ctx.save();
    ctx.strokeStyle = themeColors.fgMuted;
    ctx.globalAlpha = 0.08;
    ctx.lineWidth = 1 / Math.max(1, zoom);
    for (let gx = 0; gx <= W; gx += 140) {
      ctx.beginPath();
      ctx.moveTo(gx, 0);
      ctx.lineTo(gx, H);
      ctx.stroke();
    }
    for (let gy = 0; gy <= H; gy += 120) {
      ctx.beginPath();
      ctx.moveTo(0, gy);
      ctx.lineTo(W, gy);
      ctx.stroke();
    }
    ctx.restore();

    // v0.11 A3 — region halos. 120-radius circle at each cluster
    // centroid, filled with the cluster's color at 4% alpha. Reads as
    // a soft tint behind the dots without obscuring edges.
    for (const c of clusters) {
      ctx.save();
      ctx.fillStyle = c.color;
      ctx.globalAlpha = 0.04;
      ctx.beginPath();
      ctx.arc(c.centroid.x, c.centroid.y, 120, 0, Math.PI * 2);
      ctx.fill();
      ctx.restore();
    }

    // Density heatmap (back). `ctx.filter = "blur(...)"` is supported
    // in every browser we care about; one pass per dot is fine at 50k
    // because the blur kernel is fixed.
    if (showDensity && placed.length > 0) {
      ctx.save();
      ctx.globalAlpha = 0.55;
      ctx.filter = "blur(14px)";
      ctx.fillStyle = "rgba(122, 164, 247, 1)";
      for (const p of placed) {
        ctx.beginPath();
        ctx.arc(p.x, p.y, 10, 0, Math.PI * 2);
        ctx.fill();
      }
      ctx.restore();
    }

    // Edges (quadratic bezier with a small perpendicular bow).
    const lineScale = 1 / Math.sqrt(zoom);
    for (const { a, b, aId, bId } of edgeLines) {
      const hi = hover != null && (hover.doc.id === aId || hover.doc.id === bId);
      const mx = (a.x + b.x) / 2 + (b.y - a.y) * 0.12;
      const my = (a.y + b.y) / 2 - (b.x - a.x) * 0.12;
      ctx.strokeStyle = hi ? "#5b8def" : themeColors.fgMuted;
      ctx.globalAlpha = hi ? 0.85 : 0.22;
      ctx.lineWidth = (hi ? 1.4 : 0.6) * lineScale;
      ctx.beginPath();
      ctx.moveTo(a.x, a.y);
      ctx.quadraticCurveTo(mx, my, b.x, b.y);
      ctx.stroke();
    }
    ctx.globalAlpha = 1;

    // W3.F-c — THE ONION SKIN. One pass, between the edges and the dots:
    // the operator's own field ghosted over the machine's, with a leader
    // line from each hand-placed position to where the machine put that
    // artifact. The line IS the disagreement — long line, big argument.
    //
    // Everything here is drawn from server-aligned coordinates (or, for an
    // unsaved drag, the server's own transform recovered from its output);
    // nothing in this pass re-fits anything, and nothing in it writes.
    if (fieldVisible && (fieldGhosts.length > 0 || fieldIslandRects.length > 0)) {
      const fieldAccent =
        getComputedStyle(document.documentElement)
          .getPropertyValue("--accent")
          .trim() || "#8a7fff";
      ctx.save();
      // Islands first, behind the ghosts: a dashed envelope + the
      // operator's own label, verbatim.
      ctx.setLineDash([5 * lineScale, 4 * lineScale]);
      ctx.lineWidth = 1 * lineScale;
      for (const isl of fieldIslandRects) {
        ctx.strokeStyle = fieldAccent;
        ctx.globalAlpha = 0.4;
        ctx.strokeRect(isl.x, isl.y, isl.w, isl.h);
        if (!isl.label) continue;
        // The label's halo is a SOLID stroke — the dash belongs to the
        // island's envelope, not to its lettering.
        ctx.setLineDash([]);
        ctx.globalAlpha = 0.9;
        ctx.font = `600 ${10 * lineScale}px system-ui, sans-serif`;
        ctx.textAlign = "left";
        ctx.textBaseline = "alphabetic";
        ctx.lineWidth = 3 * lineScale;
        ctx.strokeStyle = themeColors.bg;
        ctx.lineJoin = "round";
        ctx.strokeText(isl.label, isl.x + 3, isl.y - 3);
        ctx.fillStyle = fieldAccent;
        ctx.fillText(isl.label, isl.x + 3, isl.y - 3);
        ctx.lineWidth = 1 * lineScale;
        ctx.setLineDash([5 * lineScale, 4 * lineScale]);
      }
      // Leader lines + ghost dots.
      ctx.setLineDash([]);
      for (const g of fieldGhosts) {
        if (fieldDrag && g.file === fieldDrag.file) continue; // drawn below
        drawGhost(ctx, g.x, g.y, g.mx, g.my, fieldAccent, lineScale, g.pending);
      }
      // The dot under the pointer in place mode, at the cursor.
      if (fieldDrag) {
        const target = placedBySourceRel.get(fieldDrag.file);
        drawGhost(
          ctx,
          fieldDrag.x,
          fieldDrag.y,
          target?.x ?? fieldDrag.x,
          target?.y ?? fieldDrag.y,
          fieldAccent,
          lineScale,
          true,
        );
      }
      ctx.restore();
      ctx.globalAlpha = 1;
    }

    // Cluster labels — stroke-then-fill mimics SVG's paint-order: stroke
    // (the halo around text so it stays legible over edges/dots).
    //
    // W2.3b — semantic-zoom label LOD: below ZOOM_LOD_CONTINENT the ≤12
    // per-cluster labels collapse into ≤MAX_CONTINENTS continent labels;
    // between the thresholds this is byte-identical to the pre-W2.3b
    // behaviour; above ZOOM_LOD_DETAIL each label expands to more terms +
    // a count. Cameras only ever persist `zoom`, so recalling a saved
    // camera re-derives the right LOD for free — no camera-schema change.
    const fontScale = 1 / Math.sqrt(zoom);
    const activeLabels: LodLabel[] =
      zoom < ZOOM_LOD_CONTINENT
        ? continentLabels
        : zoom > ZOOM_LOD_DETAIL
          ? detailClusterLabels
          : clusterLabels;
    for (const c of activeLabels) {
      ctx.font = `600 ${10 * fontScale}px system-ui, sans-serif`;
      ctx.textAlign = "center";
      ctx.textBaseline = "alphabetic";
      ctx.lineWidth = 3 * fontScale;
      ctx.strokeStyle = themeColors.bg;
      ctx.lineJoin = "round";
      ctx.strokeText(c.label, c.x, c.y);
      ctx.fillStyle = c.color;
      ctx.fillText(c.label, c.x, c.y);
    }

    // v0.14 S7 — session polylines. Beneath the dots so dots stay
    // interactive. One line per session, color hashed from session_id
    // (deterministic across reloads + views). Skips polylines whose
    // touched artifacts are all missing from the visible point set —
    // a sub-2-point line is just a dot rendered twice.
    if (showSessions && sessionPolylines.length > 0) {
      ctx.save();
      ctx.lineWidth = 1.5 * lineScale;
      ctx.lineJoin = "round";
      ctx.lineCap = "round";
      for (const poly of sessionPolylines) {
        const pts = poly.ids
          .map((id) => {
            const p = placedById.get(id);
            return p ? { id, x: p.x, y: p.y } : null;
          })
          .filter((p): p is { id: string; x: number; y: number } => !!p);
        if (pts.length < 2) continue;
        // v0.14 T4 — dim non-focused polylines so the chosen path
        // reads on a dense corpus; widen the focused line to make it
        // unambiguously dominant.
        const isFocused = focusedSession === poly.id;
        const hasFocus = focusedSession !== null;
        ctx.globalAlpha = hasFocus ? (isFocused ? 0.85 : 0.08) : 0.5;
        ctx.lineWidth = (isFocused ? 2.2 : 1.5) * lineScale;
        ctx.strokeStyle = poly.color;
        // CT-A6 — stroke hop-by-hop (instead of one path) so the hop
        // landing on a fuzzy-tier (path-substring) touch can dash
        // distinctly from an exact-tier (literal id) hop; the fuzzy
        // signal is honest, not unreliable, so this is a style cue, not
        // a dimming.
        for (let i = 1; i < pts.length; i += 1) {
          ctx.setLineDash(
            poly.fuzzyIds.has(pts[i].id) ? [4 * lineScale, 3 * lineScale] : [],
          );
          ctx.beginPath();
          ctx.moveTo(pts[i - 1].x, pts[i - 1].y);
          ctx.lineTo(pts[i].x, pts[i].y);
          ctx.stroke();
        }
      }
      ctx.restore();
    }

    // Dots — fixed pixel radius regardless of zoom, like the SVG `r =
    // 6/sqrt(zoom)`.
    const dotR = 6 * lineScale;
    // W1.atlas — degree sizing. `radiusFor` scales the base radius by a
    // modest ~1×–2.5× band on sqrt(degree), so one heavily-linked hub
    // doesn't dwarf the layout; a doc with no edges (or the toggle off)
    // stays at the base radius. Shared by the dots loop and the selected
    // ring below so the ring hugs whatever size the dot actually painted.
    const radiusFor = (id: string): number => {
      if (!sizeByLinks || maxDegreeSqrt <= 0) return dotR;
      const deg = degreeById.get(id) ?? 0;
      if (deg <= 0) return dotR;
      return dotR * (1 + (Math.sqrt(deg) / maxDegreeSqrt) * 1.5);
    };
    const hasClusterHighlight = highlightCluster !== null;
    // W2.3b — a non-empty lasso'd working set dims everything outside it,
    // the same alpha-multiplier shape as the cluster highlight / dim-read
    // signals above (all three compose by multiplication).
    const hasSelection = selection.size > 0;
    for (const p of placed) {
      // W1.atlas — two independent dim signals, multiplied: the legend's
      // cluster highlight (dims every OTHER cluster) and "dim read"
      // (dims artifacts already finished, per the reading-progress map —
      // see the `progress` comment above for its 200-visit honesty cap).
      let alpha = 1;
      if (hasClusterHighlight && p.cluster !== highlightCluster) alpha *= 0.15;
      if (dimRead && progress.get(p.doc.id)?.isDone) alpha *= 0.35;
      if (hasSelection && !selection.has(p.doc.id)) alpha *= 0.15;
      // W3.M-c — ONE MORE multiplier: the search-dim overlay's matched-id
      // set. A dot outside the hit set dims; nothing is ever removed from
      // `placed` on its account (dim in place, not a filter).
      if (dimIds && !dimIds.has(p.doc.id)) alpha *= 0.15;
      // W3.F-c — DISAGREEMENT HEAT. The dot's colour becomes "how far is
      // this from where I put it", straight off the server's aligned
      // distances (`heatT`/`heatColor` only pick a ramp position; the
      // magnitudes are the daemon's and are never recomputed here). An
      // artifact the operator never placed has no disagreement to show —
      // it goes neutral-grey and steps back, rather than borrowing a warm
      // colour it hasn't earned.
      // MI-W4.5 — the memory color modes slot in here, same priority band
      // as `clusterColor` below (a fallback dot is always neutral grey
      // regardless of mode; the disagreement heat override below still
      // wins over ANY color mode when "my field" is on).
      let fill: string;
      if (p.fallback) {
        fill = "#999";
      } else if (colorMode === "salience") {
        fill = salienceFillColor(memoryById.get(p.doc.id)?.salience);
      } else if (colorMode === "decay") {
        fill = decayBucketFillColor(memoryById.get(p.doc.id)?.decay_bucket);
      } else {
        fill = clusterColor(p.colorCluster);
      }
      if (heatOn && fieldVisible) {
        const dist = p.doc.source_relative
          ? disagreementBySourceRel.get(p.doc.source_relative)
          : undefined;
        if (dist == null) {
          fill = themeColors.fgMuted;
          alpha *= 0.45;
        } else {
          fill = heatColor(heatT(dist, maxDisagreement));
        }
      }
      ctx.globalAlpha = alpha;
      // W3.T-c — paint through `colorCluster` (== `cluster` on the live map;
      // the time-lapse remap on a frame) so a continent keeps ONE colour
      // across the whole playback instead of strobing on k-means renumbering.
      ctx.fillStyle = fill;
      ctx.beginPath();
      ctx.arc(p.x, p.y, radiusFor(p.doc.id), 0, Math.PI * 2);
      ctx.fill();
    }
    ctx.globalAlpha = 1;

    // W2.3b — working-set rings: a single thin accent ring per selected
    // dot, distinct from the double-ring single-node `selected` accent
    // below so a lasso'd set and an inspected dot (e.g. from clicking a
    // true-neighbor row) read as two different things when they overlap.
    if (hasSelection) {
      ctx.save();
      const selAccent =
        getComputedStyle(document.documentElement).getPropertyValue("--accent").trim() ||
        "#8a7fff";
      ctx.strokeStyle = selAccent;
      ctx.lineWidth = 1.2 * lineScale;
      ctx.globalAlpha = 0.85;
      for (const p of placed) {
        if (!selection.has(p.doc.id)) continue;
        ctx.beginPath();
        ctx.arc(p.x, p.y, radiusFor(p.doc.id) + 3, 0, Math.PI * 2);
        ctx.stroke();
      }
      ctx.restore();
    }

    // v0.11 A3 — selected-node concentric rings (accent purple). Two
    // rings at increasing radius / decreasing opacity. Drawn after the
    // dots so the rings sit on top.
    if (selected) {
      const sp = placed.find((p) => p.doc.id === selected.id);
      if (sp) {
        const selR = radiusFor(sp.doc.id);
        const accent =
          getComputedStyle(document.documentElement).getPropertyValue("--accent").trim() ||
          "#8a7fff";
        ctx.save();
        ctx.strokeStyle = accent;
        ctx.lineWidth = 1.2 * lineScale;
        ctx.globalAlpha = 0.7;
        ctx.beginPath();
        ctx.arc(sp.x, sp.y, selR + 6, 0, Math.PI * 2);
        ctx.stroke();
        ctx.lineWidth = 0.7 * lineScale;
        ctx.globalAlpha = 0.4;
        ctx.beginPath();
        ctx.arc(sp.x, sp.y, selR + 12, 0, Math.PI * 2);
        ctx.stroke();
        ctx.restore();
      }
    }

    // Hover title — drawn last so it always sits on top.
    if (hover) {
      ctx.font = `${11 * fontScale}px system-ui, sans-serif`;
      ctx.textAlign = "center";
      ctx.lineWidth = 3 * fontScale;
      ctx.strokeStyle = themeColors.bg;
      ctx.lineJoin = "round";
      const label = truncate(hover.doc.title || hover.doc.id, 36);
      const ly = hover.y - 11 * lineScale;
      ctx.strokeText(label, hover.x, ly);
      ctx.fillStyle = themeColors.fgMuted;
      ctx.fillText(label, hover.x, ly);
    }

    // W2.3b — the in-progress lasso path (freehand polygon), drawn LAST so
    // it's always visible while dragging: a soft fill (so the enclosed
    // area reads at a glance) plus a dashed accent stroke. Nothing is
    // drawn once the gesture ends — `endDrag` clears `lassoPath` and the
    // selection rings above become the persistent artifact of a completed
    // lasso, not the path itself.
    if (lassoPath && lassoPath.length > 1) {
      ctx.save();
      const lassoAccent =
        getComputedStyle(document.documentElement).getPropertyValue("--accent").trim() ||
        "#8a7fff";
      ctx.beginPath();
      ctx.moveTo(lassoPath[0].x, lassoPath[0].y);
      for (let i = 1; i < lassoPath.length; i++) {
        ctx.lineTo(lassoPath[i].x, lassoPath[i].y);
      }
      ctx.closePath();
      ctx.fillStyle = lassoAccent;
      ctx.globalAlpha = 0.1;
      ctx.fill();
      ctx.globalAlpha = 0.85;
      ctx.strokeStyle = lassoAccent;
      ctx.lineWidth = 1.4 * lineScale;
      ctx.setLineDash([4 * lineScale, 3 * lineScale]);
      ctx.stroke();
      ctx.restore();
    }

    ctx.restore();
  }, [
    placed,
    edgeLines,
    clusterLabels,
    continentLabels,
    detailClusterLabels,
    clusters,
    selected,
    selection,
    lassoPath,
    hover,
    showSessions,
    sessionPolylines,
    focusedSession,
    placedById,
    zoom,
    pan,
    showDensity,
    themeColors,
    highlightCluster,
    dimRead,
    progress,
    dimIds,
    sizeByLinks,
    degreeById,
    maxDegreeSqrt,
    // W3.F-c — the onion skin + heat pass reads these.
    fieldVisible,
    fieldGhosts,
    fieldIslandRects,
    fieldDrag,
    placedBySourceRel,
    heatOn,
    disagreementBySourceRel,
    maxDisagreement,
  ]);

  // Resize handler — invalidates the canvas backing store so it stays
  // crisp under DPR changes / layout reflow.
  useEffect(() => {
    const node = canvasRef.current;
    if (!node) return;
    const ro = new ResizeObserver(() => {
      // Force a redraw by nudging zoom in place (cheap; state ref ===
      // is unchanged).
      setZoom((z) => z);
    });
    ro.observe(node);
    return () => ro.disconnect();
  }, []);

  // Map a CSS-pixel mouse coord to a logical (W × H) coord, undoing the
  // CSS-space fit (M-b: uniform scale + letterbox — dpr=1 since
  // `getBoundingClientRect` is already CSS pixels and dpr cancels) then pan
  // + zoom.
  const screenToLogical = (
    clientX: number,
    clientY: number,
  ): { x: number; y: number } | null => {
    const canvas = canvasRef.current;
    if (!canvas) return null;
    const rect = canvas.getBoundingClientRect();
    const xCss = clientX - rect.left;
    const yCss = clientY - rect.top;
    const fit = fitTransform(rect.width, rect.height, 1, W, H);
    return fitScreenToLogical({ x: xCss, y: yCss }, fit, pan, zoom);
  };

  const findHit = (clientX: number, clientY: number): Placed | null => {
    const lp = screenToLogical(clientX, clientY);
    if (!lp) return null;
    // Hit radius scales inversely with zoom so dots stay clickable at
    // their on-screen size. Add 2px margin so a tight UMAP clump still
    // has clickable lanes between dots.
    const hitR = (8 / Math.sqrt(zoom));
    let best: Placed | null = null;
    let bestDist = Infinity;
    for (const p of placed) {
      const dx = p.x - lp.x;
      const dy = p.y - lp.y;
      const d2 = dx * dx + dy * dy;
      if (d2 <= hitR * hitR && d2 < bestDist) {
        bestDist = d2;
        best = p;
      }
    }
    return best;
  };

  // Carries a drag's `moved` flag across the mouseup→click ordering gap
  // (see endDrag). Reset on every mousedown so a drag whose click never
  // fired (released off-canvas) can't suppress the NEXT legitimate click.
  const suppressClick = useRef(false);
  const onMouseDown = (e: MouseEvent<HTMLCanvasElement>) => {
    // W3.F-c — a pointer on the canvas always wins over a running tour
    // flight; nothing is more annoying than a camera that fights the hand.
    cancelFly();
    // W3.F-c — PLACE MODE takes over the same way the lasso does, and for
    // the same reason (a mode that half-pans is a mode that surprises).
    // Grab the dot under the cursor and carry it; the MACHINE coordinate
    // never moves — what follows the pointer is the operator's ghost.
    if (placeMode) {
      const hit = findHit(e.clientX, e.clientY);
      const lp = screenToLogical(e.clientX, e.clientY);
      if (hit?.doc.source_relative && lp) {
        fieldDragRef.current = { file: hit.doc.source_relative };
        setFieldDrag({ file: hit.doc.source_relative, x: lp.x, y: lp.y });
      }
      return;
    }
    // W2.3b — lasso mode takes over the mouse-down/move/up sequence
    // entirely (no pan, no hover-select); it's mouse-only by design — the
    // separate MB4 touch handlers below early-return for non-mouse
    // pointer types and preventDefault their own compat mouse events, so
    // touch input never reaches here regardless of `lassoMode`.
    if (lassoMode) {
      lassoDrawing.current = true;
      const lp = screenToLogical(e.clientX, e.clientY);
      setLassoPath(lp ? [lp] : []);
      return;
    }
    suppressClick.current = false;
    dragState.current = {
      mx: e.clientX,
      my: e.clientY,
      px: pan.x,
      py: pan.y,
      moved: false,
    };
  };
  const onMouseMove = (e: MouseEvent<HTMLCanvasElement>) => {
    if (placeMode) {
      if (fieldDragRef.current) {
        const lp = screenToLogical(e.clientX, e.clientY);
        if (lp) {
          const file = fieldDragRef.current.file;
          setFieldDrag({ file, x: lp.x, y: lp.y });
        }
        return;
      }
      // Not dragging yet — keep hover live so it's obvious what a
      // mouse-down would pick up.
      const hit = findHit(e.clientX, e.clientY);
      setHover((prev) => (prev === hit ? prev : hit));
      return;
    }
    if (lassoMode) {
      if (!lassoDrawing.current) return;
      const lp = screenToLogical(e.clientX, e.clientY);
      if (lp) setLassoPath((prev) => (prev ? [...prev, lp] : [lp]));
      return;
    }
    if (dragState.current) {
      const dx = e.clientX - dragState.current.mx;
      const dy = e.clientY - dragState.current.my;
      if (Math.abs(dx) > 3 || Math.abs(dy) > 3) dragState.current.moved = true;
      setPan({
        x: clamp(dragState.current.px + dx, -PAN_MAX_X, PAN_MAX_X),
        y: clamp(dragState.current.py + dy, -PAN_MAX_Y, PAN_MAX_Y),
      });
      return;
    }
    const hit = findHit(e.clientX, e.clientY);
    setHover((prev) => (prev === hit ? prev : hit));
  };
  const endDrag = () => {
    if (placeMode) {
      // Commit: logical drop point → unit space → the JSON Canvas sidecar,
      // through `setPlacement` (which moves the existing `file` node or
      // appends one, preserving everything else in the document). ONE
      // debounced PUT follows. Nothing here touches `placed`, `docs`, or
      // any atlas coordinate — the machine layout is read-only.
      const drag = fieldDragRef.current;
      fieldDragRef.current = null;
      const pos = fieldDrag;
      setFieldDrag(null);
      if (drag && pos && pos.file === drag.file) {
        scheduleFieldSave(
          setPlacement(
            fieldDoc,
            drag.file,
            unitFromLogicalX(pos.x),
            unitFromLogicalY(pos.y),
          ),
        );
        censusBump("atlas.field.place");
      }
      return;
    }
    if (lassoMode) {
      // A lasso needs ≥3 points to enclose anything (matches
      // `selectionFromLasso`'s own degenerate-polygon guard) — a
      // click-without-drag in lasso mode is a no-op, not a
      // clear-the-selection gesture (the "clear" button in the working-set
      // bar owns that).
      if (lassoDrawing.current && lassoPath && lassoPath.length >= 3) {
        const ids = selectionFromLasso(
          placed.map((p) => ({ id: p.doc.id, x: p.x, y: p.y })),
          lassoPath,
        );
        setSelection(ids);
        if (ids.size > 0) censusBump("atlas.lasso");
      }
      lassoDrawing.current = false;
      setLassoPath(null);
      return;
    }
    // The browser's `click` fires AFTER mouseup, and nulling dragState
    // here blinded openHit's moved-guard — the click ending a
    // drag-to-pan landed as a real click on (usually empty) canvas and
    // cleared the inspector selection. Carry the moved flag across the
    // mouseup→click gap instead.
    suppressClick.current = dragState.current?.moved ?? false;
    dragState.current = null;
  };
  const openHit = (e: MouseEvent<HTMLCanvasElement>, newTab: boolean) => {
    // Ignore the click that ends a drag; only treat short interactions
    // as a real click on a dot. (`suppressClick` survives the
    // mouseup→click ordering; the live dragState check covers the
    // touch path, which routes around `click` entirely.)
    if (dragState.current?.moved || suppressClick.current) {
      suppressClick.current = false;
      return;
    }
    const hit = findHit(e.clientX, e.clientY);
    if (!hit) {
      // Click on empty canvas clears the inspector selection — gives
      // the user an obvious way to dismiss the rail without a button
      // hunt. cmd/ctrl-clicking empty canvas does nothing (no tab to
      // open).
      if (!newTab) setSelected(null);
      return;
    }
    const href = artifactHref(kb, hit.doc.source_relative);
    // Canvas can't be an anchor — replicate native ctrl/⌘/middle-click by
    // opening a new tab. AtlasView runs in the SPA top context (not the
    // sandboxed iframe), so window.open in a click handler keeps user
    // activation and isn't popup-blocked.
    if (newTab) {
      window.open(href, "_blank", "noopener");
      return;
    }
    // v0.11 A1 — single click on a dot selects it for the inspector
    // (not navigates). The inspector's preview button is the design's
    // canonical navigate path; users who want today's "click to open"
    // behaviour cmd/⌘+click instead (which already opened in a new
    // tab and still does). Match the design's interaction model.
    setSelected(hit.doc);
  };
  const onClick = (e: MouseEvent<HTMLCanvasElement>) => {
    if (lassoMode) return; // the mouseup above already handled the gesture
    // W3.F-c — in place mode a click IS the end of a placement gesture; it
    // must not also re-select/clear the inspector.
    if (placeMode) return;
    openHit(e, e.metaKey || e.ctrlKey);
  };
  const onAuxClick = (e: MouseEvent<HTMLCanvasElement>) => {
    if (lassoMode || e.button !== 1) return; // middle-click only
    e.preventDefault();
    openHit(e, true);
  };

  // MB4 — touch gestures (mouse falls through to the handlers above). One
  // finger pans (reuses `dragState` so the moved-guard works); two fingers
  // pinch-zoom about the gesture midpoint; a tap that didn't move selects /
  // clears the inspector. preventDefault on touch suppresses the synthetic
  // mouse+click compat events so nothing double-fires.
  const onPointerDown = (e: PointerEvent<HTMLCanvasElement>) => {
    if (e.pointerType === "mouse") return;
    cancelFly(); // a finger on the map outranks a running tour flight
    e.preventDefault();
    canvasRef.current?.setPointerCapture(e.pointerId);
    pointers.current.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (pointers.current.size === 1) {
      dragState.current = {
        mx: e.clientX,
        my: e.clientY,
        px: pan.x,
        py: pan.y,
        moved: false,
      };
      pinchRef.current = null;
    } else if (pointers.current.size === 2) {
      const [a, b] = [...pointers.current.values()];
      const midLogical = screenToLogical(
        (a.x + b.x) / 2,
        (a.y + b.y) / 2,
      ) ?? { x: W / 2, y: H / 2 };
      pinchRef.current = { dist: Math.hypot(a.x - b.x, a.y - b.y), zoom, midLogical };
      dragState.current = null; // a pinch is not a pan
    }
  };
  const onPointerMove = (e: PointerEvent<HTMLCanvasElement>) => {
    if (e.pointerType === "mouse") return;
    if (!pointers.current.has(e.pointerId)) return;
    pointers.current.set(e.pointerId, { x: e.clientX, y: e.clientY });
    const canvas = canvasRef.current;
    if (pointers.current.size >= 2 && pinchRef.current && canvas) {
      e.preventDefault();
      const [a, b] = [...pointers.current.values()];
      const start = pinchRef.current;
      const dist = Math.hypot(a.x - b.x, a.y - b.y);
      const nextZoom = clamp(
        (start.zoom * dist) / (start.dist || 1),
        ZOOM_MIN,
        ZOOM_MAX,
      );
      const rect = canvas.getBoundingClientRect();
      // M-b — bare fit (pan 0, zoom 1) converts the CSS-pixel midpoint into
      // the same "zoom*logical + pan" space `screenToLogical`'s inverse
      // uses, so solving for the new pan below stays correct under the
      // uniform+letterboxed fit (was a non-uniform `W/rect.width` /
      // `H/rect.height` split with no offset term).
      const fit = fitTransform(rect.width, rect.height, 1, W, H);
      const midCss = fitScreenToLogical(
        { x: (a.x + b.x) / 2 - rect.left, y: (a.y + b.y) / 2 - rect.top },
        fit,
        { x: 0, y: 0 },
        1,
      ) ?? { x: W / 2, y: H / 2 };
      setZoom(nextZoom);
      setPan({
        x: clamp(midCss.x - start.midLogical.x * nextZoom, -PAN_MAX_X, PAN_MAX_X),
        y: clamp(midCss.y - start.midLogical.y * nextZoom, -PAN_MAX_Y, PAN_MAX_Y),
      });
      return;
    }
    if (dragState.current) {
      e.preventDefault();
      const dx = e.clientX - dragState.current.mx;
      const dy = e.clientY - dragState.current.my;
      if (Math.abs(dx) > 3 || Math.abs(dy) > 3) dragState.current.moved = true;
      setPan({
        x: clamp(dragState.current.px + dx, -PAN_MAX_X, PAN_MAX_X),
        y: clamp(dragState.current.py + dy, -PAN_MAX_Y, PAN_MAX_Y),
      });
    }
  };
  const endPointer = (e: PointerEvent<HTMLCanvasElement>) => {
    if (e.pointerType === "mouse") return;
    const wasDragging = dragState.current;
    const hadPinch = pinchRef.current != null;
    pointers.current.delete(e.pointerId);
    try {
      canvasRef.current?.releasePointerCapture(e.pointerId);
    } catch {
      /* pointer wasn't captured — fine */
    }
    if (pointers.current.size < 2) pinchRef.current = null;
    if (pointers.current.size === 0) {
      // A tap (no pinch, no pan movement) selects the dot under it, or
      // clears the selection on empty canvas — mirrors openHit's non-newTab
      // path.
      if (!hadPinch && wasDragging && !wasDragging.moved) {
        const hit = findHit(e.clientX, e.clientY);
        setSelected(hit ? hit.doc : null);
      }
      dragState.current = null;
    }
  };

  const handleRecompute = () => runAtlasOp("recompute");
  const handleRecluster = () => runAtlasOp("recluster");

  // v0.11 A2 — pan + zoom to a cluster's centroid. Drives the regions
  // strip's click. Centres the centroid in the viewport and bumps
  // zoom to 1.6× so the cluster fills a useful fraction of the
  // canvas without making individual dots unrecognisably large.
  const panToCluster = (cx: number, cy: number) => {
    const z = 1.6;
    // After the scale, the world point (cx, cy) needs to land at
    // (W/2, H/2) in logical coords. The render math is
    // ctx.scale(zoom) applied to (cx + pan.x). So pan.x = W/(2z) - cx.
    const px = W / (2 * z) - cx;
    const py = H / (2 * z) - cy;
    setZoom(z);
    setPan({ x: clamp(px, -PAN_MAX_X, PAN_MAX_X), y: clamp(py, -PAN_MAX_Y, PAN_MAX_Y) });
  };

  // W3.F-c — the loci tour's camera. Same target math as `panToCluster`
  // (centre this logical point at this zoom), but eased over
  // TOUR_FLIGHT_MS so a walk from stop to stop reads as MOVEMENT THROUGH A
  // PLACE — that spatial continuity is the entire mnemonic value of a
  // memory-palace walk; a hard cut teleports and the loci don't stick.
  //
  // Three rules: `prefers-reduced-motion: reduce` JUMPS instead (no
  // animation at all — the walk is fully usable, just instant); any
  // pointer on the canvas cancels the flight (the hand always wins); and
  // the rAF handle is cancelled on unmount, so nothing outlives the view.
  const flyRef = useRef<number | null>(null);
  const cancelFly = () => {
    if (flyRef.current !== null) {
      cancelAnimationFrame(flyRef.current);
      flyRef.current = null;
    }
  };
  useEffect(() => cancelFly, []);
  const flyTo = (tx: number, ty: number, tz: number = TOUR_ZOOM) => {
    cancelFly();
    const targetPan = {
      x: clamp(W / (2 * tz) - tx, -PAN_MAX_X, PAN_MAX_X),
      y: clamp(H / (2 * tz) - ty, -PAN_MAX_Y, PAN_MAX_Y),
    };
    if (reducedMotion) {
      setZoom(tz);
      setPan(targetPan);
      return;
    }
    const fromPan = pan;
    const fromZoom = zoom;
    const t0 = performance.now();
    const step = (now: number) => {
      const k = Math.min(1, (now - t0) / TOUR_FLIGHT_MS);
      // ease-in-out cubic — no dependency, no spring, no config knob.
      const e = k < 0.5 ? 4 * k * k * k : 1 - Math.pow(-2 * k + 2, 3) / 2;
      setZoom(fromZoom + (tz - fromZoom) * e);
      setPan({
        x: fromPan.x + (targetPan.x - fromPan.x) * e,
        y: fromPan.y + (targetPan.y - fromPan.y) * e,
      });
      flyRef.current = k < 1 ? requestAnimationFrame(step) : null;
    };
    flyRef.current = requestAnimationFrame(step);
  };

  // W2.3b — the true-neighbors rail's "click a neighbor row" action:
  // select that dot for the inspector AND pan/zoom to center it, reusing
  // `panToCluster`'s math (it only needs an (x, y), not actually a
  // cluster centroid).
  const selectAndCenter = (id: string) => {
    const p = placedById.get(id);
    if (!p) return;
    setSelected(p.doc);
    panToCluster(p.x, p.y);
  };

  const runAtlasOp = async (op: "recompute" | "recluster") => {
    if (pending) return;
    setError(null);
    setPending(true);
    let run: string;
    try {
      const resp =
        op === "recompute" ? await recomputeAtlas(kb) : await reclusterAtlas(kb);
      run = resp.run;
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
      setPending(false);
      return;
    }

    let unsub: (() => void) | null = null;
    let timer: ReturnType<typeof setTimeout> | null = null;

    const finish = (errMsg: string | null) => {
      if (unsub) {
        unsub();
        unsub = null;
      }
      if (timer) {
        clearTimeout(timer);
        timer = null;
      }
      cleanupRef.current = null;
      setPending(false);
      if (errMsg) setError(errMsg);
      else if (onRecomputeDone) onRecomputeDone();
    };

    // Q5: recluster's completion event name differs from recompute's.
    // Coords stay unchanged on recluster — only `atlas_cluster` flips —
    // but the SPA still has to refetch docs to pick up the new labels;
    // the same onRecomputeDone callback covers both (it's a re-fetch,
    // not a "re-layout" specifically).
    const eventName = `atlas.${op}.complete`;
    unsub = sse.subscribeEvent(eventName, (payload) => {
      if (payload.run !== run) return;
      if (payload.kb && payload.kb !== kb) return;
      const errField = typeof payload.error === "string" ? payload.error : null;
      finish(errField ? `${op} failed: ${errField}` : null);
    });

    timer = setTimeout(() => {
      finish(`${op} timed out — refresh to check`);
    }, RECOMPUTE_TIMEOUT_MS);

    cleanupRef.current = () => {
      if (unsub) unsub();
      if (timer) clearTimeout(timer);
    };
  };

  // D6 — this used to read `!hasAtlasData` alone, so it claimed "no atlas
  // data yet" any time the corpus had never been through a real UMAP
  // recompute — even though the canvas ALWAYS draws a dot per doc (via
  // `hashPoint` fallback placement when real coords are absent), so a
  // scatter was visibly on screen while the banner insisted there was
  // nothing to show. Gate the true "nothing rendered" copy on
  // `placed.length === 0` instead, and give the "docs exist but no real
  // layout yet" case (fallback dots only) its own honest wording.
  //
  // W3.M-c — the measured defect this phase fixes: the status line used to
  // print `docs.length` unconditionally, so a 1089-artifact corpus capped
  // at the gallery's 200-doc page read "200 artifacts" with no hint it was
  // a subset. `countLabel` says "N of M" whenever `isTruncatedPage` is true
  // (full point set unavailable AND the gallery's own total exceeds what's
  // rendered) — otherwise it's just the honest full count.
  const countLabel = isTruncatedPage
    ? `${shownCount} of ${totalKnown} artifacts (showing a truncated page — the full map didn't load)`
    : `${shownCount} artifacts`;
  const statusText = hasAtlasData
    ? `umap-derived layout · ${countLabel}${fallbackCount > 0 ? ` · ${fallbackCount} pending recompute` : ""} · hover a dot for its title`
    : placed.length > 0
      ? `showing placeholder positions for ${countLabel} · click recompute for a real layout`
      : "no atlas data yet — click recompute to populate";

  return (
    <div
      className={
        shell ? "atlas atlas--with-insp atlas--bleed" : "atlas atlas--with-insp"
      }
    >
    <div className="atlas__main">
      {clusters.length > 0 && (
        <div className="atlas-regions" role="list" aria-label="atlas regions">
          {clusters.map((c) => (
            <button
              key={c.cluster}
              type="button"
              role="listitem"
              className="atlas-regions__pill"
              onClick={() => panToCluster(c.centroid.x, c.centroid.y)}
              title={`pan to ${c.label} (${c.count} artifacts)`}
              style={
                { "--region-dot": c.color } as React.CSSProperties
              }
            >
              <span className="atlas-regions__dot" aria-hidden />
              <span className="atlas-regions__name">{c.label}</span>
              <span className="atlas-regions__count">{c.count}</span>
            </button>
          ))}
        </div>
      )}
      {colorMode === "clusters" && clusters.length > 0 && (
        <div
          className="atlas-cluster-legend"
          role="list"
          aria-label="atlas cluster legend"
          data-testid="atlas-cluster-legend"
        >
          <div className="atlas-cluster-legend__head">
            <span>clusters</span>
            <span
              className="atlas-cluster-legend__source"
              title={
                clusters.some((c) => c.labelSource === "keywords")
                  ? "labels: server c-TF-IDF keywords (computed at the last atlas recompute/recluster)"
                  : "labels: client-side tag frequency — recompute the atlas once the label store has rows to switch to keywords"
              }
            >
              {clusters.some((c) => c.labelSource === "keywords")
                ? "keywords"
                : "tags"}
            </span>
          </div>
          {clusters.map((c) => {
            const isHighlighted = highlightCluster === c.cluster;
            return (
              <div key={c.cluster} className="atlas-cluster-legend__row" role="listitem">
                <button
                  type="button"
                  className={`atlas-cluster-legend__chip${isHighlighted ? " is-highlighted" : ""}`}
                  data-kb-act="atlas-legend-cluster"
                  aria-pressed={isHighlighted}
                  onClick={() =>
                    setHighlightCluster((cur) =>
                      cur === c.cluster ? null : c.cluster,
                    )
                  }
                  title={
                    c.terms.length > 0
                      ? `${c.label} — highlight, and see the term breakdown`
                      : `${c.label} — highlight this cluster`
                  }
                  style={
                    { "--cluster-color": clusterColor(c.colorCluster) } as React.CSSProperties
                  }
                >
                  <span className="atlas-cluster-legend__swatch" aria-hidden />
                  <span className="atlas-cluster-legend__label">{c.label}</span>
                  <span className="atlas-cluster-legend__count">{c.count}</span>
                </button>
                <ClusterGalleryLink
                  kb={kb}
                  label={c.label}
                  ids={clusterMemberIds.get(c.cluster) ?? []}
                />
                {isHighlighted && c.terms.length > 0 && (
                  <ClusterTermExplain
                    label={c.label}
                    terms={c.terms}
                    avgTokens={atlasLabelsQuery.data?.avg_tokens}
                  />
                )}
              </div>
            );
          })}
        </div>
      )}
      {showSessions && sessionPolylines.length > 0 && (
        <div
          className="atlas-sessions-legend"
          role="list"
          aria-label="atlas sessions overlay"
          data-testid="atlas-sessions-legend"
        >
          {/* CT-A6 — honesty note: these polylines are an INFERRED
              transcript text scan (literal artifact ids or file-path
              mentions), not the exact `session_files` join the reader's
              Sessions tab uses. A dashed hop marks a path-mention (fuzzy)
              touch — a real, useful signal (it catches artifacts the
              agent discussed but never opened), not unreliable noise. */}
          <span
            className="atlas-sessions-legend__note"
            data-testid="atlas-sessions-legend-note"
            title="Inferred from a transcript text scan: solid hops are literal artifact-id hits, dashed hops are file-path mentions. Path mentions can surface an artifact the agent discussed but never opened — an honest signal, not a guess to distrust."
          >
            inferred · text scan
          </span>
          {sessionPolylines.map((poly) => {
            const isFocused = focusedSession === poly.id;
            return (
              <button
                key={poly.id}
                type="button"
                role="listitem"
                className={`atlas-sessions-legend__chip${isFocused ? " is-focused" : ""}`}
                onClick={() =>
                  setFocusedSession((cur) => (cur === poly.id ? null : poly.id))
                }
                title={poly.preview ?? poly.id}
                style={
                  {
                    "--ses-color": poly.color,
                  } as React.CSSProperties
                }
              >
                <span className="atlas-sessions-legend__swatch" aria-hidden />
                <span className="atlas-sessions-legend__label">
                  {poly.preview
                    ? poly.preview.length > 32
                      ? poly.preview.slice(0, 32) + "…"
                      : poly.preview
                    : poly.id.slice(0, 12)}
                </span>
              </button>
            );
          })}
        </div>
      )}
      {/* W3.M-c — the dim-in-place search overlay. `key={kb}` remounts it
          fresh on every kb switch (clears the input + result), mirroring
          the `dimIds` reset in the effect above. */}
      <MapSearchOverlay key={kb} kb={kb} onDimSet={setDimIds} />
      <div className="atlas__header">
        <span className="atlas__note">{statusText}</span>
        <div className="atlas__header-actions">
          {error && (
            <span className="atlas__error" role="alert">
              {error}
            </span>
          )}
          <button
            type="button"
            className={`atlas__density${showDensity ? " is-on" : ""}`}
            onClick={() => setShowDensity((d) => !d)}
            aria-pressed={showDensity}
            title="Toggle density heatmap behind dots"
          >
            density
          </button>
          <button
            type="button"
            className={`atlas__density${showSessions ? " is-on" : ""}`}
            onClick={() => setShowSessions((s) => !s)}
            aria-pressed={showSessions}
            title="Overlay polylines through artifacts touched by recent sessions"
            data-testid="atlas-sessions-toggle"
          >
            sessions
          </button>
          <button
            type="button"
            className={`atlas__density${dimRead ? " is-on" : ""}`}
            onClick={() => setDimRead(!dimRead)}
            aria-pressed={dimRead}
            title="Dim artifacts you've already finished reading (last 200 opens)"
            data-kb-act="atlas-dim-read"
          >
            dim read
          </button>
          <button
            type="button"
            className={`atlas__density${sizeByLinks ? " is-on" : ""}`}
            onClick={() => setSizeByLinks(!sizeByLinks)}
            aria-pressed={sizeByLinks}
            title="Size dots by backlinks + outlinks — bigger dot, more connected"
            data-kb-act="atlas-size-links"
          >
            size by links
          </button>
          <button
            type="button"
            className={`atlas__density${timelapseOpen ? " is-on" : ""}`}
            onClick={() => {
              setTimelapseOpen((v) => {
                if (v) setPlaying(false);
                return !v;
              });
            }}
            aria-pressed={timelapseOpen}
            aria-expanded={timelapseOpen}
            aria-controls="atlas-timelapse"
            title="Scrub the corpus time-lapse — one frame per recorded atlas recompute"
            data-kb-act="atlas-timelapse"
          >
            time-lapse
          </button>
          <button
            type="button"
            className={`atlas__density${fieldOn ? " is-on" : ""}`}
            onClick={() => setFieldOn(!fieldOn)}
            aria-pressed={fieldOn}
            aria-expanded={fieldOn}
            aria-controls="atlas-field"
            title="Onion-skin YOUR map over the machine's — hand placements, named islands, and a leader line to each dot's machine position"
            data-kb-act="atlas-field"
          >
            my field
          </button>
          <button
            type="button"
            className={`atlas__density${tourOpen ? " is-on" : ""}`}
            onClick={() => setTourOpen((v) => !v)}
            aria-pressed={tourOpen}
            aria-expanded={tourOpen}
            aria-controls="atlas-tour"
            title="Walk a reading list as a path across the map, then hand off to the reader"
            data-kb-act="atlas-tour"
          >
            loci walk
          </button>
          <button
            type="button"
            className={`atlas__density${lassoMode ? " is-on" : ""}`}
            onClick={() => setLassoMode((v) => !v)}
            aria-pressed={lassoMode}
            title="Freehand-select a working set of dots (drag to draw, mouse only)"
            data-kb-act="atlas-lasso"
          >
            lasso
          </button>
          {hasMemoryColorModes && (
            <div
              className="atlas-colormode"
              role="group"
              aria-label="color dots by"
              data-testid="atlas-colormode"
            >
              {(
                [
                  ["clusters", "clusters"],
                  ["salience", "salience"],
                  ["decay", "decay"],
                ] as const
              ).map(([mode, label]) => (
                <button
                  key={mode}
                  type="button"
                  className={`atlas__density${colorMode === mode ? " is-on" : ""}`}
                  aria-pressed={colorMode === mode}
                  onClick={() => setColorMode(mode)}
                  title={
                    mode === "salience"
                      ? "color dots by memory salience — cool = low, hot = high"
                      : mode === "decay"
                        ? "color dots by decay bucket — slow vs fast"
                        : "color dots by their atlas cluster (default)"
                  }
                  data-kb-act={`atlas-colormode-${mode}`}
                >
                  {label}
                </button>
              ))}
            </div>
          )}
          <div className="atlas-cameras" ref={camerasRef}>
            <button
              type="button"
              className={`atlas__density${camerasOpen ? " is-on" : ""}`}
              onClick={() => setCamerasOpen((v) => !v)}
              aria-pressed={camerasOpen}
              aria-expanded={camerasOpen}
              aria-haspopup="true"
              title="Save or recall a bookmarked pan/zoom camera"
              data-kb-act="atlas-cameras"
            >
              cameras{cameras.length > 0 ? ` (${cameras.length})` : ""}
            </button>
            {camerasOpen && (
              <div
                className="atlas-cameras__panel"
                role="menu"
                aria-label="atlas cameras"
              >
                {savingCamera ? (
                  <form
                    className="atlas-cameras__save-form"
                    onSubmit={(e) => {
                      e.preventDefault();
                      const name = cameraNameDraft.trim();
                      if (!name) return;
                      const next = saveCamera(kb, { name, pan, zoom, colorMode });
                      setCameras(next);
                      setCameraNameDraft("");
                      setSavingCamera(false);
                      censusBump("atlas.camera.save");
                      toast.ok(`camera "${name}" saved`);
                    }}
                  >
                    <input
                      // The panel just opened from a click; focusing the
                      // name field it exists to fill is expected here.
                      autoFocus
                      type="text"
                      value={cameraNameDraft}
                      onChange={(e) => setCameraNameDraft(e.target.value)}
                      placeholder="name this view…"
                      maxLength={60}
                      className="atlas-cameras__input"
                      aria-label="camera name"
                      data-kb-act="atlas-camera-name"
                    />
                    <button
                      type="submit"
                      className="atlas-cameras__btn"
                      disabled={!cameraNameDraft.trim()}
                    >
                      save
                    </button>
                    <button
                      type="button"
                      className="atlas-cameras__btn"
                      onClick={() => {
                        setSavingCamera(false);
                        setCameraNameDraft("");
                      }}
                    >
                      cancel
                    </button>
                  </form>
                ) : (
                  <button
                    type="button"
                    className="atlas-cameras__btn atlas-cameras__save"
                    onClick={() => setSavingCamera(true)}
                    data-kb-act="atlas-camera-save"
                  >
                    save current…
                  </button>
                )}
                {cameras.length === 0 ? (
                  <p className="atlas-cameras__empty">no saved cameras yet</p>
                ) : (
                  <ul className="atlas-cameras__list">
                    {cameras
                      .slice()
                      .sort((a, b) => b.savedAt - a.savedAt)
                      .map((c) => (
                        <li key={c.name} className="atlas-cameras__row">
                          <button
                            type="button"
                            className="atlas-cameras__recall"
                            data-kb-act="atlas-camera-recall"
                            title={`recall "${c.name}"`}
                            onClick={() => {
                              setPan({
                                x: clamp(c.pan.x, -PAN_MAX_X, PAN_MAX_X),
                                y: clamp(c.pan.y, -PAN_MAX_Y, PAN_MAX_Y),
                              });
                              setZoom(clamp(c.zoom, ZOOM_MIN, ZOOM_MAX));
                              setColorMode(c.colorMode);
                              censusBump("atlas.camera.recall");
                            }}
                          >
                            {c.name}
                          </button>
                          <button
                            type="button"
                            className="atlas-cameras__delete"
                            data-kb-act="atlas-camera-delete"
                            aria-label={`delete camera ${c.name}`}
                            onClick={() => {
                              void (async () => {
                                const ok = await confirm({
                                  title: "Delete this camera?",
                                  body: `Delete the saved view "${c.name}"? This can't be undone.`,
                                  confirmLabel: "Delete",
                                });
                                if (!ok) return;
                                setCameras(deleteCamera(kb, c.name));
                                toast.ok(`camera "${c.name}" deleted`);
                              })();
                            }}
                          >
                            <Icon.X />
                          </button>
                        </li>
                      ))}
                  </ul>
                )}
              </div>
            )}
          </div>
          <button
            type="button"
            className="atlas__recompute"
            onClick={handleRecluster}
            disabled={pending || !hasAtlasData}
            aria-busy={pending}
            title="Re-run k-means on existing coords (no UMAP)"
          >
            {pending ? "working…" : "recluster"}
          </button>
          <button
            type="button"
            className="atlas__recompute"
            onClick={handleRecompute}
            disabled={pending}
            aria-busy={pending}
          >
            {pending ? "recomputing…" : "recompute atlas"}
          </button>
        </div>
      </div>
      {/* W3.F-c — the operator field's bar: the overlay's one home, and an
          honest account of what is (and isn't) being drawn. */}
      {fieldOn && (
        <div className="atlas-field" id="atlas-field" data-testid="atlas-field">
          <div className="atlas-field__controls">
            <button
              type="button"
              className={`atlas-field__btn${fieldHeat ? " is-on" : ""}`}
              onClick={() => setFieldHeat(!fieldHeat)}
              aria-pressed={fieldHeat}
              title="Colour every dot by how far it sits from where you put it (the daemon's aligned distances)"
              data-kb-act="atlas-field-heat"
            >
              disagreement heat
            </button>
            <button
              type="button"
              className={`atlas-field__btn${placeMode ? " is-on" : ""}`}
              onClick={() => setPlaceMode((v) => !v)}
              aria-pressed={placeMode}
              title="Drag a dot to say where YOU think it belongs — writes the sidecar only; the machine layout never moves"
              data-kb-act="atlas-field-place"
            >
              place
            </button>
            {fieldSaving && (
              <span className="atlas-field__saving" role="status">
                saving…
              </span>
            )}
          </div>
          {fieldQuery.isPending ? (
            <p className="atlas-field__note">loading your field…</p>
          ) : fieldQuery.isError ? (
            <p className="atlas-field__note atlas-field__note--err" role="alert">
              couldn&rsquo;t load the operator field:{" "}
              {fieldQuery.error instanceof Error
                ? fieldQuery.error.message
                : String(fieldQuery.error)}
            </p>
          ) : localPlacements.size === 0 && islandCount === 0 ? (
            <p className="atlas-field__empty" data-testid="atlas-field-empty">
              <strong>Nothing placed by hand yet.</strong> Turn on{" "}
              <em>place</em> and drag a dot to where <em>you</em> think it
              belongs — the ghost and its leader line are the gap between your
              map and the model&rsquo;s. Placements live in a JSON Canvas
              sidecar (<code>atlas/operator.canvas</code>) you can also draw in
              Obsidian; <code>type: &quot;group&quot;</code> nodes there become
              named islands, and kb never invents a name for one.
            </p>
          ) : (
            <p className="atlas-field__note">
              {localPlacements.size} hand placement
              {localPlacements.size === 1 ? "" : "s"}
              {islandCount > 0 ? ` · ${islandCount} island${islandCount === 1 ? "" : "s"}` : ""}
              {" · "}
              {disagreementQuery.data?.matched ?? 0} matched to the machine
              layout
              {maxDisagreement > 0 ? (
                <>
                  {" · largest displacement "}
                  <strong>{maxDisagreement.toFixed(3)}</strong>
                </>
              ) : null}
              .{" "}
              {(disagreementQuery.data?.matched ?? 0) === 0 &&
              localPlacements.size > 0 ? (
                <>
                  Nothing to disagree with yet: this kb has no machine layout
                  to join against (no embeddings, or no atlas recompute yet),
                  so the ghosts below are simply where you put things — the
                  comparison starts the moment there are real coordinates.
                </>
              ) : (
                <>
                  The two fields are brought into one frame by a Procrustes
                  fit computed <em>on the daemon</em> (rotation · uniform
                  scale · offset removed), so this is <em>relative</em>{" "}
                  disagreement — you aren&rsquo;t penalised for having drawn
                  your map at a different angle — and the SPA and{" "}
                  <em>kb atlas field diff</em> report the same numbers.
                </>
              )}
              {islandCount > 0 && !fieldTransform ? (
                <>
                  {" "}
                  Islands aren&rsquo;t drawn yet: placing three artifacts that
                  aren&rsquo;t all in a line is what pins your field to the
                  machine&rsquo;s frame, and until then there is no honest
                  place to put a rectangle.
                </>
              ) : null}
              {fieldOn && framePlaced ? (
                <>
                  {" "}
                  Hidden while a time-lapse frame is up — your field is fitted
                  onto <em>today&rsquo;s</em> layout, and drawing it over a
                  past frame would compare two different maps.
                </>
              ) : null}
            </p>
          )}
        </div>
      )}
      {/* W3.F-c — loci tours: navigation over an existing reading list. */}
      {tourOpen && (
        <TourBar
          kb={kb}
          placedById={placedById}
          onFly={flyTo}
          onClose={() => setTourOpen(false)}
        />
      )}
      {/* W3.T-c — the corpus time-lapse scrubber. Frames START EMPTY and
          that is the honest state, not a loading state: kb retains no past
          layout and no past embedding, so history cannot be backfilled —
          every branch below says what it knows and why, and none of them is
          a bare spinner over a blank chart. */}
      {timelapseOpen && (
        <div
          className="atlas-timelapse"
          id="atlas-timelapse"
          data-testid="atlas-timelapse"
          data-atlas-frame-id={activeFrameMeta ? String(activeFrameMeta.id) : ""}
          data-atlas-frame-count={frames.length}
        >
          {historyQuery.isPending ? (
            <p className="atlas-timelapse__note">loading the frame list…</p>
          ) : historyQuery.isError ? (
            <p className="atlas-timelapse__note atlas-timelapse__note--err" role="alert">
              couldn't load the frame list:{" "}
              {historyQuery.error instanceof Error
                ? historyQuery.error.message
                : String(historyQuery.error)}
            </p>
          ) : frames.length === 0 ? (
            <p className="atlas-timelapse__empty" data-testid="atlas-timelapse-empty">
              <strong>No frames recorded yet.</strong> kb keeps no past layout
              and no past embedding, so a corpus's real history can't be
              recovered after the fact — frames are recorded from now on, one
              per atlas recompute (or recluster). Run <em>recompute atlas</em>{" "}
              above to record the first; the time-lapse fills in as recomputes
              accumulate. To start with something to scrub, {" "}
              <code>kb atlas backfill --kb …</code> writes back-dated{" "}
              <strong>reconstructed</strong> frames (today's embeddings over
              the docs that existed at each past cut point) — useful, but not
              recorded history, and labelled as such on every frame. It's a
              CLI action on purpose: it relays the whole corpus N times.
            </p>
          ) : (
            <>
              <div className="atlas-timelapse__controls">
                {!reducedMotion && (
                  <button
                    type="button"
                    className="atlas-timelapse__play"
                    data-kb-act="atlas-timelapse-play"
                    aria-pressed={playing}
                    disabled={frames.length < 2}
                    title={
                      frames.length < 2
                        ? "only one frame recorded — nothing to play through yet"
                        : "step through the recorded frames"
                    }
                    onClick={() => {
                      if (playing) {
                        setPlaying(false);
                        return;
                      }
                      // Pressing play at the end rewinds first, so the
                      // control is never a no-op.
                      if (clampedIdx >= frames.length - 1) setFrameIdx(0);
                      setPlaying(true);
                    }}
                  >
                    {playing ? "pause" : "play"}
                  </button>
                )}
                <input
                  type="range"
                  className="atlas-timelapse__slider"
                  min={0}
                  max={frames.length - 1}
                  step={1}
                  value={clampedIdx}
                  aria-label="atlas time-lapse frame"
                  aria-valuetext={`frame ${clampedIdx + 1} of ${frames.length}${
                    activeFrameMeta
                      ? `, ${fmtFrameTime(activeFrameMeta.created_at_unix)}`
                      : ""
                  }`}
                  data-kb-act="atlas-timelapse-slider"
                  onChange={(e) => {
                    // Scrubbing by hand takes over from the player.
                    setPlaying(false);
                    setFrameIdx(Number(e.target.value));
                  }}
                />
                <span className="atlas-timelapse__pos">
                  frame {clampedIdx + 1} / {frames.length}
                </span>
              </div>
              {activeFrameMeta && (
                <p className="atlas-timelapse__meta">
                  <span>{fmtFrameTime(activeFrameMeta.created_at_unix)}</span>
                  <span aria-hidden> · </span>
                  {/* Provenance is surfaced on EVERY frame, never implied: a
                      time-lapse that can't tell first-hand from
                      reconstructed history is a lie by omission. 'recorded'
                      comes from a real recompute/recluster; 'reconstructed'
                      from `kb atlas backfill` (W3 T-d) — today's embeddings
                      laid over a past doc subset, spelled out in the note
                      below the meta line, not left to a badge colour. */}
                  <span
                    className={`atlas-timelapse__prov atlas-timelapse__prov--${activeFrameMeta.provenance}`}
                    title={
                      activeFrameMeta.provenance === "recorded"
                        ? "recorded: written by the recompute/recluster that actually produced these coordinates"
                        : "reconstructed: re-derived after the fact, not first-hand — treat the geometry as an estimate"
                    }
                  >
                    {activeFrameMeta.provenance}
                  </span>
                  <span aria-hidden> · </span>
                  <span>{activeFrameMeta.layout}</span>
                  <span aria-hidden> · </span>
                  <span>
                    {activeFrameMeta.point_count} points ·{" "}
                    {activeFrameMeta.cluster_count} clusters
                  </span>
                </p>
              )}
              {/* A one-word badge is not enough for the weaker claim: spell
                  out what a reconstructed frame actually is, every time one
                  is on screen. */}
              {activeFrameMeta?.provenance === "reconstructed" && (
                <p
                  className="atlas-timelapse__note atlas-timelapse__note--reconstructed"
                  data-testid="atlas-timelapse-reconstructed"
                >
                  <strong>Reconstructed, not recorded.</strong> This frame was
                  derived after the fact by <code>kb atlas backfill</code>:
                  today's embeddings laid out over the docs that existed at
                  this cut point (by file mtime). kb retains no past layout and
                  no past embedding, so it shows where these docs{" "}
                  <em>would</em> have sat had the atlas run then — not where
                  they did sit.
                </p>
              )}
              {frameQuery.isError ? (
                <p
                  className="atlas-timelapse__note atlas-timelapse__note--err"
                  role="alert"
                >
                  couldn't load this frame's points:{" "}
                  {frameQuery.error instanceof Error
                    ? frameQuery.error.message
                    : String(frameQuery.error)}{" "}
                  — the map below is still the live layout.
                </p>
              ) : !frameData ? (
                <p className="atlas-timelapse__note">
                  loading this frame's points — the map below is still the live
                  layout.
                </p>
              ) : (
                <>
                  {/* The newest frame is the reference every other frame is
                      aligned ONTO, so it aligns against itself: an identity
                      transform and a residual of exactly 0. Printing the
                      "a residual is never zero" caveat there would be a lie
                      in the other direction — say which case this is. */}
                  {frameData.frame.id === frameData.align_to.id ? (
                    <p className="atlas-timelapse__note">
                      this is the newest frame — it&rsquo;s the
                      reference the others are aligned onto, so there&rsquo;s
                      no alignment to report here (identity transform,
                      residual 0 by construction).
                    </p>
                  ) : (
                    <p className="atlas-timelapse__note">
                      aligned onto frame #{frameData.align_to.id} over{" "}
                      {frameData.matched} shared artifact
                      {frameData.matched === 1 ? "" : "s"} · residual{" "}
                      <strong>{frameData.residual.toFixed(4)}</strong> — don't
                      read this as error: it stays above zero even when
                      nothing actually moved, because the atlas min-max-scales
                      x and y independently, so each frame is stretched
                      differently on each axis and a rigid alignment (rotate ·
                      uniform scale · translate) can't fully undo that. The fit
                      is computed server-side, so this view and{" "}
                      <em>kb atlas show</em> agree exactly.
                    </p>
                  )}
                  <p className="atlas-timelapse__note">
                    colours are matched onto today's clusters by nearest
                    centroid — k-means renumbers clusters on every recompute,
                    so the stored ids alone would strobe. Cluster numbers in
                    the tooltip and legend are still this frame's own.
                    {framePlaced && framePlaced.droppedCount > 0 ? (
                      <>
                        {" "}
                        {framePlaced.droppedCount} artifact
                        {framePlaced.droppedCount === 1 ? "" : "s"} in this
                        frame {framePlaced.droppedCount === 1 ? "is" : "are"} no
                        longer in the corpus and {" "}
                        {framePlaced.droppedCount === 1 ? "isn't" : "aren't"}{" "}
                        drawn.
                      </>
                    ) : null}
                  </p>
                </>
              )}
            </>
          )}
        </div>
      )}
      <canvas
        ref={canvasRef}
        className="atlas__canvas"
        role="img"
        aria-label={
          activeFrameMeta && framePlaced
            ? `Atlas of ${placed.length} artifacts in ${kb}, time-lapse frame ${clampedIdx + 1} of ${frames.length} (${fmtFrameTime(activeFrameMeta.created_at_unix)})`
            : `Atlas of ${shownCount} artifacts in ${kb}`
        }
        // Test hooks (Playwright). The atlas previously rendered one
        // `<circle class="atlas__dot">` per artifact, which gave specs a
        // natural locator + click target; canvas drawing is opaque so we
        // expose a count + the first point's logical coords here.
        // W3.T-c — this counts what is actually DRAWN, which is the live
        // source normally (identical to `shownCount`) and the active
        // time-lapse frame's joinable points while one is up.
        data-atlas-count={placed.length}
        // "" when the live map is showing; the frame id while scrubbing.
        data-atlas-frame={activeFrameMeta && framePlaced ? String(activeFrameMeta.id) : ""}
        // W3.M-c — the search-dim overlay's e2e hook: how many dots are
        // currently dimmed (0 when no dim search is active).
        data-atlas-dimmed-count={dimmedCount}
        data-sessions-count={showSessions ? sessionPolylines.length : 0}
        data-first-dot={
          placed.length > 0
            ? `${placed[0].x.toFixed(2)},${placed[0].y.toFixed(2)}`
            : ""
        }
        // Interaction test hooks — canvas transforms are invisible to
        // the DOM, so wheel-zoom / drag-pan specs read these instead.
        data-atlas-zoom={zoom.toFixed(3)}
        data-atlas-pan={`${pan.x.toFixed(1)},${pan.y.toFixed(1)}`}
        // W2.3b — the working-set instrument's test hook: canvas
        // transforms/selection are DOM-invisible, so e2e reads this
        // alongside `data-atlas-count` etc. rather than counting rings.
        data-atlas-selected-count={selection.size}
        // W3.F-c — the onion skin's hooks: how many operator ghosts are
        // actually drawn, how many named islands, and which of the two
        // extra modes are live (0 = off, same shape as the attrs above).
        data-atlas-field-ghosts={fieldVisible ? fieldGhosts.length : 0}
        data-atlas-field-islands={fieldVisible ? fieldIslandRects.length : 0}
        data-atlas-field-heat={heatOn && fieldVisible ? 1 : 0}
        data-atlas-field-place={placeMode ? 1 : 0}
        onMouseDown={onMouseDown}
        onMouseMove={onMouseMove}
        onMouseUp={endDrag}
        onMouseLeave={() => {
          endDrag();
          setHover(null);
        }}
        onClick={onClick}
        onAuxClick={onAuxClick}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={endPointer}
        onPointerCancel={endPointer}
        style={{
          cursor: placeMode
            ? fieldDrag
              ? "grabbing"
              : hover
                ? "move"
                : "default"
            : lassoMode
            ? "crosshair"
            : dragState.current
              ? "grabbing"
              : hover
                ? "pointer"
                : "grab",
          touchAction: "none",
        }}
      />
      {hover && (
        <div className="atlas__tip" role="status" aria-live="polite">
          <span className="atlas__tip-title">{hover.doc.title || hover.doc.id}</span>
          {hover.fallback ? (
            <span className="atlas__tip-meta"> · pending recompute</span>
          ) : (
            <span className="atlas__tip-meta"> · cluster {hover.cluster}</span>
          )}
        </div>
      )}
      {/* W3.M-d — in the shell the projection panel beside the map owns the
          working set's verbs (open as a list · add to a list · copy for an
          agent · clear), so the in-canvas bar would be a second home for the
          same action. Unchanged everywhere else. */}
      {!shell && selection.size > 0 && (
        <WorkingSetBar
          kb={kb}
          selection={selection}
          docs={sourceDocs}
          onClear={() => setSelection(new Set())}
        />
      )}
    </div>
    <AtlasInspector
      kb={kb}
      doc={selected}
      onClear={() => setSelected(null)}
      touchedBySessions={touchedBySelected}
      atlasPoints={stressPoints}
      onSelectNeighbor={selectAndCenter}
      fieldRow={selectedFieldRow}
    />
    </div>
  );
}

// W3.F-c — LOCI TOURS: the memory-palace walk, and nothing more.
//
// A tour IS a reading list. kb-list/1 is already ordered (dense 0-based
// `position`, reordered by `kb list move`), the reader already traverses it
// (`?list=&entry=` + `components/lists/QueueBar.tsx`), and read state is
// already DERIVED per response (invariant #25). So this bar owns exactly
// one thing the stack didn't have: MOVEMENT ACROSS THE MAP between one
// entry and the next, ending in a hand-off to the reader through
// `entryTrailHref` — the same single trail-href builder every other list
// surface uses.
//
// What it deliberately does NOT have: a tour store, a completion
// percentage, a "finished" state, a streak, a badge. Read state is the
// only progress signal shown, and it comes from the list detail response
// like everywhere else. That is a recorded non-goal (README → Non-goals),
// not a styling preference — walking a palace is not a chore to complete.
function TourBar({
  kb,
  placedById,
  onFly,
  onClose,
}: {
  kb: string;
  /// Every dot the atlas actually drew, keyed by artifact id. A `Placed`
  /// carries more than `TourPoint` needs; passing the live map (rather
  /// than a copy) keeps AtlasView's `placed` the single source of truth.
  placedById: ReadonlyMap<string, TourPoint>;
  onFly: (x: number, y: number) => void;
  onClose: () => void;
}) {
  const { lists } = useLists();
  const kbLists = useMemo(() => lists.filter((l) => l.kb === kb), [lists, kb]);
  const [listId, setListId] = useState<string>("");
  const { entries, loading } = useListDetail(kb, listId || undefined);
  const stops = useMemo(
    () => tourStops(entries, placedById),
    [entries, placedById],
  );
  const skipped = entries.length - stops.length;
  const [idx, setIdx] = useState(0);
  const clamped = stops.length === 0 ? 0 : Math.min(idx, stops.length - 1);
  const stop = stops[clamped] ?? null;
  const entryById = useMemo(() => {
    const m = new Map(entries.map((e) => [e.id, e]));
    return m;
  }, [entries]);
  const currentEntry = stop ? entryById.get(stop.entryId) : undefined;
  const href =
    currentEntry && listId ? entryTrailHref(kb, listId, currentEntry) : null;

  // Fly whenever the STOP changes (including the first one after a list is
  // picked). Keyed on the entry id, not the object, so a list refetch that
  // returns the same stop doesn't re-fly.
  useEffect(() => {
    if (stop) onFly(stop.x, stop.y);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [stop?.entryId]);

  const go = (dir: -1 | 1) => setIdx((i) => stepStop(stops, i, dir));

  return (
    <div className="atlas-tour" id="atlas-tour" data-testid="atlas-tour">
      <div className="atlas-tour__controls">
        <label className="atlas-tour__pick">
          <span className="atlas-tour__lab">walk</span>
          <select
            value={listId}
            onChange={(e) => {
              setListId(e.target.value);
              setIdx(0);
              if (e.target.value) censusBump("atlas.tour.start");
            }}
            aria-label="reading list to walk"
            data-kb-act="atlas-tour-list"
          >
            <option value="">choose a reading list…</option>
            {kbLists.map((l) => (
              <option key={l.id} value={l.id}>
                {l.title}
              </option>
            ))}
          </select>
        </label>
        {stop && (
          <>
            <span className="atlas-tour__pos" data-testid="atlas-tour-pos">
              stop {clamped + 1} / {stops.length}
            </span>
            <span className="atlas-tour__label" title={stop.label}>
              {stop.label}
            </span>
            {/* Read state is DERIVED (#25) and is the ONLY progress signal
                a walk shows — no percentage, no "done". */}
            {currentEntry && !currentEntry.is_session && (
              <span
                className={`atlas-tour__read is-${currentEntry.read_state}`}
                title={`read state: ${currentEntry.read_state} (derived — override, section dwell, or scroll completion)`}
              >
                {currentEntry.read_state.replace("_", " ")}
              </span>
            )}
            <button
              type="button"
              className="atlas-tour__nav"
              onClick={() => go(-1)}
              disabled={clamped === 0}
              title="previous stop"
            >
              ← prev
            </button>
            <button
              type="button"
              className="atlas-tour__nav"
              onClick={() => go(1)}
              disabled={clamped >= stops.length - 1}
              title="next stop"
              data-kb-act="atlas-tour-next"
            >
              next →
            </button>
            {href && (
              <Link
                className="atlas-tour__open"
                to={href}
                onClick={() => censusBump("atlas.tour.open")}
                data-kb-act="atlas-tour-open"
                title="open this stop in the reader — the queue bar takes over from there"
              >
                read here →
              </Link>
            )}
          </>
        )}
        <span className="atlas-tour__spacer" />
        <button
          type="button"
          className="atlas-tour__exit"
          onClick={onClose}
          title="leave the walk"
          aria-label="close loci walk"
        >
          <Icon.X />
        </button>
      </div>
      {listId && !loading && stops.length === 0 ? (
        <p className="atlas-tour__note">
          {entries.length === 0
            ? "this list has no entries yet — add some and the walk has somewhere to go."
            : "none of this list's artifacts are on the map: every entry is either tombstoned or not part of the layout this atlas drew."}
        </p>
      ) : skipped > 0 && stop ? (
        <p className="atlas-tour__note">
          {skipped} of {entries.length} entr{entries.length === 1 ? "y" : "ies"}{" "}
          {skipped === 1 ? "isn't" : "aren't"} on this map (tombstoned, or not
          in the drawn layout) and {skipped === 1 ? "is" : "are"} skipped —
          exactly the way the reader&rsquo;s queue bar skips them.
        </p>
      ) : !listId ? (
        <p className="atlas-tour__note">
          A walk is just a reading list, taken in order across the map: each
          stop flies the camera to that artifact&rsquo;s dot, and{" "}
          <em>read here</em> hands off to the reader with the trail intact.
          Nothing is recorded: kb keeps no tour progress and no completion —
          the read state you already have is the only progress there is.
        </p>
      ) : null}
    </div>
  );
}

// W1.atlas — the cluster legend's term-breakdown panel: the "explainability
// click" the frontier synthesis names as kb's signature move, applied to
// atlas cluster labels. This stands in for the shared `ScoreExplain`
// popover primitive (`web/src/components/ScoreExplain.tsx`, props roughly
// `{ label, total, terms: [{label, value, detail?, color?}], footnote,
// children }`), which hadn't landed in this worktree when this phase built
// (see the phase brief's WIRE CAVEAT — same "batch-1 primitive missing"
// situation, here for a whole component rather than a generated field).
// Renders as an inline expand under the legend row instead of a floating
// popover, so it needs no outside-click/z-index machinery; the term rows
// (label/value/detail) already match ScoreExplain's documented shape, so
// swapping to `<ScoreExplain>` once it ships is a render-only change.
function ClusterTermExplain({
  label,
  terms,
  avgTokens,
}: {
  label: string;
  terms: AtlasLabelTerm[];
  avgTokens?: number;
}) {
  const avgLabel = avgTokens != null ? avgTokens.toFixed(0) : "A";
  return (
    <div
      className="atlas-cluster-legend__explain"
      role="note"
      aria-label={`${label} term breakdown`}
    >
      <table className="atlas-cluster-legend__explain-table">
        <tbody>
          {terms.slice(0, 5).map((t) => (
            <tr key={t.term}>
              <td className="atlas-cluster-legend__explain-term">{t.term}</td>
              <td className="atlas-cluster-legend__explain-value">
                {t.score.toFixed(3)}
              </td>
              <td className="atlas-cluster-legend__explain-detail">
                tf {t.tf} × ln(1+{avgLabel}/{t.ft})
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <p className="atlas-cluster-legend__explain-foot">
        score = tf × ln(1 + avg_tokens/ft) — term frequency in this cluster,
        weighted up the fewer other clusters ("ft") the term also appears
        in; higher = more distinctive to {label}.
      </p>
    </div>
  );
}

// W2.3b→W2.3a — the ids= grammar is real now; route through the ONE
// deep-link builder (invariant #35).
function atlasGalleryIdsUrl(kb: string, ids: readonly string[]): string {
  return galleryUrl(kb, { ids: [...ids] });
}
// W2.3b — click-to-filter: each cluster legend row can open its members as
// a gallery `&ids=` filter. Bounded by MAX_FILTER_IDS (the same envelope
// cap docs.rs enforces on a page) — past that the link is a disabled,
// title-explained span rather than an unusably long URL.
function ClusterGalleryLink({
  kb,
  label,
  ids,
}: {
  kb: string;
  label: string;
  ids: string[];
}) {
  if (ids.length === 0) return null;
  if (ids.length > MAX_FILTER_IDS) {
    return (
      <span
        className="atlas-cluster-legend__open-gallery is-disabled"
        title={`${ids.length} artifacts — too many for a gallery link (cap ${MAX_FILTER_IDS})`}
      >
        open in gallery
      </span>
    );
  }
  return (
    <Link
      className="atlas-cluster-legend__open-gallery"
      to={atlasGalleryIdsUrl(kb, ids)}
      title={`open the ${ids.length} artifacts in "${label}" as a gallery filter`}
      data-kb-act="atlas-legend-open-gallery"
      onClick={() => censusBump("atlas.selection.filter")}
    >
      open in gallery
    </Link>
  );
}

// W2.3b — the kb-list/1 Markdown grammar (`kb_core::lists::md`), enough of
// it for a fresh unchecked-checklist import: `# Title` then `N. [ ]
// [text](target)` rows. Mirrors `sanitize_link_text` (lists.rs) — square
// brackets in a title would break the `](` scan, so they're swapped for
// lookalikes, same as the server's own export path.
function buildWorkingSetMarkdown(
  title: string,
  entries: { title: string; source_relative: string }[],
): string {
  const safeTitle = title.replace(/[[\]]/g, (c) => (c === "[" ? "⟦" : "⟧"));
  const lines = [`# ${safeTitle}`, ""];
  entries.forEach((e, i) => {
    const text = (e.title || e.source_relative).replace(/[[\]]/g, (c) =>
      c === "[" ? "⟦" : "⟧",
    );
    lines.push(`${i + 1}. [ ] [${text}](${e.source_relative})`);
  });
  return lines.join("\n") + "\n";
}

// W2.3b — bulk-load a kb-list/1 Markdown document into an EXISTING list in
// ONE transaction / ONE `list.updated` emit (invariant #25), instead of N×
// per-entry `addListEntry` calls. Mirrors the CLI's `kb list import`
// (`crates/kb-cli/src/commands/list.rs`) — the server's `body: String`
// extractor takes the raw Markdown text, no JSON envelope. Kept local to
// this component (not added to `api/client.ts`, which this phase doesn't
// own beyond the `fetchAtlasSimilar` addition above) — same
// `currentDaemonBase()` + problem+json error shape every other fetcher
// uses, just not exported for reuse elsewhere yet.
async function postListImportMarkdown(
  kb: string,
  listId: string,
  markdown: string,
  mode: "replace" | "append",
): Promise<{ imported: number; skipped: { ref: string; reason: string }[] }> {
  const r = await fetch(
    `${currentDaemonBase()}/api/kb/${encodeURIComponent(kb)}/lists/${encodeURIComponent(listId)}/import?format=md&mode=${mode}`,
    { method: "POST", headers: { Accept: "application/json" }, body: markdown },
  );
  if (!r.ok) {
    const ct = r.headers.get("content-type") ?? "";
    let detail: string | undefined;
    if (ct.includes("application/problem+json")) {
      try {
        const problem = (await r.json()) as { title?: string; detail?: string };
        detail = `${problem.title ?? "error"}: ${problem.detail ?? ""}`;
      } catch {
        // torn/empty problem body — fall through to the status line
      }
    }
    throw new ApiError(
      `list import failed: ${detail ?? `${r.status} ${r.statusText}`}`,
      r.status,
    );
  }
  return (await r.json()) as {
    imported: number;
    skipped: { ref: string; reason: string }[];
  };
}

// W2.3b — the working-set bar: appears under the canvas whenever the
// lasso (or a future selection source) leaves a non-empty `selection`.
// Three destinations (recon §4) + clear: open the set as a gallery
// filter, add it to a reading list (existing or new, one kb-list/1
// import — invariant #25), or copy a ready-to-run `kb list import`
// heredoc for an agent (mirrors sessions.tsx's `buildResume` clipboard
// pattern).
// W3.M-d — exported so MapShell's projection panel can host the SAME bar
// rather than growing a second home for "open as a filter / add to a list /
// copy for an agent" (invariant #30's one-home-per-action spirit).
export function WorkingSetBar({
  kb,
  selection,
  docs,
  onClear,
  onPivot,
}: {
  kb: string;
  selection: Set<string>;
  docs: DocSummary[];
  onClear: () => void;
  /// W3.M-d — fired when the selection is opened as a gallery list, so the
  /// map-home shell can count its own gate metric without a second button.
  onPivot?: () => void;
}) {
  const selectedDocs = useMemo(
    () => docs.filter((d) => selection.has(d.id)),
    [docs, selection],
  );
  const ids = useMemo(() => [...selection], [selection]);
  const { lists, create } = useLists();
  const navigate = useNavigate();
  const kbLists = useMemo(() => lists.filter((l) => l.kb === kb), [lists, kb]);
  const [addOpen, setAddOpen] = useState(false);
  const [newListName, setNewListName] = useState("");
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState(false);
  const addRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!addOpen) return;
    const onDoc = (e: globalThis.MouseEvent) => {
      if (!addRef.current?.contains(e.target as Node)) setAddOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setAddOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    window.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      window.removeEventListener("keydown", onKey);
    };
  }, [addOpen]);

  const tooMany = ids.length > MAX_FILTER_IDS;

  const addToList = async (listId: string, title: string) => {
    setBusy(true);
    try {
      const md = buildWorkingSetMarkdown(
        `Atlas selection — ${title}`,
        selectedDocs,
      );
      const res = await postListImportMarkdown(kb, listId, md, "append");
      toast.ok(`added ${res.imported} to "${title}"`);
      censusBump("atlas.selection.addToList");
      setAddOpen(false);
      setNewListName("");
    } catch (e) {
      toast.err(`add to list failed: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const addToNew = async () => {
    const title = newListName.trim();
    if (!title) return;
    setBusy(true);
    try {
      const created: ListSummary = await create(kb, title);
      await addToList(created.id, created.title);
    } catch (e) {
      toast.err(`create list failed: ${e instanceof Error ? e.message : String(e)}`);
      setBusy(false);
    }
  };

  // W2.4 — "new board" variant: create list → import the selection (the
  // SAME one-tx markdown import `addToList` uses) → PUT a
  // `defaultLayoutFor` canvas (deterministic grid, list-position order —
  // `selectedDocs` is already that order) → navigate straight to the
  // board. Standing rule 2: no third store — the canvas is a corpus
  // sidecar, not new atlas/selection state.
  const addToNewBoard = async () => {
    const title = newListName.trim();
    if (!title) return;
    setBusy(true);
    try {
      const created: ListSummary = await create(kb, title);
      const md = buildWorkingSetMarkdown(`Atlas selection — ${title}`, selectedDocs);
      const res = await postListImportMarkdown(kb, created.id, md, "append");
      await putBoardCanvas(kb, created.id, defaultLayoutFor(selectedDocs));
      toast.ok(`created board "${title}" — ${res.imported} artifact(s)`);
      censusBump("atlas.selection.newBoard");
      setAddOpen(false);
      setNewListName("");
      navigate(`/board/${kb}/${created.id}`);
    } catch (e) {
      toast.err(`create board failed: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const copyForAgent = () => {
    const md = buildWorkingSetMarkdown(
      `Atlas selection (${selectedDocs.length} artifacts)`,
      selectedDocs,
    );
    const cmd = `kb list import - --kb ${kb} <<'EOF'\n${md}EOF`;
    void navigator.clipboard?.writeText(cmd).then(
      () => {
        setCopied(true);
        censusBump("atlas.selection.copyAgent");
        setTimeout(() => setCopied(false), 1500);
      },
      () => toast.err("copy failed — clipboard unavailable"),
    );
  };

  return (
    <div className="atlas-workingset" role="region" aria-label="atlas working set">
      <span className="atlas-workingset__count">{selection.size} selected</span>
      {tooMany ? (
        <span
          className="atlas-workingset__btn is-disabled"
          title={`${ids.length} artifacts — too many for a gallery link (cap ${MAX_FILTER_IDS})`}
        >
          open as gallery filter
        </span>
      ) : (
        <Link
          className="atlas-workingset__btn"
          to={atlasGalleryIdsUrl(kb, ids)}
          onClick={() => {
            censusBump("atlas.selection.filter");
            // W3.M-d — the shell's own gate counter rides the SAME button
            // (no second pivot home); no-op in the plain atlas view.
            onPivot?.();
          }}
          data-kb-act="atlas-selection-filter"
        >
          open as gallery filter
        </Link>
      )}
      <div className="atlas-workingset__add" ref={addRef}>
        <button
          type="button"
          className="atlas-workingset__btn"
          aria-pressed={addOpen}
          aria-expanded={addOpen}
          aria-haspopup="true"
          onClick={() => setAddOpen((v) => !v)}
          disabled={busy}
          data-kb-act="atlas-selection-add-list"
        >
          add to list
        </button>
        {addOpen && (
          <div className="atlas-workingset__add-panel" role="menu">
            {kbLists.length > 0 && (
              <ul className="atlas-workingset__add-list">
                {kbLists.map((l) => (
                  <li key={l.id}>
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() => void addToList(l.id, l.title)}
                    >
                      {l.title}
                    </button>
                  </li>
                ))}
              </ul>
            )}
            <form
              className="atlas-workingset__add-form"
              onSubmit={(e) => {
                e.preventDefault();
                void addToNew();
              }}
            >
              <input
                type="text"
                value={newListName}
                onChange={(e) => setNewListName(e.target.value)}
                placeholder="new list name…"
                maxLength={80}
                disabled={busy}
                data-kb-act="atlas-selection-new-list-name"
              />
              <button type="submit" disabled={busy || !newListName.trim()}>
                create
              </button>
              <button
                type="button"
                disabled={busy || !newListName.trim()}
                onClick={() => void addToNewBoard()}
                title="create a list AND a board laid out from this selection"
                data-kb-act="atlas-selection-new-board"
              >
                new board
              </button>
            </form>
          </div>
        )}
      </div>
      <button
        type="button"
        className="atlas-workingset__btn"
        onClick={copyForAgent}
        data-kb-act="atlas-selection-copy-agent"
      >
        {copied ? "copied" : "copy for agent"}
      </button>
      <button
        type="button"
        className="atlas-workingset__clear"
        onClick={onClear}
        title="clear selection"
        aria-label="clear selection"
        data-kb-act="atlas-selection-clear"
      >
        <Icon.X />
      </button>
    </div>
  );
}

// FNV-1a 32-bit. Splits the 32 bits into two 16-bit halves for x/y.
// Stable across runs for the same id, no dependencies, fast enough.
function hashPoint(s: string): [number, number] {
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  const u = ((h >>> 16) & 0xffff) / 0xffff;
  const v = (h & 0xffff) / 0xffff;
  return [u, v];
}

// W3.F-c — one ghost: the operator's position (a hollow ring, so it reads
// as "not the real dot"), plus a leader line to where the machine put the
// same artifact. Extracted from the draw loop so the saved-vs-unsaved
// (`pending`) variants can't drift apart.
function drawGhost(
  ctx: CanvasRenderingContext2D,
  gx: number,
  gy: number,
  mx: number,
  my: number,
  accent: string,
  lineScale: number,
  pending: boolean,
) {
  ctx.strokeStyle = accent;
  ctx.globalAlpha = pending ? 0.55 : 0.32;
  ctx.lineWidth = 0.8 * lineScale;
  ctx.beginPath();
  ctx.moveTo(gx, gy);
  ctx.lineTo(mx, my);
  ctx.stroke();
  ctx.globalAlpha = pending ? 1 : 0.8;
  ctx.lineWidth = 1.2 * lineScale;
  ctx.beginPath();
  ctx.arc(gx, gy, 4 * lineScale, 0, Math.PI * 2);
  ctx.stroke();
  if (pending) {
    // An uncommitted placement gets a filled centre — the one visual
    // difference between "you're holding it" and "the daemon has it".
    ctx.globalAlpha = 0.55;
    ctx.fillStyle = accent;
    ctx.beginPath();
    ctx.arc(gx, gy, 1.6 * lineScale, 0, Math.PI * 2);
    ctx.fill();
  }
}

function clusterColor(c: number): string {
  if (c < 0) return CLUSTER_PALETTE[0];
  return CLUSTER_PALETTE[c % CLUSTER_PALETTE.length];
}

function clamp(x: number, lo: number, hi: number): number {
  return Math.max(lo, Math.min(hi, x));
}

// W3.T-c — a frame's timestamp, in UTC, minute grain. Mirrors the CLI's
// `commands::atlas::fmt_ts` (`YYYY-MM-DD HH:MM`) so `kb atlas history` and
// the scrubber name the same frame the same way. UTC, not local: frame
// times are `created_at_unix` and the CLI prints UTC — a local-time SPA
// would silently disagree with the CLI about which day a frame is from.
function fmtFrameTime(unix: number): string {
  if (!Number.isFinite(unix)) return "unknown time";
  const iso = new Date(unix * 1000).toISOString();
  return `${iso.slice(0, 10)} ${iso.slice(11, 16)} UTC`;
}

function truncate(s: string, n: number): string {
  return s.length > n ? s.slice(0, n - 1) + "…" : s;
}
