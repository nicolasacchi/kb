import { Link } from "react-router-dom";
import { useBacklinks } from "../hooks/useNotes";
import { artifactHref } from "../lib/artifactHref";
import { Icon } from "./icons";

// "Linked from" — the inbound side of the wikilink graph. Shown under a note
// (what references this note) and on an artifact's detail (what notes/artifacts
// reference it). Renders nothing when there are no backlinks, so it never adds
// chrome to an unlinked artifact. Each row navigates to the linker's permalink
// (notes render natively in Detail, so a note backlink opens its editor).

type Props = {
  kb: string;
  id: string;
  /// Section heading — "Linked from" on a note, "Referenced in" on an artifact.
  heading?: string;
};

export default function BacklinksSection({ kb, id, heading = "Linked from" }: Props) {
  const { backlinks, loading } = useBacklinks(kb, id);
  if (loading || backlinks.length === 0) return null;
  return (
    <section className="kb-backlinks" aria-label={heading}>
      <h3 className="kb-backlinks__head">
        {heading}
        <span className="kb-backlinks__count">{backlinks.length}</span>
      </h3>
      <ul className="kb-backlinks__list">
        {backlinks.map((b) => (
          <li key={`${b.kb}:${b.id}`}>
            <Link
              className="kb-backlinks__item"
              to={artifactHref(b.kb, b.source_relative)}
              title={b.source_relative}
            >
              <span
                className="kb-backlinks__icon"
                aria-label={b.is_note ? "note" : "document"}
              >
                {b.is_note ? <Icon.Note /> : <Icon.Doc />}
              </span>
              <span className="kb-backlinks__title">{b.title}</span>
              {b.folder && <span className="kb-backlinks__folder">{b.folder}</span>}
            </Link>
          </li>
        ))}
      </ul>
    </section>
  );
}
