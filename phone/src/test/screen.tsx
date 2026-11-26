// Renders one screen the way the page does: inside a router and a link, with
// a fake link whose state the test sets.

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { act, render } from "@testing-library/react";
import type { ReactNode } from "react";
import { vi } from "vitest";
import { validateSessionSearch } from "../components/session/sheetNav";
import { type Link, LinkContext } from "../lib/link";
import { closeSession, emptyLink, type LinkEvent, type LinkState, openSession } from "../lib/model";

export type FakeLink = Link & {
  sent: unknown[];
  /** Changes the state as a message from the computer would. */
  set(over: Partial<LinkState>): void;
  emit(e: LinkEvent): void;
};

export function fakeLink(over: Partial<LinkState> = {}): FakeLink {
  let state: LinkState = { ...emptyLink(), connected: true, ...over };
  const sent: unknown[] = [];
  const listeners = new Set<() => void>();
  const eventListeners = new Set<(e: LinkEvent) => void>();
  const set = (next: LinkState) => {
    state = next;
    for (const fn of listeners) fn();
  };
  return {
    sent,
    subscribe: (fn: () => void) => {
      listeners.add(fn);
      return () => listeners.delete(fn);
    },
    subscribeEvents: (fn: (e: LinkEvent) => void) => {
      eventListeners.add(fn);
      return () => eventListeners.delete(fn);
    },
    getState: () => state,
    // As the real link: `open` and `close` change the session in the state too.
    send: vi.fn((m: { type: string; session?: unknown }) => {
      sent.push(m);
      if (m.type === "open" && typeof m.session === "string") set(openSession(state, m.session));
      else if (m.type === "close") set(closeSession(state));
    }),
    fetch: vi.fn(),
    reconnectNow() {},
    retryNow() {},
    watchNotice: vi.fn(),
    setMe(patch: object) {
      act(() => set({ ...state, me: { ...state.me, ...patch } as never }));
    },
    close() {},
    set(o: Partial<LinkState>) {
      act(() => set({ ...state, ...o }));
    },
    emit(e: LinkEvent) {
      act(() => {
        for (const fn of eventListeners) fn(e);
      });
    },
  } as unknown as FakeLink;
}

const PATHS = ["/inbox", "/sessions", "/usage", "/settings", "/new", "/computers", "/s/$session"];

// Whether `path` is an address of route `p`, such as /s/a of /s/$session.
const matches = (p: string, path: string) => new RegExp(`^${p.replace(/\$\w+/g, "[^/]+")}$`).test(path);

/** `screen` shown at `path`; the other screens are empty. */
export async function renderScreen(
  screen: ReactNode,
  path: string,
  link: Link,
  wrap: (n: ReactNode) => ReactNode = (n) => n,
  qc = new QueryClient({ defaultOptions: { queries: { retry: false } } }),
) {
  const root = createRootRoute();
  const routes = PATHS.map((p) =>
    createRoute({
      getParentRoute: () => root,
      path: p,
      component: () => (matches(p, path) ? <>{screen}</> : <div>{p}</div>),
      ...(p === "/s/$session" ? { validateSearch: validateSessionSearch } : {}),
    }),
  );
  const router = createRouter({ routeTree: root.addChildren(routes), history: createMemoryHistory({ initialEntries: [path] }) });
  await router.load();
  const out = render(
    wrap(
      <QueryClientProvider client={qc}>
        <LinkContext.Provider value={link}>
          <RouterProvider router={router} />
        </LinkContext.Provider>
      </QueryClientProvider>,
    ),
  );
  return { ...out, router };
}

/** The window as wide as `px`: what `matchMedia` answers for a min-width query. */
export function widthIs(px: number) {
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

/**
 * The page's CSS as a window of `size` applies it (a number is a width, as
 * tall as a phone), put in the document. jsdom measures its queries against
 * its own window, 1024 by 768 px, so each min-width and max-height query
 * becomes one that always or never holds. Gives back how to take it out.
 */
export function styleAt(size: number | { width: number; height: number }, ...css: string[]): () => void {
  const { width, height } = typeof size === "number" ? { width: size, height: 844 } : size;
  const el = document.createElement("style");
  el.textContent = css
    .join("\n")
    .replace(/@media\s*\((min-width|max-height):\s*(\d+)px\)/g, (_, kind, n) => {
      const holds = kind === "min-width" ? width >= Number(n) : height <= Number(n);
      return holds ? "@media screen" : "@media not all";
    });
  document.head.append(el);
  return () => el.remove();
}
