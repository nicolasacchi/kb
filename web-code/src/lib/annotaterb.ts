// V72-I2 — the annotaterb `# == Schema Information` block, read from the
// file the reader already has.
//
// WHERE THIS SHOULD LIVE, AND WHY IT DOES NOT YET
// ------------------------------------------------
// `comments/1` (V72-J1) classifies a source comment's KIND, and a schema
// banner is exactly its `generated` bucket — `classify.rs`'s own
// `GENERATED_NEEDLES` leads with `"== schema information"`. So the right
// long-term source for "does this file carry an annotaterb block, and
// where" is `GET /api/comments/file`, server-side, with the daemon's own
// taxonomy and its own honesty basis.
//
// That route did NOT exist on this unit's base (`b982002`: no `/comments`
// route in `router.rs`), and the unit was told not to block on it — so the
// block is parsed CLIENT-SIDE out of the text `GET /api/file` already
// returned: no extra request, nothing persisted, recomputed per render, and
// captioned as a browser read rather than a daemon fact
// (`SCHEMA_SOURCE_CAPTION`).
//
// V72-J1 LANDED ON MAIN WHILE THIS UNIT WAS IN FLIGHT, so the switch is now
// a follow-up someone can actually do rather than a hypothetical:
//
// TODO(follow-up): take the block RANGE from the `generated` `CommentOut`
// that `GET /api/comments/file?repo=&path=` reports, and keep ONLY the
// column-table parse below (the daemon classifies runs, it does not read
// annotaterb's column grammar). Three things must move with it: the card's
// caption (a daemon classification and a client guess are different facts
// and the caption must not blur them), the honest degrade when that request
// fails or the file is past its blame/scan budget, and this file's tests,
// which currently pin the block-FINDING rules the daemon would take over.
// Switching mid-unit was declined deliberately: it is a new request, a new
// hook, a new caption and a new failure mode on a surface that was already
// green, which is a re-scope, not a fix.
//
// The parse is deliberately narrow. It recognises the banner annotaterb
// (and the older `annotate` gem) actually writes, and refuses everything
// else: an unrecognised line inside the block is kept VERBATIM as trailing
// text rather than guessed at, and a file with no banner returns `null`
// rather than an empty table.

/// One column row of the banner:
/// `#  state      :string           default("new"), not null`.
export interface SchemaColumn {
  name: string;
  type: string;
  /// Everything after the type, verbatim — `not null, primary key`,
  /// `default(0)`, an index note. Empty string when the banner carried none.
  modifiers: string;
  /// 1-based line in the file this row was read from, so the card can jump.
  line: number;
}

/// A block section after the column table (`Indexes`, `Foreign Keys`, …),
/// captured as raw lines rather than parsed: their shapes vary by adapter
/// and gem version, and rendering them verbatim is honest where a partial
/// parse would not be.
export interface SchemaSection {
  title: string;
  lines: string[];
}

export interface SchemaBlock {
  /// 1-based, inclusive — the `# == Schema Information` line.
  startLine: number;
  /// 1-based, inclusive — the last comment line of the block.
  endLine: number;
  /// From `# Table name: orders`, or `null` when the banner omitted it.
  tableName: string | null;
  columns: SchemaColumn[];
  sections: SchemaSection[];
  /// Comment lines inside the block that matched nothing above. Rendered as
  /// they are; their presence is also what stops the card claiming the
  /// column table is the WHOLE banner.
  unparsed: string[];
}

/// `# == Schema Information` — annotaterb's own header. `Schema Info` is the
/// older `annotate` spelling; both are accepted, nothing else is.
const HEADER_RE = /^\s*#\s*==\s*Schema Info(?:rmation)?\s*$/;
/// The trailer annotaterb can be configured to write; it closes the block.
const TRAILER_RE = /^\s*#\s*==\s*Schema Information Trailer\s*$/;
const TABLE_RE = /^\s*#\s*Table name:\s*(\S+)\s*$/;
/// A column row: two leading spaces after the `#` in every version this
/// parser has seen, then `name`, then `:type`, then free-form modifiers.
const COLUMN_RE = /^\s*#\s{2,}(\S+)\s+:(\S+)\s*(.*?)\s*$/;
/// A section heading inside the block — a closed set, because a generic
/// "capitalised word alone on a line" rule would swallow a column named
/// after one.
const SECTION_RE = /^\s*#\s*(Indexes|Foreign Keys|Check Constraints)\s*$/i;
const COMMENT_RE = /^\s*#/;

