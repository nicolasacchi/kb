// W4.5's live-mirror decision logic: given a `mirror.updated` event and
// what the viewer is currently doing, decide whether to silently refetch
// the open file or surface a dismissible "file changed on disk — refresh"
// affordance instead. Pure — `hooks/useLiveMirror.ts` is the sole caller,
// feeding it `fileChanged` (does this event name the OPEN file) and
// `viewerDirty` (has the user scrolled away from the top, or made a
// selection — `CodeView`'s own scroll/selection tracking).

export interface LiveMirrorHeuristicInput {
  /// The open file's own path is among the paths this `mirror.updated`
  /// event named (repo already matched by the caller).
  fileChanged: boolean;
  /// The viewer has scrolled away from the top, OR carries an active
  /// (non-collapsed) selection — either means "the user is doing something
  /// with this view right now," so silently swapping its content
  /// underneath them would lose their place.
  viewerDirty: boolean;
}

export type LiveMirrorDecision = "ignore" | "auto-refresh" | "prompt";

/// - A mirror update for a file that ISN'T open: `"ignore"` (nothing to do
///   here — `queryClient.ts`'s broad per-repo invalidation already keeps
///   the tree/other queries fresh independently of this heuristic).
/// - The open file, viewer pristine (top of file, no selection):
///   `"auto-refresh"` — silently refetch, nothing lost.
/// - The open file, viewer dirty: `"prompt"` — surface the toast/badge and
///   let the user opt in via its Refresh action, never clobber their place.
export function decideLiveMirrorAction(input: LiveMirrorHeuristicInput): LiveMirrorDecision {
  if (!input.fileChanged) return "ignore";
  return input.viewerDirty ? "prompt" : "auto-refresh";
}

/// Whether a `mirror.updated` event's `paths` names `openPath` — repo-
/// relative comparison, exact match only (the server always reports
/// repo-relative paths for this event, see `sink.rs`'s `emit_mirror_updated`
/// doc). A `null`/`undefined` `openPath` (no file open) never matches.
export function eventNamesOpenFile(paths: string[], openPath: string | undefined | null): boolean {
  if (!openPath) return false;
  return paths.includes(openPath);
}

/// Wave E — the file-changed toast (`components/LiveMirrorBanners.tsx`'s
/// `FileChangedToast`) names WHICH pane's file changed, but only when a
/// split is actually open — a single-pane reader keeps the plain pre-Wave-E
/// wording ("File changed on disk"), since there's only one open file and
/// nothing to disambiguate. Pure so `Reader.tsx`'s labeling decision (one
/// `useLiveMirror` instance per pane, each independently deciding its own
/// `fileChangedOnDisk`) is unit-tested without mounting either pane.
export function fileChangedPaneLabel(pane: 1 | 2, otherPaneOpen: boolean): string | undefined {
  return otherPaneOpen ? `Pane ${pane}` : undefined;
}
