// F4 — pure path assembly + validation for move-artifact / rename-folder.
// Kept free of React/API so vitest can pin the contract without the SPA.

export type PathOk = { ok: true; path: string };
export type PathErr = { ok: false; error: string };
export type PathResult = PathOk | PathErr;

/**
 * Reject a relative path that has leading/trailing slashes, empty
 * segments, "..", or is otherwise not a clean source-relative path.
 * Empty string is allowed when `allowEmpty` (kb root / no subfolder).
 */
export function validateRelPath(
  raw: string,
  opts: { allowEmpty?: boolean; label?: string } = {},
): PathResult {
  const label = opts.label ?? "path";
  if (raw === "") {
    if (opts.allowEmpty) return { ok: true, path: "" };
    return { ok: false, error: `${label} is empty` };
  }
  if (raw.startsWith("/") || raw.endsWith("/")) {
    return {
      ok: false,
      error: `${label} must not start or end with "/"`,
    };
  }
  if (raw.includes("//")) {
    return { ok: false, error: `${label} must not contain empty segments` };
  }
  const segs = raw.split("/");
  for (const seg of segs) {
    if (seg === "") {
      return { ok: false, error: `${label} must not contain empty segments` };
    }
    if (seg === "." || seg === "..") {
      return { ok: false, error: `${label} must not contain "." or ".."` };
    }
  }
  return { ok: true, path: raw };
}

/** Filename only — no slashes, same segment rules as a single path piece. */
export function validateFilename(raw: string): PathResult {
  if (raw === "") return { ok: false, error: "filename is empty" };
  if (raw.includes("/")) {
    return { ok: false, error: 'filename must not contain "/"' };
  }
  if (raw === "." || raw === "..") {
    return { ok: false, error: 'filename must not be "." or ".."' };
  }
  // Reject accidental whitespace-only or padded names the server would
  // treat oddly; leading/trailing space is not a valid segment either.
  if (raw.trim() !== raw) {
    return { ok: false, error: "filename must not have leading/trailing space" };
  }
  return { ok: true, path: raw };
}

/**
 * Build the destination source-relative path for a move:
 *   folder + optional newSubfolder + filename
 *
 * - `folder` may be "" (kb root)
 * - `newSubfolder` is optional; when set, appended under `folder`
 * - `filename` is required (may differ from the current name = rename-in-place)
 */
export function joinMoveTarget(opts: {
  folder: string;
  newSubfolder?: string;
  filename: string;
}): PathResult {
  const folder = opts.folder;
  const sub = (opts.newSubfolder ?? "").trim();
  const file = opts.filename;

  const folderOk = validateRelPath(folder, {
    allowEmpty: true,
    label: "folder",
  });
  if (!folderOk.ok) return folderOk;

  if (sub !== "") {
    const subOk = validateRelPath(sub, { label: "new subfolder" });
    if (!subOk.ok) return subOk;
  }

  const fileOk = validateFilename(file);
  if (!fileOk.ok) return fileOk;

  const dir = [folderOk.path, sub === "" ? "" : sub]
    .filter((p) => p !== "")
    .join("/");
  const path = dir === "" ? fileOk.path : `${dir}/${fileOk.path}`;
  // Final belt-and-braces (double-slash / .. can't appear if pieces are clean).
  return validateRelPath(path, { label: "target" });
}

/** True when the move would be a no-op (server should not be called). */
export function isUnchangedTarget(from: string, to: string): boolean {
  return from === to;
}

/**
 * Validate a rename-folder destination (folder path only — not a filename).
 * Same segment rules as joinMoveTarget's folder pieces; empty is rejected
 * (cannot rename a folder to the kb root).
 */
export function validateFolderRenameTarget(to: string): PathResult {
  return validateRelPath(to, { allowEmpty: false, label: "folder" });
}
