// OL-SIBL1 - the TopBar's doc<->code crossing: from a code file to the kb doc
// that cites it, landing on the section when the claim carries one. Data is the
// SAME `useDocRefs` query CitedBy reads (one query key), the choice is the pure
// `siblingDocChip`. With no single destination the chip does not navigate: it
// asks CitedBy (the always-visible rail strip) to expand.

import { CrossingChip } from "@kb/ui/CrossingChip";
import { useDocRefs } from "../../hooks/useDocRefs";
import { CITEDBY_OPEN_EVENT, siblingDocChip, type SiblingDocChipModel } from "../../lib/citedBy";
import { Icon } from "../iconSet";

export function useSiblingDocChip(
  repo: string | undefined,
  path: string | undefined,
  line: number | null,
): SiblingDocChipModel | null {
  const { data } = useDocRefs(repo, path);
  if (!data) return null;
  return siblingDocChip(data.claims, data.live, line);
}

export default function SiblingDocChip({ model }: { model: SiblingDocChipModel }) {
  const common = {
    icon: <Icon.Bridge />,
    label: model.label,
    count: model.count,
    tone: model.tone,
    testId: "sibling-doc-chip",
  } as const;
  if (model.kind === "link" && model.href) {
    return <CrossingChip {...common} href={model.href} newTab title="Open the citing doc in kb" />;
  }
  return (
    <CrossingChip
      {...common}
      title="Show the citing docs"
      onPress={() => window.dispatchEvent(new CustomEvent(CITEDBY_OPEN_EVENT))}
    />
  );
}
