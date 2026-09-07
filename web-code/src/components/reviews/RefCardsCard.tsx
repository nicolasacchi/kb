// The Document tab's REF LIST, in the Review Room's side panel (V73-K2b).
//
// Every `[[…]]` the document cites, in the daemon's own order, as a jump
// list — one row per card, each naming its state so the list reads as a
// census rather than as a table of contents. Clicking a row focuses that
// card in the document; `] r`/`[ r` (`doc.card-next`/`doc.card-prev`) step
// the same focus from the keyboard.
//
// The rows are `<button>`s and the list installs NO keydown handler of its
// own. That is deliberate: a focused panel may stop only the keys it handles
// (web-code/CLAUDE.md § Keyboard), and bare `j`/`k` in `review` scope already
// belong to the inbox's own declared rows — two homes for one keystroke is
// exactly what the registry exists to prevent. Stepping lives in the registry
// rows the cockpit registers, not in a second listener here.
import type { ReviewDocCard } from "../../api/types";
import { cardAddress, cardCensusText, cardCensus, cardStateLabel } from "../../lib/reviewDoc";

export interface RefCardsCardProps {
  cards: ReviewDocCard[];
  focusedRef: string | null;
  onFocus: (ref: string) => void;
}

export default function RefCardsCard({ cards, focusedRef, onFocus }: RefCardsCardProps) {
  if (cards.length === 0) return null;
  const census = cardCensus(cards);
  return (
    <section className="kbc-review__card kbc-refcards" data-kbc-refcards={cards.length}>
      <h3 className="kbc-review__card-title">Refs</h3>
      <p className="kbc-review__card-sub" data-kbc-refcards-census>
        {cardCensusText(census)}
      </p>
      <ul className="kbc-refcards__list">
        {cards.map((c) => (
          <li key={c.ref}>
            <button
              type="button"
              className={
                "kbc-refcards__row" + (focusedRef === c.ref ? " is-focused" : "")
              }
              onClick={() => onFocus(c.ref)}
              data-kbc-refcards-row={c.ref}
              data-kbc-refcards-state={c.state}
              title={c.caption}
            >
              <span className="kbc-refcards__scheme">{c.scheme}</span>
              <span className="kbc-refcards__addr">{cardAddress(c)}</span>
              <span className="kbc-refcards__state">{cardStateLabel(c)}</span>
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}
