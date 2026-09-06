import { useKbs } from "./useKbs";
import { validCodeUrl } from "../lib/linkifyCitations";

// CT-B5 — resolve a validated `[kb.*] code_url` for a given kb name off the
// already-warm `["kbs"]` cache (the same source + `new URL()` validation
// `PreviewInspector.tsx`'s Code section already does — W1.D.R #11), so every
// citation-linkifying call site (memory rows, comment bodies, session
// decisions) shares one find+validate instead of re-implementing it. `kb`
// absent (e.g. a bare Preview with no artifact context) ⇒ `null`, same as an
// unlinked kb.
export function useCodeUrlForKb(kb: string | undefined): string | null {
  const { data: kbs } = useKbs();
  if (!kb) return null;
  const ctx = kbs?.find((k) => k.name === kb);
  return validCodeUrl(ctx?.code_url);
}
