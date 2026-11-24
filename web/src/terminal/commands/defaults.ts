// The shipped command catalogue (kb-term-commands/1) for claude and omp.
// defaults.json is the single source: the Rust validator's test includes the
// very same file. Every harness command not read from this repo's own
// sources carries `unverified: true` (see defaults.test.ts).

import defaults from "./defaults.json";
import type { Catalogue } from "./schema";

export const DEFAULT_CATALOGUE = defaults as unknown as Catalogue;
