// OL PRES3 - the glyph a presence-marked tree row wears. The glyph itself is
// decorative; the label is real text (visually hidden) because the tree rows
// are role-less divs, where an aria-label is not announced. Trust is in the
// text too ("Probably ..." / "Edited by ..." / "Changed ..."), so keyboard and
// screen-reader users get the same claim as the rail style gives sighted ones.

import { Icon } from "./iconSet";

export function PresenceGlyph({ label, count }: { label: string; count?: number }) {
  return (
    <>
      <span className="kbc-tree__presence" aria-hidden>
        <Icon.Spark />
        {count !== undefined ? <span className="kbc-tree__presence-count">{count}</span> : null}
      </span>
      <span className="kbc-sr-only" data-kbc-presence-label>
        {label}
      </span>
    </>
  );
}
