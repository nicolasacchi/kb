import type { ComponentType, SVGProps } from "react";
import type { NavHrefCtx, ResolvedNavItem } from "./navItems";

export type NavCommand = {
  id: string;
  label: string;
  hint: string;
  kbd?: string;
  glyph: ComponentType<SVGProps<SVGSVGElement>>;
  run: (deps: {
    navigate: (to: string) => void;
    onClose: () => void;
    kb: NavHrefCtx["kb"];
  }) => void;
};

/// One Cmd+K row per resolved door that declares `cmdk`. Hidden doors never
/// reach here (the resolver already dropped them).
export function navCommands(items: ResolvedNavItem[]): NavCommand[] {
  const out: NavCommand[] = [];
  for (const item of items) {
    if (!item.cmdk) continue;
    out.push({
      id: `go-nav-${item.id}`,
      label: item.cmdk.label,
      hint: item.cmdk.hint,
      kbd: item.cmdk.kbd,
      glyph: item.Glyph as NavCommand["glyph"],
      run: ({ navigate, onClose, kb }) => {
        navigate(item.href({ kb }));
        onClose();
      },
    });
  }
  return out;
}
