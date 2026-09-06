// V72-G1.2 — the Dossier rail tab: the member table as a JUMP LIST.
//
// Not a second member table. It renders the SAME `MemberRow[]` the center's
// table renders, through the SAME `lib/dossier.ts` comparator, so the rail and
// the center can never disagree about what the members are or what order they
// are in — the "one projection, two renderers" rule kbc-tree/1 states for the
// tree, applied to this pair. Everything it shows is a row the wire sent.
//
// KEYBOARD: `j`/`k` move the cursor and `Enter` opens, handled LOCALLY on the
// focused list rather than as `cmd/1` rows. That is deliberate and it follows
// the rail's own precedent — `rail` scope declares exactly one row today
// (`dismiss.sheet`), so there is no rail j/k convention in the registry to
// join, and inventing two global rows for a list that only exists inside one
// tab would be a wider claim than the feature makes. The list obeys the rule
// `web-code/CLAUDE.md` states for anything that takes focus: **it stops only
// the keys it HANDLES** (`RAIL_LIST_KEYS`), so a chord, the Space leader and
// every global command still reach `CommandRoot` from inside it — the
// `PeekPanel`/`peekPanelHandlesKey` fix, not the bug that preceded it.

import { useEffect, useMemo, useRef, useState } from "react";
import type { MemberRow } from "../../api/types";
import { sortMembers, type MemberSort } from "../../lib/dossier";
import TrustBadge from "../TrustBadge";

/// The ONLY keys this list consumes. Anything else bubbles — see the header.
const RAIL_LIST_KEYS: ReadonlySet<string> = new Set(["j", "k", "Enter", "ArrowDown", "ArrowUp"]);

export function railListHandlesKey(key: string): boolean {
  return RAIL_LIST_KEYS.has(key);
}

export interface DossierRailProps {
  fqn: string;
  members: MemberRow[];
  sort: MemberSort;
  /// Open a member's definition site in the reader.
  onOpen: (path: string, line?: number) => void;
}

export default function DossierRail({ fqn, members, sort, onOpen }: DossierRailProps) {
  const rows = useMemo(() => sortMembers(members, sort), [members, sort]);
  const [cursor, setCursor] = useState(0);
  const listRef = useRef<HTMLUListElement | null>(null);

  // A re-sort or a re-fetch can leave the cursor past the end; clamp rather
  // than letting `rows[cursor]` be `undefined` on the next Enter.
  useEffect(() => {
    setCursor((c) => (rows.length === 0 ? 0 : Math.min(c, rows.length - 1)));
  }, [rows.length]);

  if (rows.length === 0) {
    return (
      <p className="kbc-inspector__hint" data-kbc-dossier-rail-empty>
        no members to jump to.
      </p>
    );
  }

  return (
    <ul
      className="kbc-dossier-rail"
      data-kbc-dossier-rail={fqn}
      ref={listRef}
      tabIndex={0}
      role="listbox"
      aria-label={`${fqn} members`}
      onKeyDown={(e) => {
        if (!railListHandlesKey(e.key)) return;
        // Only now — the key is one this list owns.
        e.preventDefault();
        e.stopPropagation();
        if (e.key === "j" || e.key === "ArrowDown") setCursor((c) => Math.min(rows.length - 1, c + 1));
        else if (e.key === "k" || e.key === "ArrowUp") setCursor((c) => Math.max(0, c - 1));
        else if (e.key === "Enter") {
          const row = rows[cursor];
          if (row) onOpen(row.path, row.line);
        }
      }}
    >
      {rows.map((m, i) => (
        <li
          key={`${m.defining_type}#${m.name}@${m.path}:${m.line}`}
          className={"kbc-dossier-rail__row" + (i === cursor ? " is-cursor" : "")}
          role="option"
          aria-selected={i === cursor}
          data-kbc-dossier-rail-row={m.name}
        >
          <button
            type="button"
            className="kbc-dossier-rail__btn"
            onClick={() => {
              setCursor(i);
              onOpen(m.path, m.line);
            }}
            title={`${m.defining_type}#${m.name} — ${m.path}:${m.line}`}
          >
            <span className="kbc-dossier-rail__name">{m.name}</span>
            <span className="kbc-dossier-rail__kind">{m.kind}</span>
            {m.inherited && (
              <span className="kbc-dossier-rail__pill" data-kbc-dossier-rail-inherited>
                inherited
              </span>
            )}
            <TrustBadge cls={m.trust} />
          </button>
        </li>
      ))}
    </ul>
  );
}
