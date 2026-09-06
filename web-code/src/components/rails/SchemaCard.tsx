import { parseSchemaBlock, SCHEMA_SOURCE_CAPTION } from "../../lib/annotaterb";

// V72-I2 — the annotaterb overlay: a model file's `# == Schema Information`
// banner, as a compact column table in the inspector rail.
//
// DERIVED PER RENDER, PERSISTED NOWHERE. It takes the text `GET /api/file`
// already returned (no second request) and `lib/annotaterb.ts` parses it; a
// file with no banner renders NOTHING (the `DiagnosticsCard` "absent" idiom)
// rather than an empty table, because "not annotated" and "annotated with no
// columns" are different facts.
//
// The caption is not decoration either: these facts are a CLIENT parse of a
// comment, not a daemon classification, and the card says so. See
// `lib/annotaterb.ts`'s header for the `comments/1` TODO that will change
// that sentence.
export interface SchemaCardProps {
  /// The open file's text, or `undefined` while it loads / when it is not
  /// UTF-8. Absent ⇒ the card renders nothing.
  content: string | undefined;
  /// The browser-local fold PREF (`lib/prefs.ts`'s `schemaFold`, default
  /// ON), not this file's transient state: the button below is the opt-OUT
  /// door and applies to every annotated model. Unfolding just THIS file is
  /// the buffer placeholder's own click, which never touches the pref.
  foldEnabled?: boolean;
  onToggleFold?(): void;
  /// Jump the buffer to a column's own line.
  onJumpLine?(line: number): void;
}

export default function SchemaCard({ content, foldEnabled, onToggleFold, onJumpLine }: SchemaCardProps) {
  const block = content === undefined ? null : parseSchemaBlock(content);
  if (!block) return null;
  return (
    <div className="kbc-schema" data-kbc-schema>
      <div className="kbc-schema__head">
        <span className="kbc-schema__title">Schema</span>
        {block.tableName && (
          <code className="kbc-schema__table" data-kbc-schema-table>
            {block.tableName}
          </code>
        )}
        {onToggleFold && (
          <button
            type="button"
            className={"kbc-schema__fold" + (foldEnabled ? " is-on" : "")}
            aria-pressed={foldEnabled ?? false}
            title="Fold annotaterb schema banners in the buffer (applies to every annotated model)"
            data-kbc-schema-fold
            onClick={onToggleFold}
          >
            Fold banner
          </button>
        )}
      </div>

      {block.columns.length > 0 ? (
        <table className="kbc-schema__cols" data-kbc-schema-columns={block.columns.length}>
          <tbody>
            {block.columns.map((c) => (
              <tr key={`${c.name}:${c.line}`} data-kbc-schema-col={c.name}>
                <th scope="row">
                  <button
                    type="button"
                    className="kbc-schema__colname"
                    onClick={() => onJumpLine?.(c.line)}
                    data-kbc-schema-jump={c.line}
                  >
                    {c.name}
                  </button>
                </th>
                <td className="kbc-schema__type">{c.type}</td>
                <td className="kbc-schema__mods">{c.modifiers}</td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : (
        <p className="kbc-schema__absence" data-kbc-schema-absence>
          the banner is present but carries no column rows this parser recognises
        </p>
      )}

      {block.sections.map((s) => (
        <details className="kbc-schema__section" key={s.title} data-kbc-schema-section={s.title}>
          <summary>{s.title}</summary>
          <pre>{s.lines.join("\n")}</pre>
        </details>
      ))}

      {block.unparsed.length > 0 && (
        <details className="kbc-schema__section" data-kbc-schema-unparsed={block.unparsed.length}>
          <summary>{block.unparsed.length} more banner line(s)</summary>
          <pre>{block.unparsed.join("\n")}</pre>
        </details>
      )}

      <p className="kbc-schema__caption" data-kbc-schema-caption>
        {SCHEMA_SOURCE_CAPTION}
      </p>
    </div>
  );
}
