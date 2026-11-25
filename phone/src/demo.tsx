// The screens with made-up sessions and no computer, for screenshots of the
// look: `npm run dev`, then open /demo.html#/inbox, #/s/a or #/signin. Only
// demo.html loads this; the build has index.html alone, so it never ships.
import "./styles/tokens.css";
import "./styles/base.css";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createHashHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { validateSessionSearch } from "./components/session/sheetNav";
import type { Link } from "./lib/link";
import { LinkContext } from "./lib/link";
import { emptyLink, type InboxSession, type LinkState, type Session } from "./lib/model";
import { Toaster } from "./lib/toast";
import { Inbox } from "./routes/inbox";
import { SessionScreen } from "./routes/session";
import { Sessions } from "./routes/sessions";
import { SignIn } from "./routes/signin";

const now = Math.floor(Date.now() / 1000);
const inbox: InboxSession[] = [
  { id: "a", title: "Fix the relay reconnect", agent: "claude", status: "needs_input", project: "arakun", env: "env/brave-otter", updated: now - 60, asks: { kind: "permission", text: "cargo test --locked relay" } },
  { id: "b", title: "Pairing code expiry", agent: "codex", status: "error", project: "arakun", env: "feat/pair-expiry", updated: now - 240 },
  { id: "c", title: "Phone tab empty state", agent: "claude", status: "working", project: "arakun", env: "env/quiet-heron", updated: now - 5, added: 24, removed: 6 },
  { id: "d", title: "Deploy the relay worker", agent: "droid", status: "working", project: "relay", env: "main", updated: now - 20 },
  { id: "e", title: "Windows ISO build", agent: "codex", status: "idle", project: "iso", env: "main", updated: now - 3600, added: 3, removed: 1 },
] as InboxSession[];

const session: Session = {
  id: "a",
  title: "Fix the relay reconnect",
  agent: "claude",
  status: "needs_input",
  count: 6,
  from: 0,
  place: "arakun · env/brave-otter",
  question: null,
  permission: {
    tool_call_id: "t3",
    title: "cargo test --locked relay",
    options: [
      { id: "yes", name: "Allow", kind: "allow_once" },
      { id: "always", name: "Always allow", kind: "allow_always" },
      { id: "no", name: "Deny", kind: "reject_once" },
    ],
  },
  entries: [
    { type: "user", text: "The phone drops off the relay after a minute idle. Find out why and fix it.", at: now - 600 },
    { type: "agent", text: "The relay closes a socket after 60 s without traffic. A phone that only reads sends nothing, so it gets cut." },
    { type: "tool", status: "completed", title: "Edited relay/src/socket.ts", added: 24, removed: 6 },
    { type: "tool", status: "completed", title: "Edited phone/relay.js", added: 11, removed: 2 },
    { type: "agent", text: "I added a ping every 25 s and a reconnect with backoff. Now I want to run the relay tests." },
    { type: "tool", status: "in_progress", command: "cargo test --locked relay", kind: "execute" },
  ],
};

function demoLink(over: Partial<LinkState>): Link {
  let state: LinkState = { ...emptyLink(), connected: true, host: "MacBook Pro", inbox, places: [{ path: "/src/arakun", project: "arakun", branch: "main" }], ...over };
  const listeners = new Set<() => void>();
  return {
    subscribe: (fn) => (listeners.add(fn), () => listeners.delete(fn)),
    subscribeEvents: () => () => {},
    getState: () => state,
    send: (m) => {
      // Opening a row shows the one made-up session.
      if ((m as { type: string }).type === "open") state = { ...state, session };
      for (const fn of listeners) fn();
    },
    fetch: () => Promise.reject(new Error("demo")),
    reconnectNow() {},
    retryNow() {},
    watchNotice() {},
    close() {},
  };
}

const root = createRootRoute({
  component: () => (
    <>
      <Outlet />
      <Toaster />
    </>
  ),
});
const router = createRouter({
  history: createHashHistory(),
  routeTree: root.addChildren([
    createRoute({ getParentRoute: () => root, path: "/signin", component: SignIn }),
    createRoute({ getParentRoute: () => root, path: "/inbox", component: Inbox }),
    createRoute({ getParentRoute: () => root, path: "/sessions", component: Sessions }),
    createRoute({ getParentRoute: () => root, path: "/s/$session", component: SessionScreen, validateSearch: validateSessionSearch }),
  ]),
});

createRoot(document.getElementById("app")!).render(
  <StrictMode>
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      <LinkContext.Provider value={demoLink({ session })}>
        <RouterProvider router={router} />
      </LinkContext.Provider>
    </QueryClientProvider>
  </StrictMode>,
);
