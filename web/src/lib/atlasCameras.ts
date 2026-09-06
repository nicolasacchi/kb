// W1.atlas — bookmarkable atlas "cameras" (pan/zoom/color-mode presets).
//
// Client-view state only: what to look at, not corpus data. Per-kb,
// localStorage-only (key `kb:atlas:cameras:<kb>`), schema-versioned
// (`v: 1`) so a Wave-2 selection-preset upgrade (working sets, semantic
// zoom) can extend the shape without breaking cameras saved today — see
// the milestone plan's recorded CLI-parity exemption for this feature
// (client-view state, revisited when Wave 2 lands).
//
// Pure module — no React, no fetch — so it's covered by plain vitest
// (colocated `atlasCameras.test.ts`), like `galleryUrl.ts`.

/// Forward-compatible color-mode enum. MI-W4.5 adds "salience"/"decay" (the
/// memory-map modes, offered only on a memory-scoped kb — see
/// `lib/atlasMemory.ts`'s `hasMemoryPoints`); more modes may follow. Kept as
/// a closed union (not a bare string) so `isValidCamera` can reject an
/// unrecognised value read back from storage instead of trusting it blind.
export type AtlasCameraColorMode = "clusters" | "salience" | "decay";

const VALID_COLOR_MODES: readonly AtlasCameraColorMode[] = [
  "clusters",
  "salience",
  "decay",
];

export type AtlasCamera = {
  v: 1;
  name: string;
  pan: { x: number; y: number };
  zoom: number;
  colorMode: AtlasCameraColorMode;
  /// Unix milliseconds (`Date.now()`), used for newest-first sort.
  savedAt: number;
};

function storageKey(kb: string): string {
  return `kb:atlas:cameras:${kb}`;
}

function isFiniteNumber(x: unknown): x is number {
  return typeof x === "number" && Number.isFinite(x);
}

function isValidCamera(x: unknown): x is AtlasCamera {
  if (typeof x !== "object" || x === null) return false;
  const c = x as Record<string, unknown>;
  if (c.v !== 1) return false;
  if (typeof c.name !== "string" || c.name.length === 0) return false;
  if (typeof c.pan !== "object" || c.pan === null) return false;
  const pan = c.pan as Record<string, unknown>;
  if (!isFiniteNumber(pan.x) || !isFiniteNumber(pan.y)) return false;
  if (!isFiniteNumber(c.zoom)) return false;
  if (!VALID_COLOR_MODES.includes(c.colorMode as AtlasCameraColorMode)) {
    return false;
  }
  if (!isFiniteNumber(c.savedAt)) return false;
  return true;
}

function write(kb: string, cameras: AtlasCamera[]) {
  try {
    localStorage.setItem(storageKey(kb), JSON.stringify(cameras));
  } catch {
    // localStorage denied/full — the save doesn't persist past this
    // session; the caller's in-memory state (from the returned array)
    // still reflects it for the current tab.
  }
}

/// Every camera saved for `kb`, tolerant of corrupt/foreign JSON (a bad
/// blob, a future schema version, hand-edited storage) — malformed entries
/// are silently dropped rather than throwing, so one bad row can't break
/// the whole dropdown. Order is storage order (callers sort for display).
export function listCameras(kb: string): AtlasCamera[] {
  if (typeof localStorage === "undefined") return [];
  try {
    const raw = localStorage.getItem(storageKey(kb));
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(isValidCamera);
  } catch {
    return [];
  }
}

/// Save (or overwrite, by exact name) a camera and return the updated list.
/// `savedAt` is stamped here — callers pass everything else.
export function saveCamera(
  kb: string,
  camera: Omit<AtlasCamera, "v" | "savedAt">,
): AtlasCamera[] {
  const next: AtlasCamera = { v: 1, savedAt: Date.now(), ...camera };
  const updated = [...listCameras(kb).filter((c) => c.name !== next.name), next];
  write(kb, updated);
  return updated;
}

/// One camera by exact name, or `undefined` if it isn't saved (or storage
/// is corrupt/denied).
export function recallCamera(kb: string, name: string): AtlasCamera | undefined {
  return listCameras(kb).find((c) => c.name === name);
}

/// Delete by exact name and return the updated list (a no-op delete still
/// returns the unchanged list — callers don't need to special-case "not
/// found").
export function deleteCamera(kb: string, name: string): AtlasCamera[] {
  const updated = listCameras(kb).filter((c) => c.name !== name);
  write(kb, updated);
  return updated;
}
