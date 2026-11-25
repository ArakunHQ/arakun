import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { StrictMode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { LinkContext } from "../../lib/link";
import type { LinkState } from "../../lib/model";
import { detectWay, resetStart } from "../../lib/start";
import { Inbox } from "../../routes/inbox";
import { SessionScreen } from "../../routes/session";
import { validateSessionSearch } from "../session/sheetNav";
import { fakeLink, type FakeLink } from "../../test/screen";
import { Shell } from "./Shell";

const now = Math.floor(Date.now() / 1000);
const row = (id: string, status: string, over = {}) => ({ id, title: `t-${id}`, agent: "claude", status, project: "p", env: "main", updated: now - 60, ...over });
const inbox = [row("a", "needs_input"), row("b", "working"), row("c", "idle"), row("d", "error")] as LinkState["inbox"];

/** The window as wide as `px`: what `matchMedia` answers for a min-width query. */
function widthIs(px: number) {
  vi.stubGlobal("matchMedia", (q: string) => {
    const min = /min-width:\s*(\d+)px/.exec(q);
    return {
      matches: min ? px >= Number(min[1]) : false,
      media: q,
      onchange: null,
      addEventListener() {},
      removeEventListener() {},
      addListener() {},
      removeListener() {},
      dispatchEvent: () => false,
    };
  });
}

/** The account host, signed in as `rik`, with two computers. */
async function account() {
  (globalThis as any).RELAY = {
    forgetOldPairing() {},
    detect: async () => "account",
    boot: async () => ({ screen: "ok", me: { login: "rik", notices: [] }, machines: [] }),
    machines: async () => ({ list: [{ label: "m1", name: "MacBook Pro", pub: "A", online: true }, { label: "m2", name: "Build box", pub: "B", kind: "server" }] }),
    scoped: (key: string) => key,
    removeQuestion: (name: string) => `Remove ${name}?`,
  };
  await detectWay();
}

const tick = () => act(() => new Promise((r) => setTimeout(r, 0)));

// The shell around the page's routes: the inbox and the session are the real
// screens, the rest say their path.
async function renderShell(path: string, link: FakeLink) {
  const root = createRootRoute({ component: Shell });
  const plain = ["/sessions", "/computers", "/settings", "/usage", "/new"].map((p) =>
    createRoute({ getParentRoute: () => root, path: p, component: () => <div>{p}</div> }),
  );
  const router = createRouter({
    routeTree: root.addChildren([
      createRoute({ getParentRoute: () => root, path: "/inbox", component: Inbox }),
      createRoute({ getParentRoute: () => root, path: "/s/$session", component: SessionScreen, validateSearch: validateSessionSearch }),
      ...plain,
    ]),
    history: createMemoryHistory({ initialEntries: [path] }),
  });
  await router.load();
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const out = render(
    <StrictMode>
      <QueryClientProvider client={qc}>
        <LinkContext.Provider value={link}>
          <RouterProvider router={router} />
        </LinkContext.Provider>
      </QueryClientProvider>
    </StrictMode>,
  );
  await tick();
  return { ...out, router };
}


const names = (els: HTMLElement[]) => els.map((e) => e.getAttribute("aria-label") || e.textContent);

beforeEach(() => {
  localStorage.clear();
  (globalThis as any).RELAY = { scoped: (key: string) => key };
});
afterEach(() => {
  resetStart();
  vi.unstubAllGlobals();
  delete (globalThis as any).RELAY;
});

describe("the shell on a phone", () => {
  it("has the tabs at the bottom and the + in the top bar", async () => {
    widthIs(390);
    await account();
    await renderShell("/inbox", fakeLink({ inbox, host: "MacBook Pro" }));
    const tabs = screen.getByRole("navigation", { name: "Tabs" });
    expect(names(within(tabs).getAllByRole("button"))).toEqual(["Sessions", "Inbox", "Computers", "Settings"]);
    expect(within(tabs).queryByRole("button", { name: "New session" })).toBeNull();
    // What waits on you, counted in amber on the Inbox tab.
    expect(within(within(tabs).getByRole("button", { name: "Inbox" })).getByText("2").className).toBe("tbadge");
    const top = screen.getByRole("banner");
    expect(within(top).getByRole("button", { name: "New session" }).className).toContain("tplus");
    expect(within(top).getByText("MacBook Pro")).toBeTruthy();
  });

  it("has no Computers tab on the computer's own server", async () => {
    widthIs(390);
    await renderShell("/inbox", fakeLink({ inbox }));
    const tabs = screen.getByRole("navigation", { name: "Tabs" });
    expect(names(within(tabs).getAllByRole("button"))).toEqual(["Sessions", "Inbox", "Settings"]);
  });

  it("marks Settings while Usage is open", async () => {
    widthIs(390);
    await renderShell("/usage", fakeLink({ inbox }));
    const tabs = screen.getByRole("navigation", { name: "Tabs" });
    expect(within(tabs).getByRole("button", { name: "Settings" }).getAttribute("aria-current")).toBe("page");
  });

  it("shows a session as its own screen, with no tabs", async () => {
    widthIs(390);
    await renderShell("/s/a", fakeLink({ inbox }));
    expect(screen.queryByRole("navigation", { name: "Tabs" })).toBeNull();
  });
});

describe("the shell on a tablet", () => {
  it("has a rail with the tabs, the +, then a tile per live session", async () => {
    widthIs(834);
    await renderShell("/inbox", fakeLink({ inbox }));
    const rail = screen.getByRole("navigation", { name: "Tabs" });
    const buttons = names(within(rail).getAllByRole("button"));
    expect(buttons.slice(0, 5)).toEqual(["Inbox", "Sessions", "Usage", "Settings", "New session"]);
    expect(buttons).toContain("t-a, needs you");
    expect(buttons).toContain("t-d, needs you");
    expect(buttons).toContain("t-b, working");
    // A session that ended is no tile.
    expect(buttons.some((b) => b?.startsWith("t-c"))).toBe(false);
    // The inbox's list fills the rest.
    expect(screen.getByRole("heading", { name: "Inbox" })).toBeTruthy();
    expect(screen.queryByRole("banner")).toBeNull();
  });

  it("opens a session from its tile next to the rail", async () => {
    widthIs(834);
    const link = fakeLink({ inbox });
    const { router } = await renderShell("/inbox", link);
    fireEvent.click(screen.getByRole("button", { name: "t-b, working" }));
    await tick();
    expect(router.state.location.pathname).toBe("/s/b");
    expect(screen.getByRole("navigation", { name: "Tabs" })).toBeTruthy();
    expect(link.sent.filter((m) => (m as { type: string }).type === "open")).toEqual([{ type: "open", session: "b" }]);
  });
});

describe("the shell on a desktop", () => {
  it("has a sidebar with the tabs, New session and the sessions by state", async () => {
    widthIs(1440);
    await account();
    await renderShell("/s/b", fakeLink({ inbox, host: "MacBook Pro" }));
    const side = screen.getByRole("complementary");
    const tabs = within(side).getByRole("navigation", { name: "Tabs" });
    expect(names(within(tabs).getAllByRole("button"))).toEqual(["Inbox", "Sessions", "Usage", "Settings"]);
    const list = within(side).getByRole("region", { name: "Sessions" });
    const heads = [...list.querySelectorAll("h2")].map((h) => h.textContent);
    expect(heads).toEqual(["Needs you", "Working", "Done"]);
    expect(within(list).getAllByRole("button")[0].textContent).toContain("New session");
    expect(within(list).getByRole("button", { name: /t-b/ }).getAttribute("aria-current")).toBe("page");
    // The account at the bottom, and the computer picker opens the list of computers.
    expect(within(side).getByText("rik")).toBeTruthy();
    expect(within(side).getByRole("button", { name: "Sign out" })).toBeTruthy();
    fireEvent.click(within(side).getByRole("button", { name: /MacBook Pro/ }));
    const menu = await screen.findByRole("menu");
    expect(await within(menu).findByText("Build box")).toBeTruthy();
    expect(within(menu).getByRole("menuitem", { name: "Remove Build box" })).toBeTruthy();
    expect(within(menu).getByRole("menuitem", { name: "Sign out" })).toBeTruthy();
  });

  it("has no account and a plain computer label on the computer's own server", async () => {
    widthIs(1440);
    await renderShell("/s/b", fakeLink({ inbox, host: "MacBook Pro" }));
    const side = screen.getByRole("complementary");
    expect(within(side).queryByRole("button", { name: "Sign out" })).toBeNull();
    expect(within(side).getByText("MacBook Pro")).toBeTruthy();
    expect(within(side).queryByRole("button", { name: /MacBook Pro/ })).toBeNull();
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("shows the first session that needs you when none is open", async () => {
    widthIs(1440);
    const { router } = await renderShell("/inbox", fakeLink({ inbox }));
    await tick();
    expect(router.state.location.pathname).toBe("/s/a");
  });

  it("says to pick one when nothing needs you", async () => {
    widthIs(1440);
    const { router } = await renderShell("/inbox", fakeLink({ inbox: [row("b", "working")] as never }));
    expect(screen.getByText("Pick a session on the left.")).toBeTruthy();
    expect(router.state.location.pathname).toBe("/inbox");
  });

  it("goes from one session to another in the pane, closing the first, with the sidebar kept", async () => {
    widthIs(1440);
    const link = fakeLink({ inbox });
    await renderShell("/s/a", link);
    const side = screen.getByRole("complementary");
    fireEvent.click(within(side).getByRole("button", { name: /t-b/ }));
    await tick();
    expect(screen.getByRole("complementary")).toBe(side);
    const told = link.sent.map((m) => (m as { type: string; session?: string }).type + ((m as { session?: string }).session ?? ""));
    expect(told).toEqual(["opena", "openb"]);
    expect(link.getState().session?.id).toBe("b");
  });
});
