// OL-SIBL2 - the kb Code chip in the Spine `crossing` slot. Presentational:
// `useSiblingChip` owns the data and the hide rules. Destination-labelled
// (`-> coderefs.rs:120`); an `<a>` when the target is a kb-code URL, a
// `<button>` only when there is no single destination (opens the Links tab).

import { CrossingChip } from "@kb/ui/CrossingChip";
import { Icon } from "@kb/ui/icons";
import type { SiblingChipModel } from "../../hooks/useSiblingChip";

export default function SiblingChip({ model }: { model: SiblingChipModel }) {
  const common = {
    icon: <Icon.Bridge />,
    label: model.label,
    title: model.title,
    tone: model.tone,
    testId: "sibling-code-chip",
  } as const;
  const kindAttr = { "data-kb-sibling-kind": model.kind };
  const primary = model.primary;
  if (primary && primary.kind === "link") {
    return (
      <span {...kindAttr} style={{ display: "contents" }}>
        <CrossingChip {...common} href={primary.href} newTab />
      </span>
    );
  }
  return (
    <span {...kindAttr} style={{ display: "contents" }}>
      <CrossingChip {...common} onPress={model.openLinks} />
    </span>
  );
}
