// T18 — the phone Drive key vocabulary.
//
// The SPA sends key NAMES only; the runner maps each to fixed bytes
// (`crates/kb-term/src/runner/keys.rs`, `named_key_bytes`) and the relay
// refuses any name outside its own copy of that list
// (`crates/kb-server/src/terminal/keys_allow.rs`, `NAMED_KEYS`). Three
// copies of one closed list is the price of a closed list; `keys.test.ts`
// reads the Rust copy and fails if this one drifts.

/** Every key name the relay forwards, in the runner table's order. */
export const KEY_NAMES = [
  "Esc",
  "Tab",
  "S-Tab",
  "Enter",
  "Up",
  "Down",
  "Right",
  "Left",
  "C-c",
  "C-d",
  "C-u",
  "BSpace",
  "Slash",
  "1",
  "2",
  "3",
  "4",
  "5",
  "6",
  "7",
  "8",
  "9",
  "y",
  "n",
] as const;

export type KeyName = (typeof KEY_NAMES)[number];

export function isKeyName(s: string): s is KeyName {
  return (KEY_NAMES as readonly string[]).includes(s);
}

/** One key-bar button. `label` is what is drawn, `aria` what a screen reader hears. */
export interface KeyBarKey {
  name: KeyName;
  label: string;
  aria: string;
}

/** The key bar, left to right (Spec 3). Ctrl-C and Ctrl-D are holds, `/` also opens the slash menu on a hold. */
export const KEYBAR_KEYS: readonly KeyBarKey[] = [
  { name: "Esc", label: "Esc", aria: "Escape" },
  { name: "Tab", label: "Tab", aria: "Tab" },
  { name: "S-Tab", label: "Shift+Tab", aria: "Shift Tab" },
  { name: "Up", label: "↑", aria: "Up" },
  { name: "Down", label: "↓", aria: "Down" },
  { name: "Left", label: "←", aria: "Left" },
  { name: "Right", label: "→", aria: "Right" },
  { name: "Enter", label: "Enter", aria: "Enter" },
  { name: "BSpace", label: "⌫", aria: "Backspace" },
  { name: "C-c", label: "Ctrl-C", aria: "Control C (hold to send)" },
  { name: "C-d", label: "Ctrl-D", aria: "Control D (hold to send)" },
  { name: "C-u", label: "Ctrl-U", aria: "Control U" },
  { name: "Slash", label: "/", aria: "Slash (hold for commands)" },
];

/**
 * The slash-menu entries per harness (Spec 3). An entry is PASTED, never
 * entered. A harness with no entry here (or an empty list) makes a held `/`
 * behave as a tap.
 */
export const SLASH_MENU: Readonly<Record<string, readonly string[]>> = {
  claude: ["/help", "/compact", "/clear", "/model", "/resume"],
  omp: ["/compact", "/shake", "/tree"],
};

export function slashMenuFor(harness: string): readonly string[] {
  return SLASH_MENU[harness] ?? [];
}

/** The choice-bar buttons (Spec 7): digits 1-4 first, then 5-9 behind a disclosure, then y, n and the movers. */
export const CHOICE_DIGITS_PRIMARY: readonly KeyName[] = ["1", "2", "3", "4"];
export const CHOICE_DIGITS_MORE: readonly KeyName[] = ["5", "6", "7", "8", "9"];
export const CHOICE_ACTIONS: readonly KeyBarKey[] = [
  { name: "y", label: "y", aria: "Yes" },
  { name: "n", label: "n", aria: "No" },
  { name: "Enter", label: "Enter", aria: "Enter" },
  { name: "Esc", label: "Esc", aria: "Escape" },
  { name: "Up", label: "↑", aria: "Up" },
  { name: "Down", label: "↓", aria: "Down" },
];

/**
 * PHIN2 - the bytes the runner writes for a key NAME (`named_key_bytes` in
 * `crates/kb-term/src/runner/keys.rs`), for the direct-typing path that sends
 * raw bytes over the WS instead of names over REST. `keys.test.ts` reads the
 * Rust table and fails on drift. The arrows follow the terminal's cursor-keys
 * mode (`ESC O x` in application mode, `ESC [ x` otherwise); the runner's table
 * is the normal-mode form.
 */
const KEY_BYTES: Readonly<Record<KeyName, string>> = {
  Esc: "\x1b",
  Tab: "\x09",
  "S-Tab": "\x1b[Z",
  Enter: "\x0d",
  Up: "\x1b[A",
  Down: "\x1b[B",
  Right: "\x1b[C",
  Left: "\x1b[D",
  "C-c": "\x03",
  "C-d": "\x04",
  "C-u": "\x15",
  BSpace: "\x7f",
  Slash: "/",
  "1": "1",
  "2": "2",
  "3": "3",
  "4": "4",
  "5": "5",
  "6": "6",
  "7": "7",
  "8": "8",
  "9": "9",
  y: "y",
  n: "n",
};

const ARROW_FINAL: Readonly<Partial<Record<KeyName, string>>> = { Up: "A", Down: "B", Right: "C", Left: "D" };

export function keyBytes(name: KeyName, appCursor: boolean): string {
  const f = ARROW_FINAL[name];
  if (f !== undefined) return `${appCursor ? "\x1bO" : "\x1b["}${f}`;
  return KEY_BYTES[name];
}

/** Ctrl+arrow (`ESC [ 1 ; 5 x`); null for any other key (Ctrl does not change it). */
export function ctrlArrowBytes(name: KeyName): string | null {
  const f = ARROW_FINAL[name];
  return f === undefined ? null : `\x1b[1;5${f}`;
}
