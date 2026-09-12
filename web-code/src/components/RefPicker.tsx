import { useMemo, useState } from "react";
import { useNavigate } from "react-router";
import CheckoutDialog from "./CheckoutDialog";
import { useRefs } from "../hooks/useRefs";
import { codeUrl, type PaneLoc } from "../lib/codeUrl";

export interface RefPickerProps {
  repo: string;
  path: string;
  /// `undefined` = the working tree (the reader's default, mirroring
  /// `GET /api/file`'s own no-`ref` default).
  activeRef?: string;
  /// Wave E — the CURRENTLY open pane2 (if any), carried over into every
  /// ref-navigation this picker builds: switching pane1's ref is orthogonal
  /// to whatever split is open (pane2 has its own independent `ref` baked
  /// into `pane2=`, see `lib/codeUrl.ts`) — it must not silently close it.
  pane2?: PaneLoc;
}

/// `GET /api/refs?repo=` — branches + tags, current branch bolded.
/// Selecting a ref re-navigates to the SAME repo/path with `?ref=` set (or
/// cleared, for the synthetic "Working tree" entry), carrying `pane2`
/// forward via `codeUrl` directly (not `readerUrl`, whose narrower
/// signature doesn't expose `pane2` — see `lib/codeUrl.ts`'s header doc).
export default function RefPicker({ repo, path, activeRef, pane2 }: RefPickerProps) {
  const { data } = useRefs(repo);
  const navUrl = (ref?: string) => codeUrl({ repo, path, ref, pane2 });
  const [open, setOpen] = useState(false);
  /// W4.7 — the ref this picker is mid-checkout-confirm for, or `null`.
  /// `CheckoutDialog` owns the whole confirm→submit→{ok|dirty|error} flow
  /// (`lib/checkoutFlow.ts`); this is just "which target, if any."
  const [checkoutTarget, setCheckoutTarget] = useState<string | null>(null);
  const navigate = useNavigate();

  const branches = useMemo(() => data?.refs.filter((r) => r.kind === "branch") ?? [], [data]);
  const tags = useMemo(() => data?.refs.filter((r) => r.kind === "tag") ?? [], [data]);

  const label = activeRef ?? "Working tree";

  return (
    <div className="kbc-refpicker">
      <button
        type="button"
        className="kbc-refpicker__trigger"
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        {label}
      </button>
      {open && (
        <ul className="kbc-refpicker__menu" role="listbox">
          <li>
            <button
              type="button"
              className={activeRef === undefined ? "kbc-refpicker__item--active" : undefined}
              onClick={() => {
                setOpen(false);
                navigate(navUrl(undefined));
              }}
            >
              Working tree
            </button>
          </li>
          {branches.length > 0 && <li className="kbc-refpicker__group">Branches</li>}
          {branches.map((r) => (
            <li key={r.full_name} className="kbc-refpicker__row">
              <button
                type="button"
                className={r.name === activeRef ? "kbc-refpicker__item--active" : undefined}
                onClick={() => {
                  setOpen(false);
                  navigate(navUrl(r.name));
                }}
              >
                {r.name}
                {r.is_head && <span className="kbc-refpicker__head">HEAD</span>}
              </button>
              {!r.is_head && (
                <button
                  type="button"
                  className="kbc-refpicker__switch"
                  title={`Switch the working tree to ${r.name}`}
                  data-kbc-refpicker-switch
                  onClick={(e) => {
                    e.stopPropagation();
                    setOpen(false);
                    setCheckoutTarget(r.name);
                  }}
                >
                  Switch
                </button>
              )}
            </li>
          ))}
          {tags.length > 0 && <li className="kbc-refpicker__group">Tags</li>}
          {tags.map((r) => (
            <li key={r.full_name} className="kbc-refpicker__row">
              <button
                type="button"
                className={r.name === activeRef ? "kbc-refpicker__item--active" : undefined}
                onClick={() => {
                  setOpen(false);
                  navigate(navUrl(r.name));
                }}
              >
                {r.name}
              </button>
              <button
                type="button"
                className="kbc-refpicker__switch"
                title={`Switch the working tree to ${r.name}`}
                data-kbc-refpicker-switch
                onClick={(e) => {
                  e.stopPropagation();
                  setOpen(false);
                  setCheckoutTarget(r.name);
                }}
              >
                Switch
              </button>
            </li>
          ))}
        </ul>
      )}
      {checkoutTarget && (
        <CheckoutDialog repo={repo} target={checkoutTarget} onClose={() => setCheckoutTarget(null)} />
      )}
    </div>
  );
}
