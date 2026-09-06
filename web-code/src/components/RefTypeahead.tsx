import {
  useId,
  useMemo,
  useRef,
  useState,
  type InputHTMLAttributes,
  type KeyboardEvent,
} from "react";
import { highlightSegments, speedFilterItems } from "../lib/speedSearch";

const LIST_CAP = 12;

export interface RefTypeaheadProps {
  value: string;
  onChange: (next: string) => void;
  items: readonly string[];
  placeholder?: string;
  "aria-label"?: string;
  /// Extra class on the input (Compare/RangeDiff keep their existing
  /// `kbc-compare__ref-input` / `kbc-rangediff__input` so layout stays
  /// byte-compatible).
  inputClassName?: string;
  className?: string;
  /// Forwarded onto the `<input>` so existing `data-kbc-*` attrs survive
  /// the swap from a raw field. `data-*` is an index signature because
  /// `InputHTMLAttributes` does not declare those keys.
  inputProps?: InputHTMLAttributes<HTMLInputElement> & {
    [key: `data-${string}`]: string | undefined;
  };
  disabled?: boolean;
  id?: string;
}

/// Shared ARIA combobox for branch/tag/ref names. Filtering is
/// `speedFilterItems`; marks are `highlightSegments`. Controlled value —
/// callers own the string, so Compare/RangeDiff submitted query params
/// stay whatever the operator typed (selecting a hit just writes that
/// name back).
export default function RefTypeahead({
  value,
  onChange,
  items,
  placeholder,
  "aria-label": ariaLabel,
  inputClassName,
  className,
  inputProps,
  disabled,
  id,
}: RefTypeaheadProps) {
  const uid = useId();
  const listId = `${uid}-list`;
  const inputId = id ?? `${uid}-input`;
  const inputRef = useRef<HTMLInputElement | null>(null);
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);

  const hits = useMemo(() => speedFilterItems(items, value, (s) => s).slice(0, LIST_CAP), [items, value]);

  function select(name: string) {
    onChange(name);
    setOpen(false);
    inputRef.current?.focus();
  }

  function onKeyDown(e: KeyboardEvent<HTMLInputElement>) {
    if (e.key === "ArrowDown") {
      if (hits.length === 0) return;
      e.preventDefault();
      setOpen(true);
      setActive((i) => (i + 1) % hits.length);
      return;
    }
    if (e.key === "ArrowUp") {
      if (hits.length === 0) return;
      e.preventDefault();
      setOpen(true);
      setActive((i) => (i - 1 + hits.length) % hits.length);
      return;
    }
    if (e.key === "Enter" && open && hits.length > 0) {
      e.preventDefault();
      const hit = hits[active] ?? hits[0];
      if (hit) select(hit.item);
      return;
    }
    if (e.key === "Escape") {
      if (open) {
        e.preventDefault();
        setOpen(false);
      }
    }
  }

  const showList = open && !disabled && hits.length > 0;
  const activeId = showList && hits[active] ? `${uid}-opt-${active}` : undefined;

  return (
    <div className={"kbc-reftypeahead" + (className ? ` ${className}` : "")} data-kbc-reftypeahead>
      <input
        {...inputProps}
        ref={inputRef}
        id={inputId}
        className={inputClassName}
        value={value}
        disabled={disabled}
        placeholder={placeholder}
        aria-label={ariaLabel}
        role="combobox"
        aria-expanded={showList}
        aria-controls={listId}
        aria-autocomplete="list"
        aria-activedescendant={activeId}
        autoComplete="off"
        onChange={(e) => {
          onChange(e.target.value);
          setOpen(true);
          setActive(0);
        }}
        onFocus={() => {
          if (hits.length > 0) setOpen(true);
        }}
        onBlur={() => {
          // Delay so an option mousedown (which preventDefault's to keep
          // focus) can still fire click before we tear the list down.
          window.setTimeout(() => setOpen(false), 0);
        }}
        onKeyDown={onKeyDown}
      />
      {showList && (
        <ul className="kbc-reftypeahead__list" role="listbox" id={listId} data-kbc-reftypeahead-list>
          {hits.map((hit, i) => {
            const segs = highlightSegments(hit.item, hit.ranges);
            return (
              <li
                key={hit.item}
                id={`${uid}-opt-${i}`}
                role="option"
                aria-selected={i === active}
                className={
                  "kbc-reftypeahead__opt" + (i === active ? " kbc-reftypeahead__opt--active" : "")
                }
                data-kbc-reftypeahead-opt={hit.item}
                onMouseDown={(e) => e.preventDefault()}
                onMouseEnter={() => setActive(i)}
                onClick={() => select(hit.item)}
              >
                {segs.map((s, j) =>
                  s.hit ? <mark key={j}>{s.text}</mark> : <span key={j}>{s.text}</span>,
                )}
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
