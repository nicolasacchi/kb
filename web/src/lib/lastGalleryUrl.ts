// X1 — the gallery records the full URL it was last viewed at (path + query:
// kb, tags, folder, sort, view, …) so the Detail "back to recent" button can
// return you to exactly that filtered view instead of the bare default grid.
// Returning to the same URL also re-hits the useScrollRestoration slot, so the
// scroll position comes back too. Module-level (one tab, ephemeral) — never
// persisted; a fresh tab simply has no last-gallery and falls back.
//
// The `kb` is stored alongside so the back button only reuses the URL when it
// belongs to the artifact's own corpus (a directly-opened /a/otherkb/... never
// jumps to a stale gallery for a different kb).

type LastGallery = { url: string; kb: string | null };

let last: LastGallery | null = null;

export function setLastGalleryUrl(url: string, kb: string | null): void {
  last = { url, kb };
}

export function getLastGalleryUrl(): LastGallery | null {
  return last;
}
