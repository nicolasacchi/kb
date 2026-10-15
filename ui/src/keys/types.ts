// Registry-agnostic shapes for the which-key overlay and the keyboard help
// sheet. Nothing here knows which app's keymap fed it: an adapter maps its own
// registry onto these rows and hands them to the shared components.

export type KeyRow = {
  /** The key sequence to press, one token per element (`["g", "d"]`). Empty = no key in this profile. */
  keys: string[];
  /** What the sequence does. */
  verb: string;
  scope?: string;
  /** Heading the row is listed under. */
  group?: string;
  /** Stable id of the underlying command; rendered as `data-kbc-cmd` in the help sheet. */
  id?: string;
  /** The row is declared but not built yet (help sheet: dimmed, tagged "planned"). */
  planned?: boolean;
  /** The row stands for a group of further keys (which-key: italic, "N more..."). */
  isGroup?: boolean;
};

export type KeyScopeView = {
  /** The chord typed so far, one element per token. */
  prefix: string[];
  /** Next-key continuations for that prefix, in display order. */
  rows: KeyRow[];
  /** Name of the surface the continuations belong to (which-key header). */
  scope?: string;
};

export type KeyProfile = { id: string; title: string };
