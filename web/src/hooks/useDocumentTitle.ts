import { useEffect } from "react";

const SUFFIX = "kb";

// Sets document.title to `<title> · kb`, or just "kb" when title is
// null/empty. Restores "kb" on unmount so a route that doesn't set a
// title can't leave a stale tab name behind.
export function useDocumentTitle(title: string | null | undefined): void {
  useEffect(() => {
    const t = title?.trim();
    document.title = t ? `${t} · ${SUFFIX}` : SUFFIX;
    return () => {
      document.title = SUFFIX;
    };
  }, [title]);
}
