// V76-R3c — banners in the reader are GENERATED from kbc-frames/1.
//
// One function, one table. A hand-written "file at ref: ODB" string in a
// component would drift from GET /api/frames the moment the table moved.
// `frameBanner(lane, row, atRef)` is the only renderer; the golden walks
// the table × lanes.

export interface FrameRow {
  lane: string;
  source: string;
  off_head: string;
  ref_aware: boolean;
  why: string;
}

const SOURCE_LABEL: Readonly<Record<string, string>> = {
  working_tree: "working tree",
  odb: "ODB",
  git: "git",
  refused: "refused",
};

function sourceLabel(source: string): string {
  return SOURCE_LABEL[source] ?? source;
}

/// `null` on the working tree (no banner). Off a ref, every lane gets a
/// sentence derived from its table row — never a string someone typed in
/// a component.
export function frameBanner(lane: string, row: FrameRow, atRef: string | null | undefined): string | null {
  if (!atRef) return null;
  const src = sourceLabel(row.source);
  if (lane === "file_at_ref") {
    return `file at ref: ${src}`;
  }
  if (!row.ref_aware) {
    return `${lane}: answers the checkout (${src})`;
  }
  if (row.source === "refused" || row.off_head === "refused") {
    return `${lane}: refused — ${row.why}`;
  }
  return `${lane} at ref: ${src} (ceiling ${row.off_head})`;
}
