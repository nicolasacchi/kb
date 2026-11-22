// FUTERMPASTE — a DataTransfer lookalike for jsdom tests. Not a test file.

/** A DataTransfer lookalike (jsdom has none). */
export function fakeDt(parts: { types?: string[]; files?: File[] }): DataTransfer {
  const files = parts.files ?? [];
  return {
    types: parts.types ?? [],
    files,
    items: files.map((f) => ({ kind: "file", type: f.type, getAsFile: () => f })),
  } as unknown as DataTransfer;
}
