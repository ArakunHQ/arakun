import { act, screen } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { Session } from "../lib/model";
import { fakeLink, renderScreen } from "../test/screen";
import { SessionScreen } from "./session";

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

const strict = (n: React.ReactNode) => <StrictMode>{n}</StrictMode>;
const tick = () => act(() => new Promise((r) => setTimeout(r, 0)));
const opens = (sent: unknown[]) => sent.filter((m) => (m as { type: string }).type === "open");

beforeEach(() => {
  localStorage.clear();
  (globalThis as any).RELAY = { scoped: (key: string) => key };
});
afterEach(() => {
  delete (globalThis as any).RELAY;
});

describe("the session screen", () => {
  it("opens the session when the page lands on its address, once", async () => {
    const link = fakeLink();
    await renderScreen(<SessionScreen />, "/s/a", link, strict);
    await tick();
    expect(opens(link.sent)).toEqual([{ type: "open", session: "a" }]);
    expect(link.sent).not.toContainEqual({ type: "close" });
  });

  it("does not open again what the row tap opened", async () => {
    const link = fakeLink({ session: session() });
    await renderScreen(<SessionScreen />, "/s/a", link, strict);
    await tick();
    expect(opens(link.sent)).toEqual([]);
  });

  it("closes the session on the way out", async () => {
    const link = fakeLink({ session: session() });
    const { router } = await renderScreen(<SessionScreen />, "/s/a", link, strict);
    await act(() => router.navigate({ to: "/inbox" }));
    await tick();
    expect(link.sent).toContainEqual({ type: "close" });
    expect(link.getState().session).toBeNull();
  });

  it("shows the title and the agent's lines, and asks for older ones near the top", async () => {
    const link = fakeLink({
      session: session({ from: 40, count: 42, entries: [{ type: "user", text: "hello" }, { type: "agent", text: "**hi** there" }] }),
    });
    const { container } = await renderScreen(<SessionScreen />, "/s/a", link);
    expect(screen.getByText("Fix the build")).toBeTruthy();
    // The top is in reach at once: what came before is asked for.
    expect(link.sent).toContainEqual({ type: "entries", session: "a", from: 0, to: 40 });
    expect(container.querySelector(".older")?.textContent).toBe("Loading…");
    expect(container.querySelector(".entry.agent.lead .who")?.textContent).toBe("claude");
    expect(container.querySelector(".entry.agent b")?.textContent).toBe("hi");
    const older = Array.from({ length: 40 }, (_, i) => ({ type: "notice", text: `n${i}` }));
    const s = link.getState().session!;
    link.set({ session: { ...s, from: 0, entries: [...older, ...s.entries] } });
    expect(container.querySelector(".older")?.textContent).toBe("");
    expect(container.querySelector(".entry.agent b")?.textContent).toBe("hi");
  });

  it("answers a permission and hides the card", async () => {
    const permission = { tool_call_id: "t1", title: "ls", options: [{ id: "no", name: "Deny", kind: "reject_once" }] };
    const link = fakeLink({ session: session({ status: "needs_input", permission }) });
    await renderScreen(<SessionScreen />, "/s/a", link);
    act(() => screen.getByText("Deny").click());
    expect(link.sent).toContainEqual({ type: "permission", session: "a", option: "no" });
    expect(screen.queryByText("Asks permission")).toBeNull();
  });
});
