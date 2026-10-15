import { useId, type ReactNode } from "react";
import { Icon } from "./icons";
import "./statecard.css";

export type StateKind = "loading" | "empty" | "refused" | "degraded" | "mismatch";

export type StateAction = {
  label: string;
  onClick?: () => void;
  href?: string;
  /** Stable handle for e2e: rendered as `data-kb-action`. */
  key?: string;
};

export type StateProgress = { done: number; total?: number; unit: string };

export type StateCardProps = {
  kind: StateKind;
  title: string;
  reason?: ReactNode;
  /** The CLI twin of this state, shown in a `<code>` with a copy button. */
  cli?: string;
  actions?: StateAction[];
  progress?: StateProgress;
  /** Overrides the default icon of the kind. */
  icon?: ReactNode;
  /** ui/ imports no router: the app passes its own link renderer. */
  renderLink?: (href: string, children: ReactNode) => ReactNode;
  headingLevel?: 1 | 2 | 3 | 4 | 5 | 6;
  /** "page" (default) is the centred card; "inline" is a left-aligned muted
   * line with no icon and no card chrome, for panels and rail sections. */
  size?: "page" | "inline";
};

function defaultIcon(kind: StateKind): ReactNode {
  switch (kind) {
    case "loading":
      return <Icon.Clock />;
    case "empty":
      return <Icon.Doc />;
    case "refused":
    case "degraded":
      return <Icon.Warn />;
    case "mismatch":
      return <Icon.Swap />;
  }
}

function copyCli(text: string) {
  if (typeof navigator === "undefined" || !navigator.clipboard) return;
  void navigator.clipboard.writeText(text).catch(() => {});
}

export function StateCard({
  kind,
  title,
  reason,
  cli,
  actions,
  progress,
  icon,
  renderLink,
  headingLevel = 2,
  size = "page",
}: StateCardProps) {
  const titleId = useId();
  const loading = kind === "loading";
  const inline = size === "inline";
  const cls = inline ? "kb-statecard kb-statecard--inline" : "kb-statecard";
  const H = `h${headingLevel}` as "h2";
  const body = (
    <>
      {!inline && (
        <div className="kb-statecard__icon" aria-hidden="true">
          {icon ?? defaultIcon(kind)}
        </div>
      )}
      {loading ? (
        <p className="kb-statecard__title">{title}</p>
      ) : (
        <H className="kb-statecard__title" id={titleId}>
          {title}
        </H>
      )}
      {reason && <p className="kb-statecard__reason">{reason}</p>}
      {progress && (
        <div className="kb-statecard__progress">
          <progress
            max={progress.total}
            value={progress.total === undefined ? undefined : progress.done}
          />
          <span className="kb-statecard__progress-text">
            {progress.total === undefined
              ? `${progress.done} ${progress.unit}`
              : `${progress.done} / ${progress.total} ${progress.unit}`}
          </span>
        </div>
      )}
      {actions && actions.length > 0 && (
        <div className="kb-statecard__actions">
          {actions.map((a, i) =>
            a.href !== undefined ? (
              renderLink ? (
                <span key={a.key ?? i} className="kb-statecard__action" data-kb-action={a.key}>
                  {renderLink(a.href, a.label)}
                </span>
              ) : (
                <a key={a.key ?? i} className="kb-statecard__action" href={a.href} data-kb-action={a.key}>
                  {a.label}
                </a>
              )
            ) : (
              <button
                key={a.key ?? i}
                type="button"
                className="kb-statecard__action"
                onClick={a.onClick}
                data-kb-action={a.key}
              >
                {a.label}
              </button>
            ),
          )}
        </div>
      )}
      {cli && (
        <div className="kb-statecard__cli">
          <code>{cli}</code>
          <button
            type="button"
            className="kb-statecard__copy"
            aria-label="Copy command"
            onClick={() => copyCli(cli)}
          >
            <Icon.Copy />
          </button>
        </div>
      )}
    </>
  );
  return loading ? (
    <div className={cls} data-kb-state-kind={kind} role="status" aria-live="polite">
      {body}
    </div>
  ) : (
    <section className={cls} data-kb-state-kind={kind} role="region" aria-labelledby={titleId}>
      {body}
    </section>
  );
}

export default StateCard;
