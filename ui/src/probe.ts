// Witness constant for the shared ui/ source directory. Both SPAs import it in
// main.tsx and stamp it on <html data-kb-ui>, so the ui/ import is compiled
// into the app bundle (the e2e and spa-image CI jobs build it).
export const UI_PROBE = "ui/1";
