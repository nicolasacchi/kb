// CT-B3 — the one "show in gallery" exit every cross-entity list that
// already resolves to artifact ids gets, riding the shipped `?ids=` atom
// (invariant #35). A thin presentational wrapper over `galleryUrl` +
// `idsPivot`'s pure cap check — three call sites (a session's touched
// files, its recalled memories, a memory's `LineageViewer` chain) share
// this rather than three copies of the same over-cap branch.
import { Link } from "react-router-dom";
import { galleryUrl } from "../lib/galleryUrl";
import { GALLERY_IDS_CAP, isOverIdsCap } from "../lib/idsPivot";

export default function IdsGalleryChip({
  kb,
  ids,
  label,
  className = "kb-idschip",
  testId,
}: {
  /// The kb this chip's `ids` all belong to (the gallery route is kb-scoped
  /// — invariant #33 — so a chip only ever links into ONE corpus).
  kb: string;
  ids: readonly string[];
  label: string;
  className?: string;
  testId?: string;
}) {
  if (ids.length === 0) return null;
  // Over-cap degrades LOUDLY (invariant #35): a disabled chip explaining
  // why, never a link the gallery would 400 on.
  if (isOverIdsCap(ids)) {
    return (
      <span
        className={`${className} ${className}--disabled`}
        aria-disabled="true"
        data-testid={testId}
        title={`${ids.length} artifacts exceeds the gallery's ${GALLERY_IDS_CAP}-id ?ids= cap — too many to pivot in one view`}
      >
        {label}
      </span>
    );
  }
  return (
    <Link
      className={className}
      to={galleryUrl(kb, { ids: [...ids] })}
      title={`open in the ${kb} gallery`}
      data-testid={testId}
    >
      {label}
    </Link>
  );
}
