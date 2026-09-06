import SlateCard, { type SlateCardActions } from "./SlateCard";
import type { SlateBoardCard } from "../../api/slateTypes";

// One board column (§10 "Accessibility": columns are `role="region"` WITH
// the section name, so a screen reader can jump between HANDS / ASKS /
// TAKES / FOUND+IDEA / TRIED the way a sighted reader's eye does).
//
// On MOBILE the same component renders as an accordion with its count in
// the summary — one column, sections collapsed, exactly as §10 says. The
// desktop and mobile shapes share this file so the two can't grow different
// ordering: `cards` arrives already ordered by `slateLanes.orderCards`.

type Props = {
  id: string;
  name: string;
  cards: SlateBoardCard[];
  codeUrl?: string | null;
  /// The kb the `code_url` belongs to (D29's caption query is kb-scoped).
  kb?: string | null;
  /// SL7f (v0.42 amendment) — the board's own slug, forwarded to every
  /// `SlateCard` as the caption query's `?repo=`.
  repo?: string | null;
  mobile?: boolean;
  isOperator?: boolean;
  focusedSeq?: number | null;
  flashSeq?: number | null;
  actions?: SlateCardActions;
  /// Mobile accordion open state (desktop ignores it).
  open?: boolean;
  onToggle?: () => void;
};

export default function SlateColumn({
  id,
  name,
  cards,
  codeUrl,
  kb,
  repo,
  mobile = false,
  isOperator = false,
  focusedSeq = null,
  flashSeq = null,
  actions,
  open = true,
  onToggle,
}: Props) {
  const body = (
    <div className="slate-col__cards">
      {cards.length === 0 ? (
        <p className="slate-col__empty">nothing here</p>
      ) : (
        cards.map((c) => (
          <SlateCard
            key={c.seq}
            card={c}
            codeUrl={codeUrl}
            kb={kb}
            repo={repo}
            mobile={mobile}
            isOperator={isOperator}
            focused={focusedSeq === c.seq}
            flashing={flashSeq === c.seq}
            actions={actions}
          />
        ))
      )}
    </div>
  );

  if (mobile) {
    return (
      <section
        className="slate-col slate-col--acc"
        role="region"
        aria-label={name}
        data-col={id}
      >
        <h2 className="slate-col__head">
          <button
            type="button"
            className="slate-col__accbtn"
            aria-expanded={open}
            aria-controls={`slate-col-${id}`}
            onClick={onToggle}
          >
            <span className="slate-col__name">{name}</span>
            <span className="slate-col__count">{cards.length}</span>
          </button>
        </h2>
        <div id={`slate-col-${id}`} hidden={!open}>
          {body}
        </div>
      </section>
    );
  }

  return (
    <section className="slate-col" role="region" aria-label={name} data-col={id}>
      <h2 className="slate-col__head">
        <span className="slate-col__name">{name}</span>
        <span className="slate-col__count">{cards.length}</span>
      </h2>
      {body}
    </section>
  );
}
