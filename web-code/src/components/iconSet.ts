import { createIconSet } from "@kb/ui/icons";

// kb-code's icon set: the shared `@kb/ui/icons` glyphs with the three default
// render sizes kb-code has always used for names whose kb default differs
// (Search 16 vs 14, Folder 16 vs 14, X 12 vs 14), so existing call sites keep
// their rendered size.
export const Icon = createIconSet({ Search: 16, Folder: 16, X: 12 });
