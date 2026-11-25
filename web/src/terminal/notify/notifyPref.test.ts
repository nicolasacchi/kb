import { describe, expect, it } from "vitest";
import { NOTIFY_KEY, getNotifyPref, setNotifyPref } from "./notifyPref";

function mem() {
  const m = new Map<string, string>();
  return {
    getItem: (k: string) => m.get(k) ?? null,
    setItem: (k: string, v: string) => void m.set(k, v),
    removeItem: (k: string) => void m.delete(k),
  };
}
const boom = {
  getItem: () => {
    throw new Error("x");
  },
  setItem: () => {
    throw new Error("x");
  },
  removeItem: () => {
    throw new Error("x");
  },
};

describe("notifyPref", () => {
  it("defaults off and round-trips", () => {
    const s = mem();
    expect(getNotifyPref(s)).toBe(false);
    setNotifyPref(true, s);
    expect(s.getItem(NOTIFY_KEY)).toBe("1");
    expect(getNotifyPref(s)).toBe(true);
    setNotifyPref(false, s);
    expect(getNotifyPref(s)).toBe(false);
  });
  it("throwing or missing storage degrades to off", () => {
    expect(getNotifyPref(boom)).toBe(false);
    expect(() => setNotifyPref(true, boom)).not.toThrow();
    expect(getNotifyPref(null)).toBe(false);
  });
});
