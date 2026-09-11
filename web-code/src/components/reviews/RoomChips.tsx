// V76-R2a — the Room's chip + section-decorator components. Pure renderers
// over `lib/reviewRoom.ts`'s mappings: no component here picks a colour or
// an icon itself (the module doc there states the rule). Every chip sets
// `--kbc-room-chip-color` from its ONE token so the wash, the border and
// the glyph can never disagree (`AgentVerdictCard`'s own one-token trick).
import type { FindingSeverity } from "../../api/types";
import { Icon } from "../icons";
import {
  actChip,
  categoryChip,
  sectionDecor,
  severityChip,
  type ChipSpec,
  type RoomSectionKind,
} from "../../lib/reviewRoom";

/// Resolve an icon NAME to its component. The golden test
/// (`lib/reviewRoom.test.ts`) proves every mapped name is a real key, so
/// the fallback is for an icon set edited AFTER the mapping — degrade to a
/// dot, never a crash.
function Glyph({ name }: { name: string }) {
  const C = (Icon as Record<string, React.ComponentType | undefined>)[name] ?? Icon.Dot;
  return <C aria-hidden="true" />;
}

function chipStyle(spec: ChipSpec): React.CSSProperties {
  return { ["--kbc-room-chip-color" as string]: `var(${spec.token})` };
}

export function SeverityChip({ severity }: { severity: FindingSeverity }) {
  const spec = severityChip(severity);
  return (
    <span
      className="kbc-room-chip"
      style={chipStyle(spec)}
      data-kbc-room-chip={`severity:${severity}`}
      title={spec.title}
    >
      <Glyph name={spec.icon} /> {severity}
    </span>
  );
}

export function ActChip({ act }: { act: string | undefined }) {
  const label = act ?? "issue";
  const spec = actChip(act);
  return (
    <span
      className="kbc-room-chip"
      style={chipStyle(spec)}
      data-kbc-room-chip={`act:${label}`}
      title={spec.title}
    >
      <Glyph name={spec.icon} /> {label}
    </span>
  );
}

export function CategoryChip({ category }: { category: string }) {
  const spec = categoryChip(category);
  return (
    <span
      className="kbc-room-chip"
      style={chipStyle(spec)}
      data-kbc-room-chip={`category:${category}`}
      title={spec.title}
    >
      <Glyph name={spec.icon} /> {category}
    </span>
  );
}

/// A section divider with the kind's icon + colour token. Renders as a real
/// heading (`h2`) with a `data-kbc-room-section` anchor — the
/// `review.jump.*` command rows scroll to these, so the attribute is a
/// contract, not a hook for styling alone.
export function SectionDecor({
  kind,
  title,
  count,
}: {
  kind: RoomSectionKind;
  title?: string;
  count?: number;
}) {
  const decor = sectionDecor(kind);
  return (
    <h2
      className="kbc-room-section"
      style={{ ["--kbc-room-section-color" as string]: `var(${decor.token})` }}
      data-kbc-room-section={kind}
    >
      <span className="kbc-room-section__glyph" aria-hidden="true">
        <Glyph name={decor.icon} />
      </span>
      <span className="kbc-room-section__title">{title ?? decor.label}</span>
      {count != null && <span className="kbc-room-section__n">{count}</span>}
    </h2>
  );
}
