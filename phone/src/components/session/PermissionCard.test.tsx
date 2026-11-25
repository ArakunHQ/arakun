import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { lastToast, toast } from "../../lib/toast";
import { type Permission, PermissionCard } from "./PermissionCard";

const permission: Permission = {
  tool_call_id: "t1",
  title: "rm -rf build",
  options: [
    { id: "allow", name: "Allow", kind: "allow_once" },
    { id: "always", name: "Always allow", kind: "allow_always" },
    { id: "deny", name: "Deny", kind: "reject_once" },
  ],
};

beforeEach(() => {
  vi.useFakeTimers();
  toast("");
});
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

const hold = () => screen.getByText("Hold to allow").closest("button")!;
const always = () => screen.getByText("Always allow").closest("button")!;

describe("PermissionCard", () => {
  it("does not allow when the finger lifts before 700 ms", () => {
    const onAnswer = vi.fn();
    render(<PermissionCard permission={permission} onAnswer={onAnswer} />);
    fireEvent.pointerDown(hold());
    act(() => void vi.advanceTimersByTime(699));
    fireEvent.pointerUp(hold());
    act(() => void vi.advanceTimersByTime(1000));
    expect(onAnswer).not.toHaveBeenCalled();
  });

  it("allows once after a 700 ms hold, with a buzz", () => {
    const vibrate = vi.fn();
    vi.stubGlobal("navigator", { ...navigator, vibrate });
    const onAnswer = vi.fn();
    render(<PermissionCard permission={permission} onAnswer={onAnswer} />);
    const b = hold();
    fireEvent.pointerDown(b);
    expect(b.className).toContain("holding");
    act(() => void vi.advanceTimersByTime(700));
    fireEvent.pointerUp(b);
    fireEvent.click(b);
    expect(onAnswer).toHaveBeenCalledTimes(1);
    expect(onAnswer).toHaveBeenCalledWith("allow");
    expect(vibrate).toHaveBeenCalledWith(15);
    expect(lastToast()).toBe("");
  });

  it("says to hold on a plain tap", () => {
    const onAnswer = vi.fn();
    render(<PermissionCard permission={permission} onAnswer={onAnswer} />);
    fireEvent.click(hold());
    expect(lastToast()).toBe("Hold to allow");
    expect(onAnswer).not.toHaveBeenCalled();
  });

  it("answers Always allow only on a second tap within 3000 ms", () => {
    const onAnswer = vi.fn();
    render(<PermissionCard permission={permission} onAnswer={onAnswer} />);
    fireEvent.click(always());
    expect(screen.getByText("Tap again to always allow")).toBeTruthy();
    expect(onAnswer).not.toHaveBeenCalled();
    act(() => void vi.advanceTimersByTime(2999));
    fireEvent.click(screen.getByText("Tap again to always allow").closest("button")!);
    expect(onAnswer).toHaveBeenCalledWith("always");
  });

  it("only arms Always allow again after 3001 ms", () => {
    const onAnswer = vi.fn();
    render(<PermissionCard permission={permission} onAnswer={onAnswer} />);
    fireEvent.click(always());
    act(() => void vi.advanceTimersByTime(3001));
    expect(screen.getByText("Always allow")).toBeTruthy();
    fireEvent.click(always());
    expect(onAnswer).not.toHaveBeenCalled();
    expect(screen.getByText("Tap again to always allow")).toBeTruthy();
  });

  it("denies on a plain tap", () => {
    const onAnswer = vi.fn();
    render(<PermissionCard permission={permission} onAnswer={onAnswer} />);
    fireEvent.click(screen.getByText("Deny"));
    expect(onAnswer).toHaveBeenCalledWith("deny");
  });
});
