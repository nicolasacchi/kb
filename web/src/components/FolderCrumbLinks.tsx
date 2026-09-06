import { Link } from "react-router-dom";
import { galleryUrl } from "../lib/galleryUrl";

// v0.22 — render a doc's folder as per-SEGMENT gallery deep-links. Each
// segment links to the gallery filtered to that cumulative folder
// (descendant-inclusive, matching gallery.tsx folder semantics). A root-level
// doc (folder === "") renders a single "(root)" link to the unfiltered kb
// gallery. Shared by the reader's Identifier passport + the ContextBar
// breadcrumb so the folder→gallery pivot is byte-identical in both places.
export default function FolderCrumbLinks({
  kb,
  folder,
  linkClass,
}: {
  kb: string;
  folder: string;
  /// Class applied to each segment <Link> (lets each host style its crumbs).
  linkClass?: string;
}) {
  if (!folder) {
    return (
      <Link className={linkClass} to={galleryUrl(kb)} title="all files in this kb">
        (root)
      </Link>
    );
  }
  const segs = folder.split("/").filter(Boolean);
  // Precompute the cumulative path for each segment ("a", "a/b", "a/b/c").
  const cumulative: string[] = [];
  segs.forEach((seg, i) => {
    cumulative.push(i === 0 ? seg : `${cumulative[i - 1]}/${seg}`);
  });
  return (
    <>
      {segs.map((seg, i) => (
        <span key={cumulative[i]}>
          {i > 0 && <span className="kb-crumb__sep">/</span>}
          <Link
            className={linkClass}
            to={galleryUrl(kb, { folder: cumulative[i] })}
            title={`files in ${cumulative[i]} (+ subfolders)`}
          >
            {seg}
          </Link>
        </span>
      ))}
    </>
  );
}
