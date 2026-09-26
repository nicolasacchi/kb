// RS-U11 — golden-pins the base-policy / patchset-kind / warning-code →
// (token, icon) tables the same way `reviewRoom.test.ts` pins severity/
// act/category, plus the pure label/command-line builders.
import { describe, expect, it } from "vitest";
import type { ReviewBaseOut } from "../api/types";
import { Icon } from "../components/icons";
import {
  BASE_LEGACY_CHIP,
  BASE_MODE_CHIPS,
  BASE_WARNING_CHIP,
  FORGE_UNVERIFIED_CHIP,
  PATCHSET_KIND_CHIPS,
  baseChipLabel,
  baseChipSpec,
  baseMergeBaseSuffix,
  baseNeedsRetrack,
  baseSourceLabel,
  forgeUnverified,
  patchsetBaseShort,
  patchsetKindLabel,
  patchsetKindSpec,
  retrackCommandLine,
  warningChipSpec,
  warningShortLabel,
} from "./reviewBase";

function base(overrides: Partial<ReviewBaseOut> = {}): ReviewBaseOut {
  return {
    mode: "track",
    branch: "main",
    set_by: "auto",
    source: "forge-api",
    state: "ok",
    merge_base: "7c1ed0cfdd1234567890",
    fetched_at: 1000,
    last_fetch: "fetched",
    fetched_via: "gh-cli (nicolasacchi)",
    ...overrides,
  };
}

describe("baseChipSpec / baseChipLabel", () => {
  it("track/local read the neutral --blue tone", () => {
    expect(baseChipSpec(base({ mode: "track" }))).toEqual({ token: "--blue", icon: "Branch" });
    expect(baseChipSpec(base({ mode: "local" }))).toEqual({ token: "--blue", icon: "Branch" });
    expect(baseChipLabel(base({ mode: "track", branch: "main" }))).toBe("tracking main");
    expect(baseChipLabel(base({ mode: "local", branch: "feature-x" }))).toBe("local feature-x");
  });

  it("pin reads amber and its label IS the merge-base, short", () => {
    expect(baseChipSpec(base({ mode: "pin" }))).toEqual({ token: "--warn", icon: "Pin" });
    expect(baseChipLabel(base({ mode: "pin", merge_base: "7c1ed0cfdd1234567890" }))).toBe("pinned 7c1ed0c");
  });

  it("a pin with no merge-base yet degrades to a bare 'pinned'", () => {
    expect(baseChipLabel(base({ mode: "pin", merge_base: null }))).toBe("pinned");
  });

  it("a row with no resolved policy (mode absent) reads 'legacy', same amber tone as pin", () => {
    expect(baseChipSpec(base({ mode: null }))).toEqual(BASE_LEGACY_CHIP);
    expect(baseChipSpec(base({ mode: null })).token).toBe("--warn");
    expect(baseChipLabel(base({ mode: null }))).toBe("legacy");
  });

  it("an unknown future mode degrades to the legacy tone rather than crashing", () => {
    expect(baseChipSpec(base({ mode: "something-new" }))).toEqual(BASE_LEGACY_CHIP);
  });
});

describe("baseMergeBaseSuffix", () => {
  it("shows for track/local when a merge-base is captured", () => {
    expect(baseMergeBaseSuffix(base({ mode: "track", merge_base: "7c1ed0cfdd1234567890" }))).toBe(
      "merge-base 7c1ed0c",
    );
    expect(baseMergeBaseSuffix(base({ mode: "local", merge_base: "7c1ed0cfdd1234567890" }))).toBe(
      "merge-base 7c1ed0c",
    );
  });

  it("is absent for pin — the label already IS the merge-base", () => {
    expect(baseMergeBaseSuffix(base({ mode: "pin", merge_base: "7c1ed0cfdd1234567890" }))).toBeNull();
  });

  it("is absent with no captured merge-base yet (no patchset)", () => {
    expect(baseMergeBaseSuffix(base({ mode: "track", merge_base: null }))).toBeNull();
  });
});

describe("baseSourceLabel", () => {
  it("mirrors BaseSource::label() (review_base.rs) for every known rung", () => {
    expect(baseSourceLabel("explicit")).toBe("explicit");
    expect(baseSourceLabel("forge-api")).toBe("forge API");
    expect(baseSourceLabel("caller")).toBe("caller");
    expect(baseSourceLabel("merge-ref")).toBe("merge ref");
    expect(baseSourceLabel("default-assumed")).toBe("default branch, assumed");
    expect(baseSourceLabel("upstream")).toBe("upstream");
    expect(baseSourceLabel("stack-parent")).toBe("stack parent");
    expect(baseSourceLabel("legacy")).toBe("legacy row");
  });

  it("degrades an unknown slug to dashes-as-spaces, never a guess", () => {
    expect(baseSourceLabel("a-future-rung")).toBe("a future rung");
  });

  it("is null when absent", () => {
    expect(baseSourceLabel(null)).toBeNull();
    expect(baseSourceLabel(undefined)).toBeNull();
  });
});

