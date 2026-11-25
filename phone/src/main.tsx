import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { StrictMode, useSyncExternalStore } from "react";
import { createRoot } from "react-dom/client";
import { activeLink, subscribeActiveLink } from "./lib/api";
import { LinkContext } from "./lib/link";
import { createAppRouter } from "./router";
import "./styles/legacy.css";

const queryClient = new QueryClient();
const router = createAppRouter(queryClient);

// The service worker lets the page go on the home screen and hears pushes.
// Both need a secure origin; over plain http on the LAN there is neither.
if (isSecureContext && "serviceWorker" in navigator) navigator.serviceWorker.register("sw.js").catch(() => {});

function App() {
  const link = useSyncExternalStore(subscribeActiveLink, activeLink);
  return (
    <QueryClientProvider client={queryClient}>
      <LinkContext.Provider value={link}>
        <RouterProvider router={router} />
      </LinkContext.Provider>
    </QueryClientProvider>
  );
}

createRoot(document.getElementById("app")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
