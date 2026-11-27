// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// First: in dev, the Worker's page policy blocks the inline script that sets
// up React's hot reload, so this module does it. In a build it is empty.
import "@vitejs/plugin-react/preamble";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider, createRouter } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { ApiError, setOnSignedOut } from "./api";
import { meQuery } from "./queries";
import { routeTree } from "./routeTree.gen";
import "./styles/tokens.css";
import "./styles/app.css";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // The server said no, and asking again gets the same answer.
      retry: (failures, error) => !(error instanceof ApiError && error.status < 500) && failures < 2,
    },
  },
});

const router = createRouter({
  routeTree,
  context: { queryClient },
  // Query decides when data is stale, not the router.
  defaultPreloadStaleTime: 0,
  scrollRestoration: true,
  Wrap: ({ children }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>,
});

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}

// The session ended while the panel was open: whatever was pressed did not
// happen. Before the first sign-in there is no operator yet: the root route
// sends the visit to sign-in itself, and clearing the cache would cancel the
// very call it waits on.
setOnSignedOut(() => {
  if (queryClient.getQueryData(meQuery.queryKey) === undefined) return;
  queryClient.clear();
  void router.navigate({ to: "/sign-in", search: { signed_out: 1 } });
});

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      <RouterProvider router={router} />
    </StrictMode>,
  );
}