describe("baseNeedsRetrack", () => {
  it("offers Retrack for pin and for a fully unclassified (mode absent) row only", () => {
    expect(baseNeedsRetrack(base({ mode: "pin" }))).toBe(true);
    expect(baseNeedsRetrack(base({ mode: null }))).toBe(true);
    expect(baseNeedsRetrack(base({ mode: "track" }))).toBe(false);
    expect(baseNeedsRetrack(base({ mode: "local" }))).toBe(false);
  });
});

describe("retrackCommandLine", () => {
  it("matches the agent CLI table (README §12/§13)", () => {
    expect(retrackCommandLine(65)).toBe("kb-code review retrack 65 --dry-run");
    expect(retrackCommandLine(65, false)).toBe("kb-code review retrack 65");
  });
});

describe("warningShortLabel / warningChipSpec", () => {
  it("turns a kebab-case code into a short label", () => {
    expect(warningShortLabel("base-pinned")).toBe("base pinned");
    expect(warningShortLabel("credential-account-mismatch")).toBe("credential account mismatch");
    expect(warningShortLabel("pr-target-assumed")).toBe("pr target assumed");
  });

  it("every warning shares the same tone — the wire carries no severity axis", () => {
    expect(BASE_WARNING_CHIP).toEqual({ token: "--warn", icon: "Warn" });
    for (const code of [
      // the base model's own (`review_base.rs::warn`)
      "base-pinned",
      "pr-target-assumed",
      "base-upgraded",
      "base-vanished",
      "credential-account-mismatch",
      "stale-mirror",
      // RS-U10b's `review sync`/`review status` codes (`review_sync.rs::
      // sync_warn`) — pass through the SAME `BaseWarningOut` shape, so
      // nothing here needed to change for them to render correctly.
      "forge-unavailable",
      "base-ignored",
      "pr-closed",
      "would-reopen",
      "fetch-unavailable",
      "fetch-failed",
    ]) {
      expect(warningChipSpec({ code })).toEqual(BASE_WARNING_CHIP);
      expect(warningShortLabel(code)).toBe(code.replace(/-/g, " "));
    }
  });
});

describe("forgeUnverified", () => {
  it("mirrors the daemon's own doctor predicate (forge_verified != verified && forge_kind known)", () => {
    expect(forgeUnverified({ forge_verified: "unverified", forge_kind: "gitlab" })).toBe(true);
    expect(forgeUnverified({ forge_verified: "verified", forge_kind: "github" })).toBe(false);
    expect(forgeUnverified({ forge_verified: "unverified", forge_kind: null })).toBe(false);
    expect(forgeUnverified(null)).toBe(false);
    expect(forgeUnverified(undefined)).toBe(false);
  });
});

describe("patchset kind", () => {
  it("every known kind maps to a spec; an unknown/absent kind degrades to neutral", () => {
    expect(patchsetKindSpec("push")).toEqual({ token: "--ink-mute", icon: "Dot" });
    expect(patchsetKindSpec("rebase")).toEqual({ token: "--blue", icon: "Swap" });
    expect(patchsetKindSpec("base-moved")).toEqual({ token: "--warn", icon: "Branch" });
    expect(patchsetKindSpec(null)).toEqual({ token: "--ink-mute", icon: "Dot" });
    expect(patchsetKindSpec("some-future-kind")).toEqual({ token: "--ink-mute", icon: "Dot" });
  });

  it("forced reads NEUTRAL — a --force re-mint of an unchanged pair is not a base-tracking anomaly (RS-U10b's own SyncReason doc: 'nothing moved')", () => {
    expect(patchsetKindSpec("forced")).toEqual({ token: "--ink-mute", icon: "Refresh" });
    expect(patchsetKindSpec("forced").token).not.toBe("--warn");
  });

  it("label is null for a legacy patchset (no badge), else dashes-as-spaces", () => {
    expect(patchsetKindLabel(null)).toBeNull();
    expect(patchsetKindLabel(undefined)).toBeNull();
    expect(patchsetKindLabel("base-corrected")).toBe("base corrected");
    expect(patchsetKindLabel("initial")).toBe("initial");
  });

  it("patchsetBaseShort prefers base_tip_sha, falls back to the merge-base", () => {
    expect(
      patchsetBaseShort({ base_tip_sha: "aaaaaaaaaa1234567890", base_sha_full: "b".repeat(40), base_sha: "bbbbbbb" }),
    ).toBe("aaaaaaa");
    expect(
      patchsetBaseShort({ base_tip_sha: null, base_sha_full: "cccccccccc1234567890", base_sha: "ccccccc" }),
    ).toBe("ccccccc");
  });
});

describe("every mapped icon name is a REAL key of the icon set", () => {
  it("base-mode, legacy, forge-unverified, warning and patchset-kind chips", () => {
    const names = new Set<string>();
    for (const spec of Object.values(BASE_MODE_CHIPS)) names.add(spec.icon);
    names.add(BASE_LEGACY_CHIP.icon);
    names.add(FORGE_UNVERIFIED_CHIP.icon);
    names.add(BASE_WARNING_CHIP.icon);
    for (const spec of Object.values(PATCHSET_KIND_CHIPS)) names.add(spec.icon);
    names.add("Dot"); // the patchset-kind fallback
    for (const n of names) {
      expect(Icon[n as keyof typeof Icon], `Icon.${n} does not exist`).toBeTruthy();
    }
  });
});
