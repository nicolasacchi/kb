import { useEffect, useState } from "react";
import { Icon } from "@kb/ui/icons";

// Removable (and, where the caller allows, editable) `key:value` filter
// tokens. Carried over from the old context line so the power-user remove /
// edit affordances survive its removal.
export type FilterToken = {
  k: string;
  value: string;
  onRemove: () => void;
  onChange?: (next: string) => void;
};

export default function FilterChips({ tokens }: { tokens: FilterToken[] }) {
  return (
    <>
      {tokens.map((t) => (
        <FilterChip key={`${t.k}:${t.value}`} {...t} />
      ))}
    </>
  );
}

function FilterChip({ k, value, onRemove, onChange }: FilterToken) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(value);
  useEffect(() => {
    if (!editing) setDraft(value);
  }, [value, editing]);

  const editable = !!onChange;
  return (
    <span className="kb-fchip">
      <span className="kb-fchip__tok k">{k}</span>
      <span className="kb-fchip__op">:</span>
      {editing && onChange ? (
        <input
          ref={(el) => {
            if (el && document.activeElement !== el) {
              el.focus();
              el.select();
            }
          }}
          className="kb-fchip__tok v kb-fchip__edit"
          value={draft}
          size={Math.max(4, draft.length + 1)}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              const next = draft.trim();
              if (next && next !== value) onChange(next);
              setEditing(false);
            } else if (e.key === "Escape") {
              setDraft(value);
              setEditing(false);
            }
          }}
          onBlur={() => {
            setDraft(value);
            setEditing(false);
          }}
          aria-label={`edit ${value}`}
        />
      ) : (
        <span className={`kb-fchip__tok v kb-fchip__tok--removable${editable ? " kb-fchip__tok--editable" : ""}`}>
          <span
            className="kb-fchip__val"
            role={editable ? "button" : undefined}
            tabIndex={editable ? 0 : undefined}
            onClick={(e) => {
              if (editable) {
                e.stopPropagation();
                setEditing(true);
              }
            }}
            onKeyDown={(e) => {
              if (editable && (e.key === "Enter" || e.key === " ")) {
                e.preventDefault();
                setEditing(true);
              }
            }}
          >
            {value}
          </span>
          <button
            type="button"
            className="kb-fchip__x"
            onClick={(e) => {
              e.stopPropagation();
              onRemove();
            }}
            title="remove filter"
            aria-label={`remove ${value}`}
          >
            <Icon.X />
          </button>
        </span>
      )}
    </span>
  );
}
