// The screens with made-up sessions and no computer, for screenshots of the
// look: `npm run dev`, then open /demo.html#/inbox, #/s/a or #/signin, at any
// width. /demo.html?account#/inbox plays the account host, signed in with two
// computers. Add &cloud for an account with a cloud environment, &cloud=ask for
// one that may ask for access. Only demo.html loads this; the build has index.html alone, so it
// never ships.
import "./styles/tokens.css";
import "./styles/base.css";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createHashHistory, createRootRoute, createRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { Shell } from "./components/shell/Shell";
import { validateSessionSearch } from "./components/session/sheetNav";
import type { Link } from "./lib/link";
import { LinkContext } from "./lib/link";
import { emptyLink, type InboxSession, type LinkState, type Session } from "./lib/model";
import { detectWay } from "./lib/start";
import { Computers } from "./routes/computers";
import { Toaster } from "./lib/toast";
import { Inbox } from "./routes/inbox";
import { SessionScreen } from "./routes/session";
import { Sessions } from "./routes/sessions";
import { Settings } from "./routes/settings";
import { SignIn } from "./routes/signin";
import { Usage } from "./routes/usage";

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
      // Opening a row shows the made-up session, under that row's title; only
      // the first one asks for a permission.
      const msg = m as { type: string; session?: string };
      if (msg.type === "open") {
        const row = inbox.find((r) => r.id === msg.session);
        state = { ...state, session: msg.session === "a" || !row ? session : ({ ...session, ...row, permission: null } as Session) };
      }
      for (const fn of listeners) fn();
    },
    fetch: () => Promise.reject(new Error("demo")),
    reconnectNow() {},
    retryNow() {},
    watchNotice() {},
    setMe() {},
    close() {},
  };
}

const root = createRootRoute({
  component: () => (
    <>
      <Shell />
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
    createRoute({ getParentRoute: () => root, path: "/settings", component: Settings }),
    createRoute({ getParentRoute: () => root, path: "/usage", component: Usage }),
    createRoute({ getParentRoute: () => root, path: "/computers", component: Computers }),
  ]),
});

// The account host, made up: relay.js's answers, without the network.
if (new URLSearchParams(location.search).has("account")) {
  const cloud = new URLSearchParams(location.search).get("cloud");
  const machines = [
    { label: "m1", name: "MacBook Pro", pub: "A", online: true },
    { label: "m2", name: "Build box", pub: "B", kind: "server", last_seen_at: now - 7200 },
    ...(cloud === "" ? [{ label: "abcdefghijklmnop", name: "Cloud", pub: "C", kind: "server", last_seen_at: now - 1800 }] : []),
  ];
  const flags = cloud === "" ? ["cloud"] : [];
  // The cloud routes, which the page asks the account host for itself.
  const hour = 3_600_000;
  const answers: Record<string, unknown> = {
    "/cloud/access": { status: "none", requested_at: null, decided_at: null, retry_at: null },
    "/cloud/machines": [{ label: "abcdefghijklmnop", name: "Cloud", running: false, snapshot: true, last_active: Date.now() - 1_800_000, task_ms: 2 * hour, stopped: "task_cap" }],
    "/cloud/usage": { month: "2026-11", used_ms: 6.5 * hour, allowance_ms: 20 * hour, left_ms: 13.5 * hour, task_cap_ms: 2 * hour, stopped: null, message: null },
  };
  const real = fetch;
  globalThis.fetch = async (input, init) => {
    const path = typeof input === "string" ? input : input instanceof URL ? input.pathname : input.url;
    if (cloud !== null && path in answers) return Response.json(answers[path]);
    return real(input, init);
  };
  Object.assign((globalThis as unknown as { RELAY: object }).RELAY, {
    detect: async () => "account",
    boot: async () => ({ screen: "ok", me: { login: "rikvisser", notices: [], flags }, machines }),
    machines: async () => ({ list: machines }),
    sessions: async () => ({ list: [] }),
  });
  await detectWay();
}

createRoot(document.getElementById("app")!).render(
  <StrictMode>
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      <LinkContext.Provider value={demoLink({ session })}>
        <RouterProvider router={router} />
      </LinkContext.Provider>
    </QueryClientProvider>
  </StrictMode>,
);
