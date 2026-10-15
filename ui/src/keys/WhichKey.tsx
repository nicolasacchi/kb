import type { KeyRow, KeyScopeView } from "./types";
import "./keys.css";

export type WhichKeyProps = {
  view: KeyScopeView | null;
  /** Called with the row when a cell is clicked. The overlay is pointer-transparent by default; this only matters if a host enables pointer events. */
  onPick?: (row: KeyRow) => void;
  /** Maps a raw key token to what is printed on the keycap. */
  renderKey?: (token: string) => string;
};

const identity = (t: string) => t;

export function WhichKey({ view, onPick, renderKey = identity }: WhichKeyProps) {
  if (!view) return null;
  let lastGroup = "";
  return (
    <div className="kbc-whichkey" data-kbc-whichkey role="status" aria-live="polite">
      <div className="kbc-whichkey__head">
        <kbd className="kbc-whichkey__pending">{view.prefix.join(" ")}</kbd>
        {view.scope !== undefined && <span className="kbc-whichkey__scope">{view.scope}</span>}
        <span className="kbc-whichkey__esc">
          <kbd>Esc</kbd> cancel
        </span>
      </div>
      <div className="kbc-whichkey__rows">
        {view.rows.map((r) => {
          const group = r.group ?? "";
          const head = group !== lastGroup ? group : null;
          lastGroup = group;
          const token = r.keys[0] ?? "";
          return (
            <span
              key={token}
              className="kbc-whichkey__cell"
              data-kbc-whichkey-key={token}
              onClick={onPick ? () => onPick(r) : undefined}
            >
              {head && <span className="kbc-whichkey__group">{head}</span>}
              <kbd>{renderKey(token)}</kbd>
              <span className={"kbc-whichkey__label" + (r.isGroup ? " is-group" : "")}>{r.verb}</span>
            </span>
          );
        })}
      </div>
    </div>
  );
}

export default WhichKey;
