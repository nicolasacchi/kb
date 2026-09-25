// RS-U11 — the review header's base-policy chip (README §12): `tracking
// main · merge-base 7c1ed0c` for `track`/`local`, an amber `pinned
// 7c1ed0c` for `pin`, or `legacy` for a pre-upgrade row `classify_base`
// could not resolve to a policy — each with a Retrack affordance. Same
// "ONE token drives wash, border and glyph" chip primitive
// `components/reviews/RoomChips.tsx` uses (`.kbc-room-chip`,
// `--kbc-room-chip-color`), duplicated here in miniature rather than
// importing that file's private `Glyph`/`chipStyle` — this header isn't
// otherwise a Room-chip consumer, and the two token tables (base modes,
// warning codes) live in `lib/reviewBase.ts`, not `lib/reviewRoom.ts`.
import type { CSSProperties } from "react";
import type { BaseWarningOut, ReviewBaseOut } from "../../api/types";
import {
  BASE_WARNING_CHIP,
  FORGE_UNVERIFIED_CHIP,
  baseChipLabel,
  baseChipSpec,
  baseMergeBaseSuffix,
  baseNeedsRetrack,
  baseSourceLabel,
  retrackCommandLine,
  warningShortLabel,
  type BaseChipSpec,
} from "../../lib/reviewBase";
import { Icon } from "../icons";
import { toast } from "../../lib/toast";

function Glyph({ name }: { name: string }) {
  const C = (Icon as Record<string, React.ComponentType<{ "aria-hidden"?: boolean }> | undefined>)[name] ?? Icon.Dot;
  return <C aria-hidden={true} />;
}

function chipStyle(spec: BaseChipSpec): CSSProperties {
  return { ["--kbc-room-chip-color" as string]: `var(${spec.token})` };
}

function copy(text: string, what: string) {
  navigator.clipboard.writeText(text).then(
    () => toast.ok(`${what} copied`),
    () => toast.err(`couldn't copy the ${what}`),
  );
}

export interface BaseChipProps {
  reviewId: number;
  base: ReviewBaseOut;
}

/// RS-U7's own `POST /api/reviews/{id}/retrack` route hasn't shipped on
/// this build (README §12's CLI table, §13) — this button COPIES the CLI
/// line rather than calling a route that would 404, the same posture
/// `lib/reviewDoc.ts`'s `composeCommandLine` documents for D22's
/// loopback-only authoring.
///
/// TODO(RS-U7): once the retrack HTTP route ships, call it directly here
/// (with a `--dry-run` toggle mirroring the CLI flag) instead of only
/// copying the line.
function RetrackButton({ reviewId, spec }: { reviewId: number; spec: BaseChipSpec }) {
  const line = retrackCommandLine(reviewId);
  return (
    <button
      type="button"
      className="kbc-room-chip kbc-room-chip--btn"
      style={chipStyle(spec)}
      title={`Retrack isn't a live action yet (RS-U7) — copies the CLI line: ${line}`}
      onClick={() => copy(line, "retrack command")}
      data-kbc-review-retrack
    >
      <Icon.Copy /> Retrack
    </button>
  );
}

export default function BaseChip({ reviewId, base }: BaseChipProps) {
  const spec = baseChipSpec(base);
  const label = baseChipLabel(base);
  const mergeBaseSuffix = baseMergeBaseSuffix(base);
  const sourceTitle = baseSourceLabel(base.source) ?? undefined;

  return (
    <span className="kbc-review__base" data-kbc-review-base data-kbc-review-base-mode={base.mode ?? "legacy"}>
      <span className="kbc-room-chip" style={chipStyle(spec)} title={sourceTitle} data-kbc-review-base-chip>
        <Glyph name={spec.icon} /> {label}
      </span>
      {mergeBaseSuffix && (
        <span className="kbc-review__base-suffix" data-kbc-review-base-mergebase>
          {mergeBaseSuffix}
        </span>
      )}
      {baseNeedsRetrack(base) && <RetrackButton reviewId={reviewId} spec={spec} />}
    </span>
  );
}

/// D8: GitHub is the only live-verified forge for Phase 1; every other
/// forge ships `forge-unverified` (a STORE-level fact, so the caller
/// reads it off `useReviewStoreCard` and renders nothing while that's
/// still loading or the repo has no store row).
export function ForgeUnverifiedChip() {
  return (
    <span
      className="kbc-room-chip"
      style={chipStyle(FORGE_UNVERIFIED_CHIP)}
      title="This review's forge isn't GitHub — base tracking follows the same GitHub semantics but hasn't been live-verified against this forge yet (README D8)."
      data-kbc-review-forge-unverified
    >
      <Glyph name={FORGE_UNVERIFIED_CHIP.icon} /> forge-unverified
    </span>
  );
}

export function BaseWarningChips({ warnings }: { warnings: BaseWarningOut[] }) {
  if (warnings.length === 0) return null;
  return (
    <>
      {warnings.map((w) => (
        <span
          key={w.code}
          className="kbc-room-chip"
          style={chipStyle(BASE_WARNING_CHIP)}
          title={w.message}
          data-kbc-review-base-warning={w.code}
        >
          <Glyph name={BASE_WARNING_CHIP.icon} /> {warningShortLabel(w.code)}
        </span>
      ))}
    </>
  );
}
