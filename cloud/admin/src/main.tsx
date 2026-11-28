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

// The Cloudflare Access session ended while the panel was open: whatever was
// pressed did not happen. Loading the page again lets Access sign the operator
// in. Before the page first loaded there is no operator yet: the root route
// shows the error itself, and loading again would only loop.
setOnSignedOut(() => {
  if (queryClient.getQueryData(meQuery.queryKey) === undefined) return;
  window.location.reload();
});

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      <RouterProvider router={router} />
    </StrictMode>,
  );
}