function stripComment(line: string): string {
  return line.replace(/^\s*#\s?/, "").trimEnd();
}

/// Parse the FIRST annotaterb banner in `content`, or `null` when there is
/// none. `null` is the card's own "this file is not annotated" answer — the
/// card renders nothing at all rather than an empty table (the
/// `DiagnosticsCard` "absent" idiom).
export function parseSchemaBlock(content: string): SchemaBlock | null {
  const lines = content.split("\n");
  let start = -1;
  for (let i = 0; i < lines.length; i++) {
    if (HEADER_RE.test(lines[i])) {
      start = i;
      break;
    }
  }
  if (start === -1) return null;

  let tableName: string | null = null;
  const columns: SchemaColumn[] = [];
  const sections: SchemaSection[] = [];
  const unparsed: string[] = [];
  let current: SchemaSection | null = null;
  let end = start;

  for (let i = start + 1; i < lines.length; i++) {
    const raw = lines[i];
    // The block is ONE contiguous comment run. A blank line or code ends it
    // — which is what annotaterb writes, and what keeps this from eating a
    // file whose second comment block happens to follow.
    if (!COMMENT_RE.test(raw)) break;
    end = i;
    if (TRAILER_RE.test(raw)) break;

    const section = SECTION_RE.exec(raw);
    if (section) {
      current = { title: section[1], lines: [] };
      sections.push(current);
      continue;
    }
    if (current) {
      const body = stripComment(raw);
      if (body !== "") current.lines.push(body);
      continue;
    }
    const table = TABLE_RE.exec(raw);
    if (table) {
      tableName = table[1];
      continue;
    }
    const col = COLUMN_RE.exec(raw);
    if (col) {
      columns.push({ name: col[1], type: col[2], modifiers: col[3], line: i + 1 });
      continue;
    }
    const body = stripComment(raw);
    if (body !== "") unparsed.push(body);
  }

  return { startLine: start + 1, endLine: end + 1, tableName, columns, sections, unparsed };
}

/// A one-line caption for the card head — the daemon says nothing about this
/// block, so the card must say where the facts came from.
export const SCHEMA_SOURCE_CAPTION =
  "read from this file's own annotaterb banner, in the browser — not a daemon fact, and not stored";

/// Should the buffer fold this block? Only a block with a real column table
/// is worth collapsing; a two-line banner would fold to something longer
/// than itself.
export function isFoldable(block: SchemaBlock | null): boolean {
  return block !== null && block.endLine - block.startLine >= 3;
}

/// The summary the fold's placeholder shows in place of the block.
export function foldLabel(block: SchemaBlock): string {
  const lines = block.endLine - block.startLine + 1;
  const cols = block.columns.length;
  const table = block.tableName === null ? "schema" : block.tableName;
  return `== Schema Information · ${table} · ${cols} column${cols === 1 ? "" : "s"} · ${lines} lines`;
}

/// Is `[startLine, endLine]` (1-based, inclusive) a range this document can
/// actually fold? Lives here rather than in `editor/schemaFold.ts` so it is
/// unit-testable in this app's DOM-free vitest environment — and the guard
/// matters more than the decoration does, because a range past the end of
/// the document throws inside CM6 instead of degrading.
export function foldRangeIsValid(totalLines: number, startLine: number, endLine: number): boolean {
  return (
    Number.isInteger(startLine) &&
    Number.isInteger(endLine) &&
    startLine >= 1 &&
    endLine >= startLine &&
    endLine <= totalLines
  );
}
