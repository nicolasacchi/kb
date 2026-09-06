/** Cross-file oracle: definition side (TS). */
export function greet(name: string): string {
  return `hi ${name}`;
}

export function otherGreet(name: string): string {
  return `yo ${name}`;
}
