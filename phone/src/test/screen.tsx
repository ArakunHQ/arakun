// Renders one screen the way the page does: inside a router and a link, with
// a fake link whose state the test sets.

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { render } from "@testing-library/react";
import type { ReactNode } from "react";
import { vi } from "vitest";
import { type Link, LinkContext } from "../lib/link";
import { emptyLink, type LinkState } from "../lib/model";

export function fakeLink(over: Partial<LinkState> = {}): Link & { sent: unknown[] } {
  const state = { ...emptyLink(), connected: true, ...over };
  const sent: unknown[] = [];
  return {
    sent,
    subscribe: () => () => {},
    subscribeEvents: () => () => {},
    getState: () => state,
    send: vi.fn((m) => void sent.push(m)),
    fetch: vi.fn(),
    reconnectNow() {},
    retryNow() {},
    watchNotice: vi.fn(),
    close() {},
  } as unknown as Link & { sent: unknown[] };
}

const PATHS = ["/inbox", "/sessions", "/usage", "/settings", "/new", "/s/$session"];

/** `screen` shown at `path`; the other screens are empty. */
export async function renderScreen(screen: ReactNode, path: string, link: Link) {
  const root = createRootRoute();
  const routes = PATHS.map((p) => createRoute({ getParentRoute: () => root, path: p, component: () => (p === path ? <>{screen}</> : <div>{p}</div>) }));
  const router = createRouter({ routeTree: root.addChildren(routes), history: createMemoryHistory({ initialEntries: [path] }) });
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  await router.load();
  const out = render(
    <QueryClientProvider client={qc}>
      <LinkContext.Provider value={link}>
        <RouterProvider router={router} />
      </LinkContext.Provider>
    </QueryClientProvider>,
  );
  return { ...out, router };
}
