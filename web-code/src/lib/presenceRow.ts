// OL PRES3 - how one tree row wears its presence mark. Pure, so the exact
// attributes, aria text and hue are pinned in vitest without mounting the
// virtualised tree (a server render of it has no scroll element, hence no
// rows).

import type { DirRollup, PresenceMark, Trust } from "./presence";
import { provHueStyle } from "./provHue";

export interface RowPresence {
  className: string;
  style: Record<string, string>;
  label: string;
  level: number;
  trust: Trust;
  /// Only for a non-neutral trust: a neutral mark claims no session.
  sid: string | undefined;
  /// Set for a collapsed directory's roll-up only.
  count: number | undefined;
}

/// `file` is the row's own mark; `dir` is its roll-up, honoured ONLY while the
/// directory is collapsed (an expanded one shows nothing extra: its children
/// carry their own marks).
export function rowPresence(
  file: PresenceMark | undefined,
  dir: DirRollup | undefined,
  dirOpen: boolean,
): RowPresence | null {
  const m = file ?? (dir && !dirOpen ? dir : undefined);
  if (!m) return null;
  const sid = m.trust !== "neutral" ? m.sessionId : undefined;
  return {
    className: `kbc-tree__row--presence kbc-tree__row--presence-${m.trust}`,
    style: sid ? provHueStyle(sid) : {},
    label: m.label,
    level: m.level,
    trust: m.trust,
    sid,
    count: file ? undefined : (dir as DirRollup).count,
  };
}
