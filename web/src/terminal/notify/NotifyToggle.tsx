// TERMNOTIFY - the opt-in control for browser notifications. Exported, mounted
// by whichever unit owns terminal settings. The permission prompt is requested
// from the click itself and the pref is stored only when it is granted.

import { useState, type ChangeEvent, type ReactElement } from "react";
import { getNotifyPref, setNotifyPref } from "./notifyPref";

export default function NotifyToggle(): ReactElement {
  const supported = typeof Notification !== "undefined";
  const [on, setOn] = useState(() => supported && getNotifyPref());
  const [denied, setDenied] = useState(() => supported && Notification.permission === "denied");

  const onChange = (e: ChangeEvent<HTMLInputElement>) => {
    if (!supported) return;
    if (!e.target.checked) {
      setNotifyPref(false);
      setOn(false);
      return;
    }
    // No await before the call: the request must run inside the user gesture.
    const p = Notification.requestPermission();
    void Promise.resolve(p).then(
      (res) => {
        if (res === "granted") {
          setNotifyPref(true);
          setOn(true);
          setDenied(false);
        } else {
          setNotifyPref(false);
          setOn(false);
          setDenied(res === "denied");
        }
      },
      () => {
        setNotifyPref(false);
        setOn(false);
      },
    );
  };

  return (
    <label className="kb-term-notify">
      <input type="checkbox" checked={on} disabled={!supported} onChange={onChange} />{" "}
      Notify me when another terminal needs me
      {!supported && <span className="kb-term-notify__cap"> (not supported)</span>}
      {supported && denied && <span className="kb-term-notify__cap"> (blocked by the browser)</span>}
    </label>
  );
}
