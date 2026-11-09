// kb-term tap: xterm attribute data -> SGR parameter string.
//
// Pure function. The returned string is the parameter list only (no ESC[ ]m
// wrapper), always starting with `0` (full reset), so replaying it after a
// CSI/ESC prefix reproduces the exact attribute data in a fresh xterm
// terminal. A null attribute (a DECSC slot that was never saved) encodes to
// the default reset `0`.
//
// xterm 6.0.0 attribute encoding (IAttributeData): `fg`/`bg` are 32-bit
// words. Low 24 bits hold the colour value (palette index for P16/P256 or
// packed RGB); bits 24-25 hold the colour mode (0 default, 1 P16, 2 P256,
// 3 RGB; mask 0x3000000). The remaining bits are flags: in `fg`,
// INVERSE=0x4000000 BOLD=0x8000000 UNDERLINE=0x10000000 BLINK=0x20000000
// INVISIBLE=0x40000000 STRIKETHROUGH=0x80000000; in `bg`, ITALIC=0x4000000
// DIM=0x8000000 HAS_EXTENDED=0x10000000 (internal, never emitted) PROTECTED
// =0x20000000 OVERLINE=0x40000000. Extended underline style (0..5) and
// colour live in the `extended` attrs, the colour in the same encoded form;
// the colour value 0x3FFFFFF is xterm's "default" sentinel (written by
// SGR 59, which stores -1 masked to the colour bits) and is treated as
// default here. P16 colours are emitted as 30-37/90-97 (40-47/100-107) so
// the replayed word gains CM_P16 rather than CM_P256; 38;5;n only ever
// produces CM_P256 in xterm, which would not compare equal.
export interface AttrCopy {
  fg: number;
  bg: number;
  underlineStyle: number;
  underlineColor: number;
}

const CM_MASK = 0x3000000;
const CM_P16 = 0x1000000;
const CM_P256 = 0x2000000;
const CM_RGB = 0x3000000;

const FG_INVERSE = 0x4000000;
const FG_BOLD = 0x8000000;
const FG_UNDERLINE = 0x10000000;
const FG_BLINK = 0x20000000;
const FG_INVISIBLE = 0x40000000;
const FG_STRIKETHROUGH = 0x80000000;

const BG_ITALIC = 0x4000000;
const BG_DIM = 0x8000000;
const BG_OVERLINE = 0x40000000;

const DEFAULT_UL_COLOR = 0x3ffffff;

/** Colour params for one word (`38`/`48` family), or null when default. */
function colorParams(word: number, kind: 'fg' | 'bg'): string | null {
  const mode = word & CM_MASK;
  if (mode === 0) return null;
  if (mode === CM_P16) {
    const idx = word & 0xff;
    if (kind === 'fg') return idx < 8 ? String(30 + idx) : String(90 + idx - 8);
    return idx < 8 ? String(40 + idx) : String(100 + idx - 8);
  }
  if (mode === CM_P256) return `${kind === 'fg' ? 38 : 48};5;${word & 0xff}`;
  const v = word & 0xffffff;
  return `${kind === 'fg' ? 38 : 48};2;${(v >>> 16) & 255};${(v >>> 8) & 255};${v & 255}`;
}

/** SGR parameter list (starts with `0`) reproducing the attribute exactly. */
export function sgrFromAttr(a: AttrCopy | null): string {
  const fg = a ? a.fg >>> 0 : 0;
  const bg = a ? a.bg >>> 0 : 0;
  const style = a ? a.underlineStyle : 0;
  const ulColor = a ? a.underlineColor >>> 0 : 0;
  const params: string[] = ['0'];
  if (fg & FG_BOLD) params.push('1');
  if (bg & BG_DIM) params.push('2');
  if (bg & BG_ITALIC) params.push('3');
  if (style !== 0) params.push(`4:${style}`);
  else if (fg & FG_UNDERLINE) params.push('4:1'); // flag set, style stored NONE
  if (fg & FG_BLINK) params.push('5');
  if (fg & FG_INVERSE) params.push('7');
  if (fg & FG_INVISIBLE) params.push('8');
  if (fg & FG_STRIKETHROUGH) params.push('9');
  if (bg & BG_OVERLINE) params.push('53');
  const fgC = colorParams(fg, 'fg');
  if (fgC) params.push(fgC);
  const bgC = colorParams(bg, 'bg');
  if (bgC) params.push(bgC);
  if (ulColor !== 0 && ulColor !== DEFAULT_UL_COLOR) {
    const mode = ulColor & CM_MASK;
    if (mode === CM_RGB) {
      const v = ulColor & 0xffffff;
      params.push(`58:2::${(v >>> 16) & 255}:${(v >>> 8) & 255}:${v & 255}`);
    } else if (mode === CM_P16 || mode === CM_P256) {
      params.push(`58:5:${ulColor & 0xff}`);
    }
    // CM default with stray bits: not expressible, skip (matches getUnderlineColor).
  }
  return params.join(';');
}
