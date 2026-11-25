// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import NotifyToggle from "./NotifyToggle";
import { NOTIFY_KEY, getNotifyPref } from "./notifyPref";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  localStorage.clear();
});

function stub(result: string, permission = "default") {
  const requestPermission = vi.fn(() => Promise.resolve(result));
  vi.stubGlobal("Notification", { permission, requestPermission });
  return requestPermission;
}

describe("NotifyToggle", () => {
  it("requests permission synchronously from the click and stores on granted", async () => {
    const req = stub("granted");
    render(<NotifyToggle />);
    expect(req).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("checkbox"));
    expect(req).toHaveBeenCalledTimes(1); // same tick as the click
    await waitFor(() => expect(getNotifyPref()).toBe(true));
  });
  it("does not persist when the result is denied, and says so", async () => {
    stub("denied");
    render(<NotifyToggle />);
    fireEvent.click(screen.getByRole("checkbox"));
    await waitFor(() => expect(screen.getByText(/blocked by the browser/)).toBeTruthy());
    expect(localStorage.getItem(NOTIFY_KEY)).toBeNull();
  });
  it("turning off just clears the pref without asking", async () => {
    const req = stub("granted", "granted");
    localStorage.setItem(NOTIFY_KEY, "1");
    render(<NotifyToggle />);
    fireEvent.click(screen.getByRole("checkbox"));
    expect(getNotifyPref()).toBe(false);
    expect(req).not.toHaveBeenCalled();
  });
  it("unsupported renders disabled with a caption", () => {
    vi.stubGlobal("Notification", undefined);
    render(<NotifyToggle />);
    expect((screen.getByRole("checkbox") as HTMLInputElement).disabled).toBe(true);
    expect(screen.getByText(/not supported/)).toBeTruthy();
  });
});
