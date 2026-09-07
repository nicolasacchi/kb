import { Link } from "react-router-dom";
import TrustBadge from "../TrustBadge";
import { useAtomActions } from "../../hooks/useRails";
import { menuOrder } from "../../lib/actionOps";
import { actionsTargetOf, type RailsAtom } from "../../lib/railsAtoms";
import { fullSearchUrl } from "../../lib/searchLanes";

// V72-I2 — the Rails ATOM table, rendered inside the reader's hover card.
//
// One block per atom on the hovered line: what the lens saw, where it points,
// how much it claims, and the `kbc-actions/1` rows for the TARGET.
//
// TWO RULES IT ENFORCES ON SCREEN:
//
//   * **Nothing auto-navigates.** Every target is a `<Link>` the reader
//     clicks. `rails-lens/1` has no `exact` tier by construction, and D5's
//     rule is that candidate/likely never auto-jump — so this card offers,
//     it never moves.
//   * **The action rows are the SERVER's.** `menuOrder(out.groups)` is
//     rendered whole, in the daemon's own order, with each row's `cli` twin
//     and its `disabled_reason` — this card picks none of them and composes
//     none. There is deliberately no second EXECUTOR here either: `.`
//     (`action.panel`) at the target's own address is the one home for
//     running an action (root CLAUDE.md #30), and duplicating `runAction`'s
//     dispatch would be exactly the "two homes for one action" this app
//     keeps paying for.
export interface RailsAtomCardProps {
  repo: string;
  atoms: readonly RailsAtom[];
  gitRef?: string;
}

export default function RailsAtomCard({ repo, atoms, gitRef }: RailsAtomCardProps) {
  if (atoms.length === 0) return null;
  return (
    <div className="kbc-railsatoms" data-kbc-rails-atoms={atoms.length}>
      <div className="kbc-railsatoms__head">Rails</div>
      {atoms.map((atom) => (
        <RailsAtomRow key={atom.id} repo={repo} atom={atom} gitRef={gitRef} />
      ))}
      <p className="kbc-railsatoms__caption">
        rails-lens/1 convention edges — likely or candidate, never exact. Nothing here jumps on its
        own.
      </p>
    </div>
  );
}

function RailsAtomRow({ repo, atom, gitRef }: { repo: string; atom: RailsAtom; gitRef?: string }) {
  const target = actionsTargetOf(atom);
  const actions = useAtomActions(repo, target, gitRef);
  const rows = actions.data ? menuOrder(actions.data.groups) : [];
  return (
    <section className="kbc-railsatoms__atom" data-kbc-rails-atom={atom.kind}>
      <h4 className="kbc-railsatoms__kind">{atom.label}</h4>

      {atom.targets.length > 0 && (
        <ul className="kbc-railsatoms__targets">
          {atom.targets.map((t, i) => (
            <li key={`${t.label}:${i}`} className={t.trustClass} data-kbc-rails-atom-target={t.label}>
              {t.href ? (
                <Link className="kbc-railsatoms__target" to={t.href}>
                  {t.label}
                </Link>
              ) : (
                <span className="kbc-railsatoms__target is-unaddressed" title="this edge resolved to a symbol with no file — absent, not guessed">
                  {t.label}
                </span>
              )}
              {t.dstKind && <span className="kbc-railsatoms__dstkind">{t.dstKind}</span>}
              <TrustBadge cls={t.trust} />
            </li>
          ))}
        </ul>
      )}

      {atom.clause && (
        <Link
          className="kbc-railsatoms__search"
          to={fullSearchUrl(atom.clause, repo)}
          data-kbc-rails-atom-search={atom.clause}
        >
          search {atom.clause}
        </Link>
      )}

      {atom.note && (
        <p className="kbc-railsatoms__note" data-kbc-rails-atom-note>
          {atom.note}
        </p>
      )}

      {target === null && atom.source === "lens" && (
        <p className="kbc-railsatoms__note">
          no file target on this edge, so there are no actions to list for it
        </p>
      )}

      {rows.length > 0 && (
        <ul className="kbc-railsatoms__actions" data-kbc-rails-atom-actions={rows.length}>
          {rows.map((row) => (
            <li key={row.id} data-kbc-rails-atom-action={row.id} title={row.doc}>
              <span className="kbc-railsatoms__action-title">{row.title}</span>
              {row.cli && <code className="kbc-railsatoms__action-cli">{row.cli}</code>}
              {!row.enabled && row.disabled_reason && (
                <span className="kbc-railsatoms__action-off">{row.disabled_reason}</span>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
