import { act, fireEvent, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { latestStep, ranFor } from "../components/session/Subagents";
import { reduce, type Session, type Subagent } from "../lib/model";
import { fakeLink, renderScreen } from "../test/screen";
import { SessionScreen } from "./session";

const session = (over: Partial<Session> = {}): Session => ({
  id: "a",
  title: "Fix the build",
  agent: "claude",
  status: "idle",
  count: 2,
  from: 0,
  entries: [
    { type: "user", text: "look into it" },
    { type: "tool", id: "call-1", title: "Explore", status: "completed" },
  ],
  permission: null,
  question: null,
  place: "p",
  ...over,
});

const explore: Subagent = { id: "sub1", name: "Explore", task: "Find where the build breaks", state: "completed", started: 100, ended: 172, call: "call-1", host: null };

const tick = () => act(() => new Promise((r) => setTimeout(r, 0)));
const subagentMessages = (sent: unknown[]) => sent.filter((m) => (m as { type: string }).type === "subagent");

beforeEach(() => {
  localStorage.clear();
  (globalThis as any).RELAY = { scoped: (key: string) => key };
});
afterEach(() => {
  delete (globalThis as any).RELAY;
  vi.useRealTimers();
});

describe("subagents on the session screen", () => {
  it("leaves a Task row as it was when the computer sends no subagents", async () => {
    const link = fakeLink({ session: session() });
    const { container } = await renderScreen(<SessionScreen />, "/s/a", link);
    expect(container.querySelector(".subcard")).toBeNull();
    expect(container.querySelector("details.tool .ttitle")?.textContent).toBe("Explore");
    expect(container.querySelector(".bgsub")).toBeNull();
  });

  it("draws the subagent's card in place of its row: name, task, state and time", async () => {
    const link = fakeLink({ session: session(), subagents: [explore] });
    const { container } = await renderScreen(<SessionScreen />, "/s/a", link);
    const card = container.querySelector(".subcard.completed")!;
    expect(card).toBeTruthy();
    expect(card.querySelector(".scname")?.textContent).toBe("Explore");
    expect(card.querySelector(".sctask")?.textContent).toBe("Find where the build breaks");
    expect(card.querySelector(".scstate")?.textContent).toBe("Done · 1m 12s");
    expect(container.querySelector("details.tool")).toBeNull();
  });

  it("opens the transcript on a tap, follows it live, and goes back to the session", async () => {
    const link = fakeLink({ session: session(), subagents: [explore] });
    const { container, router } = await renderScreen(<SessionScreen />, "/s/a", link);
    fireEvent.click(screen.getByRole("button", { name: /Explore, Done/ }));
    await tick();
    expect(router.state.location.search).toEqual({ subagent: "sub1" });
    expect(link.sent).toContainEqual({ type: "subagent", session: "a", subagent: "sub1" });
    expect(screen.getByRole("dialog", { name: "Explore transcript" })).toBeTruthy();
    expect(container.querySelector(".subempty")?.textContent).toBe("Loading…");

    // The computer sends its tail, then each change.
    const msg = { type: "subagent_entries" as const, id: "a", subagent: "sub1", from: 0, count: 1, entries: [{ type: "agent", text: "Found it in **lib.rs**" }] };
    link.set(reduce(link.getState(), msg));
    const view = container.querySelector(".subview")!;
    expect(view.querySelector(".entry.agent b")?.textContent).toBe("lib.rs");
    expect(view.querySelector(".subempty")).toBeNull();
    // Read-only: no composer over a subagent.
    expect(view.querySelector("textarea")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Back to the session" }));
    await tick();
    expect(router.state.location.search).toEqual({});
    expect(container.querySelector(".subview")).toBeNull();
    expect(subagentMessages(link.sent).at(-1)).toEqual({ type: "subagent", session: "a" });
    expect(link.getState().subagent).toBeNull();
    // The session stays open under it.
    expect(link.sent).not.toContainEqual({ type: "close" });
  });

  it("lands on a subagent's address and asks for older entries near the top", async () => {
    const link = fakeLink({ session: session(), subagents: [explore] });
    await renderScreen(<SessionScreen />, "/s/a?subagent=sub1", link);
    await tick();
    expect(link.sent).toContainEqual({ type: "subagent", session: "a", subagent: "sub1" });
    const entries = [{ type: "notice", text: "n80" }];
    link.set(reduce(link.getState(), { type: "subagent_entries", id: "a", subagent: "sub1", from: 80, count: 81, entries }));
    expect(link.sent).toContainEqual({ type: "entries", session: "a", subagent: "sub1", from: 30, to: 80 });
    expect(screen.getByText("Loading…")).toBeTruthy();
  });

  it("says how many subagents run on in the background once the turn is over", async () => {
    const running = { ...explore, state: "running", ended: null };
    const link = fakeLink({ session: session(), subagents: [running, { ...running, id: "sub2", call: "call-2" }] });
    const { container } = await renderScreen(<SessionScreen />, "/s/a", link);
    expect(container.querySelector(".bgsub")?.textContent).toBe("2 subagents running in the background");
    // While the agent works, the stop row says it is busy.
    link.set({ session: { ...link.getState().session!, status: "working" }, inbox: [] });
    expect(container.querySelector(".bgsub")).toBeNull();
  });
});

describe("a subagent card's details", () => {
  it("runs its clock until it ends", () => {
    expect(ranFor({ ...explore, state: "running", ended: null }, 128)).toBe("28s");
    expect(ranFor(explore, 9999)).toBe("1m 12s");
  });

  it("takes the latest step from the open transcript, else the calls made under it", () => {
    const entries = [
      { type: "tool", id: "t1", title: "Read lib.rs", parent: "call-1" },
      { type: "tool", id: "t2", title: "Not its own", parent: "call-9" },
    ];
    expect(latestStep(explore, entries, null)).toBe("Read lib.rs");
    const open = { id: "sub1", loaded: true, count: 2, from: 0, entries: [{ type: "tool", command: "cargo build" }, { type: "thought", text: "hm" }] };
    expect(latestStep(explore, entries, open)).toBe("Thinking");
    expect(latestStep(explore, [], null)).toBe("");
  });
});
