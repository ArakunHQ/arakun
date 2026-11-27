// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createMemoryHistory, createRouter } from "@tanstack/react-router";
import { render } from "@testing-library/react";
import { routeTree } from "./routeTree.gen";

/// Renders the whole panel at `path` on a memory history and a fresh cache,
/// the way main.tsx builds it, so a route test goes through the root's
/// sign-in check too.
export function renderRoute(path: string) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createRouter({
    routeTree,
    context: { queryClient },
    history: createMemoryHistory({ initialEntries: [path] }),
    defaultPreloadStaleTime: 0,
    Wrap: ({ children }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>,
  });
  render(<RouterProvider router={router} />);
  return { router, queryClient };
}

/// jsdom has no modal <dialog> methods and no scrolling.
export function stubDialog(): void {
  // The router restores scroll positions; jsdom has no scrolling.
  window.scrollTo = () => {};
  HTMLDialogElement.prototype.showModal = function (this: HTMLDialogElement) {
    this.open = true;
  };
  HTMLDialogElement.prototype.close = function (this: HTMLDialogElement) {
    this.open = false;
  };
}
