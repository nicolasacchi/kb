// T15 - decode one kitty Unicode placeholder cell. Pure: no DOM, no xterm.
//
// A placeholder cell is `U+10EEEE` followed by up to three combining marks
// from the kitty `rowcolumn-diacritics` table (`diacritics.ts`):
//
//   1st mark -> row, 2nd -> column, 3rd -> the HIGH BYTE of the image id.
//
// The low 24 bits of the id ride in the cell's foreground colour (true
// colour), or the low 8 bits when the foreground is a palette index. A
// cell with no marks continues the previous cell on the line (same id:
// same row, next column); a cell with a row mark but no column mark is the
// next column of that row when the previous cell has the same id and row.

import { DIAC_INDEX, PH } from "./diacritics";

/** A decoded placeholder cell: which picture, and which cell of it. */
export interface Cell {
  id: number;
  row: number;
  col: number;
}

/**
 * Decode `chars` (the cell's full grapheme string). Returns `null` when
 * the first code point is not the placeholder, so the caller can break its
 * "previous cell" chain.
 *
 * @param fgIsRGB  the cell's foreground is a 24-bit colour
 * @param fgIsPal  the cell's foreground is a palette index
 * @param fg       the foreground value (`getFgColor()`)
 * @param prev     the previous placeholder cell on this line, or null
 */
export function decodePlaceholder(
  chars: string,
  fgIsRGB: boolean,
  fgIsPal: boolean,
  fg: number,
  prev: Cell | null,
): Cell | null {
  const cps = Array.from(chars, (c) => c.codePointAt(0) as number);
  if (cps[0] !== PH) return null;
  let id = fgIsRGB ? fg & 0xffffff : fgIsPal ? fg & 0xff : 0;
  let row = DIAC_INDEX.get(cps[1]) ?? -1;
  let col = DIAC_INDEX.get(cps[2]) ?? -1;
  const high = DIAC_INDEX.get(cps[3]);
  // `>>> 0` keeps the id an unsigned 32-bit value when the high byte is >= 128.
  if (high !== undefined) id = (id | (high << 24)) >>> 0;
  if (row < 0) {
    // No marks at all: continue the previous cell when the colour matches.
    if (prev !== null && prev.id === id) {
      row = prev.row;
      col = prev.col + 1;
    } else {
      row = 0;
      col = 0;
    }
  } else if (col < 0) {
    col = prev !== null && prev.id === id && prev.row === row ? prev.col + 1 : 0;
  }
  return { id, row, col };
}
