// Smallest possible ui/ component: proves JSX from ui/src compiles and renders
// against the importing app's React (18.3 in web-code, 19.3 in web).
export function Probe({ label }: { label: string }) {
  return <span data-kb-ui-probe>{label}</span>;
}
