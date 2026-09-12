import { useLocation, useSearchParams } from "react-router";
import { useCommands } from "../commands/CommandRoot";
import { isReaderFilePath } from "../lib/codeUrl";

/// V76-R3c — TopBar chip for the reader's `@ref`. Click opens the
/// typeahead (`reader.ref-typeahead`). Hidden off a file-reader URL.
export default function RefChip() {
  const { pathname } = useLocation();
  const [params] = useSearchParams();
  const bus = useCommands();
  const ref = params.get("ref");

  if (!isReaderFilePath(pathname)) return null;

  const label = ref ? `@${ref}` : "working tree";
  const cls = ref ? "exact" : null;

  return (
    <button
      type="button"
      className={"kbc-topbar__chip kbc-topbar__ref" + (ref ? " is-active" : "")}
      data-kbc-ref-chip
      title="Change ref (Space @)"
      onClick={() => bus.run("reader.ref-typeahead")}
    >
      {label}
      {cls && <span className="kbc-topbar__ref-class">{cls}</span>}
    </button>
  );
}
