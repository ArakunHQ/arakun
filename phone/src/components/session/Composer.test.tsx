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
    expect(link.sent).not.toContainEqual(expect.objectContaining({ type: "cancel" }));
    act(() => void vi.advanceTimersByTime(STOP_WINDOW + 1));
    // The window went by: this tap only arms it again.
    fireEvent.click(screen.getByRole("button", { name: "Stop" }));
    expect(link.sent).not.toContainEqual(expect.objectContaining({ type: "cancel" }));
    act(() => void vi.advanceTimersByTime(STOP_WINDOW - 1));
    fireEvent.click(screen.getByRole("button", { name: "Tap again to stop" }));
    expect(link.sent).toContainEqual({ type: "cancel", session: "a", keep_queue: true });
  });

  it("shows a sent message as pending until the computer has it", async () => {
    const link = fakeLink({ session: session() });
    await renderScreen(<SessionScreen />, "/s/a", link);
    fireEvent.change(draft(), { target: { value: "run the tests" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(link.sent).toContainEqual({ type: "send", session: "a", text: "run the tests", sent: expect.any(String) });
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
    expect(link.sent).toContainEqual({ type: "send", session: "a", text: "later", sent: expect.any(String) });
    expect(screen.getByText("Sending…")).toBeTruthy();
  });

  // What the phone sent last, and its id.
  const lastSend = (link: ReturnType<typeof fakeLink>) => (link.sent as { type: string; text: string; sent: string }[]).filter((m) => m.type === "send").at(-1)!;

  it("marks a message the server never got, and sends it again on Retry", async () => {
    const link = fakeLink({ session: session() });
    await renderScreen(<SessionScreen />, "/s/a", link);
    fireEvent.change(draft(), { target: { value: "deploy it" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    const first = lastSend(link);
    // Another id is not this message.
    act(() => link.emit({ type: "unsent", session: "a", sent: "other", message: "Lost." }));
    expect(screen.getByText("Sending…")).toBeTruthy();
    act(() => link.emit({ type: "unsent", session: "a", sent: first.sent, message: "The server was not connected." }));
    expect(screen.queryByText("Sending…")).toBeNull();
    expect(screen.getByText("The server was not connected.")).toBeTruthy();
    // A user entry from elsewhere does not settle it: its copy is not coming.
    link.set({ session: session({ count: 1, entries: [{ type: "user", text: "from the desktop" }] }) });
    expect(screen.getByText("The server was not connected.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Retry: deploy it" }));
    const again = lastSend(link);
    expect(again.text).toBe("deploy it");
    expect(again.sent).not.toBe(first.sent);
    expect(screen.queryByText("The server was not connected.")).toBeNull();
    expect(screen.getByText("Sending…")).toBeTruthy();
  });

  it("takes a message the server never got back into the box", async () => {
    const link = fakeLink({ session: session() });
    await renderScreen(<SessionScreen />, "/s/a", link);
    fireEvent.change(draft(), { target: { value: "deploy it" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    act(() => link.emit({ type: "unsent", session: "a", sent: lastSend(link).sent, message: "Not sent." }));
    fireEvent.change(draft(), { target: { value: "first" } });
    const sends = link.sent.length;
    fireEvent.click(screen.getByRole("button", { name: "Take back: deploy it" }));
    expect(draft().value).toBe("first\n\ndeploy it");
    expect(screen.queryByText("Not sent.")).toBeNull();
    expect(link.sent.length).toBe(sends);
  });
});

describe("Queue", () => {
  it("takes a queued prompt back into the box", async () => {
    const link = fakeLink({ session: session({ status: "working", queue: ["first", "second"] }) });
    await renderScreen(<SessionScreen />, "/s/a", link);
    fireEvent.change(draft(), { target: { value: "draft" } });
    fireEvent.click(screen.getByRole("button", { name: "Take back: second" }));
    expect(link.sent).toContainEqual({ type: "unqueue", session: "a", index: 1, text: "second" });
    expect(draft().value).toBe("draft\n\nsecond");
  });

  it("offers Resume only while the queue is paused", async () => {
    const link = fakeLink({ session: session({ queue: ["next"] }) });
    await renderScreen(<SessionScreen />, "/s/a", link);
    expect(screen.queryByRole("button", { name: "Resume" })).toBeNull();
    link.set({ session: session({ queue: ["next"], queue_paused: true }) });
    fireEvent.click(screen.getByRole("button", { name: "Resume" }));
    expect(link.sent).toContainEqual({ type: "resume_queue", session: "a" });
  });

  it("shows a sent prompt the computer queued once, in the queue", async () => {
    const link = fakeLink({ session: session({ status: "working" }) });
    await renderScreen(<SessionScreen />, "/s/a", link);
    fireEvent.change(draft(), { target: { value: "after this" } });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    expect(screen.getByText("Sending…")).toBeTruthy();
    link.set({ session: session({ status: "working", queue: ["after this"] }) });
    expect(screen.queryByText("Sending…")).toBeNull();
    expect(screen.getByRole("button", { name: "Take back: after this" })).toBeTruthy();
  });

  it("stops and sends what is written", async () => {
    const link = fakeLink({ session: session({ status: "working" }) });
    await renderScreen(<SessionScreen />, "/s/a", link);
    fireEvent.change(draft(), { target: { value: "do this instead" } });
    fireEvent.click(screen.getByRole("button", { name: "Stop and send" }));
    fireEvent.click(screen.getByRole("button", { name: "Tap again to stop and send" }));
    expect(link.sent).toContainEqual({ type: "send", session: "a", text: "do this instead", now: true, sent: expect.any(String) });
  });
});
