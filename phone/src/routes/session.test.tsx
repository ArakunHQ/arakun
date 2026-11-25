import { act, screen } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import composerCss from "../components/session/Composer.css?raw";
import permissionCss from "../components/session/PermissionCard.css?raw";
import transcriptCss from "../components/session/transcript.css?raw";
import type { Session } from "../lib/model";
import baseCss from "../styles/base.css?raw";
import { fakeLink, renderScreen, styleAt, widthIs } from "../test/screen";
import sessionCss from "./session.css?raw";
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
  vi.unstubAllGlobals();
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

  it("closes a sheet with the back gesture and stays in the session", async () => {
    const link = fakeLink({ session: session({ changes: { files: 1, added: 2, removed: 0 } }) });
    const { router, container } = await renderScreen(<SessionScreen />, "/s/a", link, strict);
    await tick();
    await act(async () => (container.querySelector(".chg") as HTMLButtonElement).click());
    await tick();
    expect(router.state.location.search).toEqual({ sheet: "changes" });
    expect(container.querySelector(".sheet .title")?.textContent).toBe("Changes");
    expect(link.sent.filter((m) => (m as { type: string }).type === "changes")).toHaveLength(1);
    await act(async () => router.history.back());
    await tick();
    expect(container.querySelector(".sheet")).toBeNull();
    expect(router.state.location.pathname).toBe("/s/a");
    // A sheet is a step of the address, not another opening of the session.
    expect(link.sent).not.toContainEqual({ type: "close" });
    expect(opens(link.sent)).toEqual([]);
  });

  it("goes from a diff back to the list of changes", async () => {
    const link = fakeLink({ session: session({ changes: { files: 1, added: 1, removed: 0 } }) });
    const { router, container } = await renderScreen(<SessionScreen />, "/s/a", link);
    await act(async () => (container.querySelector(".chg") as HTMLButtonElement).click());
    await tick();
    link.set({ changes: [{ text: "fix", at: 0, files: [{ name: "a.ts", path: "src/a.ts", diffs: [[3, 0]], added: 1, removed: 0 }] }] });
    await act(async () => screen.getByText("a.ts").closest("button")!.click());
    await tick();
    expect(router.state.location.search).toEqual({ sheet: "diff" });
    expect(container.querySelector(".sheet .title")?.textContent).toBe("a.ts");
    expect(link.sent).toContainEqual({ type: "diff", session: "a", entry: 3, index: 0 });
    await act(async () => router.history.back());
    await tick();
    expect(container.querySelector(".sheet .title")?.textContent).toBe("Changes");
    // The sheet's own Back steps back too: the session is left with no sheet.
    await act(async () => (container.querySelector(".sheet .iconbtn") as HTMLButtonElement).click());
    await tick();
    expect(container.querySelector(".sheet")).toBeNull();
    expect(router.state.location.search).toEqual({});
    // Stepped back, not forward: the address the page landed on is the first again.
    expect(router.history.canGoBack()).toBe(false);
    expect(link.sent).not.toContainEqual({ type: "close" });
  });

  it("falls back to the list when a diff's address has nothing to show", async () => {
    const link = fakeLink({ session: session() });
    const { container } = await renderScreen(<SessionScreen />, "/s/a?sheet=diff", link);
    await tick();
    expect(container.querySelector(".sheet .title")?.textContent).toBe("Changes");
    expect(link.sent).toContainEqual({ type: "changes", session: "a" });
  });
});

describe("the session at each width", () => {
  const permission = {
    tool_call_id: "t1",
    title: "cargo test",
    options: [
      { id: "yes", name: "Allow", kind: "allow_once" },
      { id: "always", name: "Always allow", kind: "allow_always" },
      { id: "no", name: "Deny", kind: "reject_once" },
    ],
  };
  let unstyle = () => {};
  afterEach(() => unstyle());

  async function at(px: number) {
    widthIs(px);
    unstyle = styleAt(px, baseCss, sessionCss, transcriptCss, permissionCss, composerCss);
    const link = fakeLink({ session: session({ status: "needs_input", permission, entries: [{ type: "user", text: "hello" }] }) });
    return renderScreen(<SessionScreen />, "/s/a", link);
  }

  it("reads in a 760 px column in the middle of a desktop, the composer under the transcript", async () => {
    const { container } = await at(1440);
    const col = container.querySelector<HTMLElement>(".scol")!;
    expect(col).toBeTruthy();
    const css = getComputedStyle(col);
    expect(css.maxWidth).toBe("760px");
    expect(css.width).toBe("100%");
    expect([css.marginLeft, css.marginRight]).toEqual(["auto", "auto"]);
    const log = col.querySelector(".logwrap")!;
    const composer = col.querySelector(".composer")!;
    expect(log && composer).toBeTruthy();
    expect(log.compareDocumentPosition(composer) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    // The sidebar is the way out; the header has no Back.
    expect(screen.queryByRole("button", { name: "Back" })).toBeNull();
  });

  it.each([834, 390])("fills the width with big permission buttons at %i px", async (px) => {
    const { container } = await at(px);
    expect(getComputedStyle(container.querySelector(".scol")!).maxWidth).not.toBe("760px");
    const buttons = [...container.querySelectorAll<HTMLElement>(".perm .opts button, .perm .always")];
    // Deny, Hold to allow and Always allow.
    expect(buttons).toHaveLength(3);
    for (const b of buttons) {
      const css = getComputedStyle(b);
      expect(Math.max(parseFloat(css.height) || 0, parseFloat(css.minHeight) || 0)).toBeGreaterThanOrEqual(44);
    }
    // Hold to allow and tap twice for Always allow, at every width.
    expect(container.querySelector(".perm .btn.hold")).toBeTruthy();
    expect(screen.getByText("For the rest of this session · tap twice")).toBeTruthy();
    // A tablet has the rail to go back with; a phone has only Back.
    expect(!!screen.queryByRole("button", { name: "Back" })).toBe(px < 768);
  });
});
