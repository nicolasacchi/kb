import { Fragment } from "react";

// FolderBreadcrumb — renders the active folder path as a clickable trail
// in the gallery header: `recent › ideas › jira-to-github-projects`.
// `recent` clears the folder filter; each ancestor segment jumps to that
// folder; the last segment is the current location (not a link).

type Props = {
  /// Active folder path, e.g. "ideas/jira-to-github-projects".
  path: string;
  /// Navigate to `folder`, or `null` to clear the folder filter.
  onNavigate: (folder: string | null) => void;
};

export default function FolderBreadcrumb({ path, onNavigate }: Props) {
  const segments = path.split("/").filter(Boolean);
  return (
    <>
      <button
        type="button"
        className="gallery-crumb"
        onClick={() => onNavigate(null)}
      >
        recent
      </button>
      {segments.map((seg, i) => {
        const isLast = i === segments.length - 1;
        const cumulative = segments.slice(0, i + 1).join("/");
        return (
          <Fragment key={cumulative}>
            <span className="gallery-crumb-sep" aria-hidden="true">
              {" › "}
            </span>
            {isLast ? (
              <span className="gallery-h1-accent">{seg}</span>
            ) : (
              <button
                type="button"
                className="gallery-crumb"
                onClick={() => onNavigate(cumulative)}
              >
                {seg}
              </button>
            )}
          </Fragment>
        );
      })}
    </>
  );
}
