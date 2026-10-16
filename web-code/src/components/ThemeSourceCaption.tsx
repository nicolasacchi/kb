import { themeSourceCaption } from "../lib/kbTheme";
import type { ThemeFamilySource } from "../lib/prefs";

/// OL S1 - the picker's one-line statement of where the family in force came
/// from. Renders nothing when there is nothing to say (no pick, no kb).
export default function ThemeSourceCaption({ source }: { source: ThemeFamilySource }) {
  const text = themeSourceCaption(source);
  if (!text) return null;
  return (
    <p className="kbc-themepicker__source" data-kbc-theme-source={source}>
      {text}
    </p>
  );
}
