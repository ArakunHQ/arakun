import { act, fireEvent, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Session } from "../../lib/model";
import { SessionScreen } from "../../routes/session";
import { fakeLink, renderScreen } from "../../test/screen";
import { STOP_WINDOW } from "./StopRow";

const session = (over: Partial<Session> = {}): Session => ({
  id: "a",
  title: "Fix the build",
  agent: "claude",
  status: "idle",
  count: 0,
  from: 0,
  entries: [],
  permission: null,
  question: null,
  place: "p",
  ...over,
});

beforeEach(() => {
  localStorage.clear();
  (globalThis as any).RELAY = { scoped: (key: string) => key };
});
afterEach(() => {
  vi.useRealTimers();
  delete (globalThis as any).RELAY;
});

const draft = () => screen.getByPlaceholderText("Message claude…") as HTMLTextAreaElement;

describe("Composer", () => {
  it("keeps the draft per session, across a remount", async () => {
    const link = fakeLink({ session: session() });
    const first = await renderScreen(<SessionScreen />, "/s/a", link);
    fireEvent.change(draft(), { target: { value: "half a thought" } });
    expect(localStorage.getItem("arakun.draft:a")).toBe("half a thought");
    first.unmount();
    await renderScreen(<SessionScreen />, "/s/a", fakeLink({ session: session() }));
    expect(draft().value).toBe("half a thought");
  });

  it("stops only on a second tap within the window", async () => {
    const link = fakeLink({ session: session({ status: "working" }) });
    await renderScreen(<SessionScreen />, "/s/a", link);
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout", "Date"] });
    fireEvent.click(screen.getByRole("button", { name: "Stop" }));
    expect(link.sent).not.toContainEqual({ type: "cancel", session: "a" });
    act(() => void vi.advanceTimersByTime(STOP_WINDOW + 1));
    // The window went by: this tap only arms it again.
    fireEvent.click(screen.getByRole("button", { name: "Stop" }));
    expect(link.sent).not.toContainEqual({ type: "cancel", session: "a" });
    act(() => void vi.advanceTimersByTime(STOP_WINDOW - 1));
    fireEvent.click(screen.getByRole("button", { name: "Tap again to stop" }));
    expect(link.sent).toContainEqual({ type: "cancel", session: "a" });
  });

  it("shows a sent message as pending until the computer has it", async () => {
    const link = fakeLink({ session: session() });
    await renderScreen(<SessionScreen />, "/s/a", link);
    fireEvent.change(draft(), { target: { value: "run the tests" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(link.sent).toContainEqual({ type: "send", session: "a", text: "run the tests" });
    expect(screen.getByText("Sending…")).toBeTruthy();
    expect(draft().value).toBe("");
    // An agent line alone does not settle it.
    link.set({ session: session({ count: 1, entries: [{ type: "agent", text: "ok" }] }) });
    expect(screen.queryByText("Sending…")).toBeTruthy();
    link.set({ session: session({ count: 2, entries: [{ type: "agent", text: "ok" }, { type: "user", text: "run the tests" }] }) });
    expect(screen.queryByText("Sending…")).toBeNull();
  });

  it("keeps a message written offline and sends it once linked", async () => {
    const link = fakeLink({ connected: false, session: session() });
    await renderScreen(<SessionScreen />, "/s/a", link);
    fireEvent.change(draft(), { target: { value: "later" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(link.sent).not.toContainEqual(expect.objectContaining({ type: "send" }));
    expect(screen.getByText("Waiting to send · sends when reconnected")).toBeTruthy();
    link.set({ connected: true });
    link.emit({ type: "linked" });
    expect(link.sent).toContainEqual({ type: "send", session: "a", text: "later" });
    expect(screen.getByText("Sending…")).toBeTruthy();
  });
});
