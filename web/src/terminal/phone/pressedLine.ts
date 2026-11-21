// FUPHONELINKS — the buffer line under a long press, handed to the Read view
// that the press opens. A one-shot module slot: the scroller sets it just
// before it calls onLongPress, Read takes it when it mounts. A stale value
// (Read opened some other way) expires after a few seconds.

const TTL_MS = 3_000;
let slot: { line: number; at: number } | null = null;

export function setPressedLine(line: number | null): void {
  slot = line === null ? null : { line, at: Date.now() };
}

/** Take and clear the pressed line; null when there is none or it is stale. */
export function takePressedLine(): number | null {
  const s = slot;
  slot = null;
  return s && Date.now() - s.at < TTL_MS ? s.line : null;
}
