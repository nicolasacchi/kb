// OL L4 — the gallery card's single quiet meta line: folder · age · reading
// time. Reading time is words / 220 (the common adult-prose rate), rounded,
// never below 1 minute, and absent when the doc carries no word count.
export const WORDS_PER_MINUTE = 220;

export type CardMetaParts = {
  folder: string | null;
  read: string | null;
};

export function readMinutes(words: number | null | undefined): number | null {
  if (!words || words <= 0) return null;
  return Math.max(1, Math.round(words / WORDS_PER_MINUTE));
}

export function cardMetaParts(
  folder: string | null | undefined,
  words: number | null | undefined,
): CardMetaParts {
  const m = readMinutes(words);
  return {
    folder: folder ? folder : null,
    read: m == null ? null : `${m} min read`,
  };
}
