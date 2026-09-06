/** Cross-file oracle: consumer with relative import. */
import { greet } from "./util";

export function main(): string {
  return greet("world");
}
