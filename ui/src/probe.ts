// Witness constant for the shared ui/ source directory. Both SPAs import it in
// main.tsx and stamp it on <html data-kb-ui>, so a production bundle provably
// contains ui/ code.
export const UI_PROBE = "ui/1";
