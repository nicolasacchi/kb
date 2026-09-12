import { useLocation } from "react-router";

// V70-A0 — pins the `?desk=` grammar one wave before its consumer (the
// Track A `A3` unit that lands the actual five-region Desk shell), mirroring
// `lib/codeUrl.ts`'s `pane2` precedent: the builder/parser ship now so the
// grammar is frozen early, but nothing reads or renders off it yet. See
// docs/research/kb-code-v7-continuum-2026-09.html §D1 ("`?pane2=` stays
// URL-pure … and gains `frame=`, `follow:`, `pin`") and §P1 ("Presets Read /
// Review / Explore / Present plus named desks").
//
// Grammar: `?desk=<preset>` where `<preset>` is one of the four named
// presets, or the literal `legacy` (D1's `?shell=legacy` escape hatch,
// folded into this one param rather than a second query key — one URL
// switch, not two). Anything else — an unknown preset, a typo, an empty
// value, the param absent — parses to `null`: "no override", never a thrown
// error and never a silent default substitution. Total, pure, no Router
// context needed (same discipline as `useActiveRepo.ts`'s `repoOf`).
export type DeskPreset = "read" | "review" | "explore" | "present";

const DESK_PRESETS: ReadonlySet<DeskPreset> = new Set(["read", "review", "explore", "present"]);

export function parseDeskParam(search: string): DeskPreset | "legacy" | null {
  const params = new URLSearchParams(search);
  const raw = params.get("desk");
  if (raw === null || raw === "") return null;
  if (raw === "legacy") return "legacy";
  if ((DESK_PRESETS as ReadonlySet<string>).has(raw)) return raw as DeskPreset;
  return null;
}

// Thin Router-context wrapper over the pure parser above — mirrors
// `useActiveRepo.ts`'s `useExplicitRepo`/`repoOf` split so the parsing logic
// stays independently unit-testable (`deskParam.test.ts`) while every
// component gets a one-line hook. No consumer exists yet (see this file's
// header doc); adding one is out of scope for V70-A0.
export function useDeskOverride(): DeskPreset | "legacy" | null {
  const loc = useLocation();
  return parseDeskParam(loc.search);
}
