// T15 - the graphics layer's public surface. Imported ONLY from the lazy
// terminal chunk (`TerminalPanel.tsx`); see `lazy.test.ts`.
import "./lightbox.css";
export { attachGfx, TAP_MAX_MOVE_PX, TAP_MAX_MS } from "./attach";
export type { AttachGfxOptions, GfxHandle, OpenPicture } from "./attach";
export { MediaCache } from "./media";
