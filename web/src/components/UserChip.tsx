import { isOtherUser } from "../lib/userAttribution";

// v0.34 W — small attribution chip for a username that differs from me.
// Used on comment rows, history rows, and (optionally) denser lists.
// Own / legacy-empty users render nothing so single-operator UI is
// byte-identical to pre-multi-user.

export type UserChipProps = {
  user?: string | null;
  me?: string | null;
  /// Extra class (e.g. history__user / cp__row-user).
  className?: string;
};

export default function UserChip({ user, me, className }: UserChipProps) {
  if (!isOtherUser(user, me)) return null;
  const name = (user ?? "").trim();
  const cls = ["kb-user-chip", className].filter(Boolean).join(" ");
  return (
    <span
      className={cls}
      data-kb-user={name}
      aria-label={`user ${name}`}
      title={name}
    >
      {name}
    </span>
  );
}
