export const a = { "--fx-from-ts": "red" };
export const b = "var(--fx-ts-string-missing, 2px)";
const flag = Math.random() > 0.5;
export const c = flag ? "--fx-ternary-not-declared" : "none";
export const d = "var(--fx-ternary-not-declared, 1px)";
