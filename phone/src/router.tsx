// The page's screens and the way between them. Hash history: both servers
// serve exact paths only, with no fallback to index.html, so the route lives
// after the `#`. The root asks `whereTo` before each screen, which is where
// `start()` in app.js decided what to show.

import { type QueryClient, useQueryClient } from "@tanstack/react-query";
import {
  createHashHistory,
  createRootRouteWithContext,
  createRoute,
  createRouter,
  Outlet,
  redirect,
  type Router,
  useRouter,
  useRouterState,
} from "@tanstack/react-router";
import { useEffect } from "react";
import { dropMachine, leaveMachine, pickedMachine, signedOut, useMe } from "./lib/account";
import { activeLink, subscribeActiveLink } from "./lib/api";
import type { LinkEvent } from "./lib/link";
import { mode, setPairMessage, tappedNotice, whereTo } from "./lib/start";
import { Toaster, toast } from "./lib/toast";
import { useViewportFit } from "./lib/viewport";
import { Resubscribe } from "./components/Resubscribe";
import { Computers } from "./routes/computers";
import { Fatal } from "./routes/fatal";
import { Inbox } from "./routes/inbox";
import { Moved } from "./routes/moved";
import { NewSession } from "./routes/new";
import { Offline } from "./routes/offline";
import { Pair } from "./routes/pair";
import { SessionScreen } from "./routes/session";
import { Sessions } from "./routes/sessions";
import { Settings } from "./routes/settings";
import { SignIn } from "./routes/signin";
import { Usage } from "./routes/usage";

export interface RouterContext {
  queryClient: QueryClient;
}

// The address of a session before the router: `#s=<id>`.
const OLD_SESSION = /^\/?s=([^/]+)$/;

const rootRoute = createRootRouteWithContext<RouterContext>()({
  beforeLoad: async ({ location, context }) => {
    const old = OLD_SESSION.exec(location.pathname);
    if (old) throw redirect({ to: "/s/$session", params: { session: decodeURIComponent(old[1]) }, replace: true });
    const to = await whereTo(location.pathname, context.queryClient);
    if (to) throw redirect({ to: to as "/", replace: true });
  },
  component: Root,
});

// Written out one by one: a helper would lose each path's type.
const routeTree = rootRoute.addChildren([
  createRoute({ getParentRoute: () => rootRoute, path: "/signin", component: SignIn }),
  createRoute({ getParentRoute: () => rootRoute, path: "/computers", component: Computers }),
  createRoute({ getParentRoute: () => rootRoute, path: "/inbox", component: Inbox }),
  createRoute({ getParentRoute: () => rootRoute, path: "/sessions", component: Sessions }),
  createRoute({ getParentRoute: () => rootRoute, path: "/s/$session", component: SessionScreen }),
  createRoute({ getParentRoute: () => rootRoute, path: "/new", component: NewSession }),
  createRoute({ getParentRoute: () => rootRoute, path: "/usage", component: Usage }),
  createRoute({ getParentRoute: () => rootRoute, path: "/settings", component: Settings }),
  createRoute({ getParentRoute: () => rootRoute, path: "/pair", component: Pair }),
  createRoute({ getParentRoute: () => rootRoute, path: "/moved", component: Moved }),
  createRoute({ getParentRoute: () => rootRoute, path: "/offline", component: Offline }),
  createRoute({ getParentRoute: () => rootRoute, path: "/fatal", component: Fatal }),
]);

export function createAppRouter(queryClient: QueryClient) {
  return createRouter({ routeTree, history: createHashHistory(), context: { queryClient } });
}

declare module "@tanstack/react-router" {
  interface Register {
    router: ReturnType<typeof createAppRouter>;
  }
}

type Go = (to: string) => void;

/** What the page does when the link says something the screen must act on. */
export function onLinkEvent(e: LinkEvent, qc: QueryClient, go: Go): void {
  switch (e.type) {
    case "toast":
      return toast(e.text);
    case "session-elsewhere":
      return go("/inbox");
    case "unpaired":
      // The computer let go of this phone, or never knew it.
      dropMachine();
      setPairMessage(e.message);
      return go("/pair");
    case "closed": {
      if (e.kind === "signed_out") {
        signedOut(qc);
        return go("/signin");
      }
      const name = pickedMachine()?.name || "The computer";
      const said =
        e.kind === "untrusted" ? `${name} did not answer with the key your account has for it. Sign in again in Arakun on that computer.` : e.detail;
      leaveMachine(qc, said);
      return go("/computers");
    }
  }
}

/** Follows the events of whichever link is in use; gives back how to stop. */
export function followLink(qc: QueryClient, go: Go): () => void {
  let off = () => {};
  const attach = () => {
    off();
    const link = activeLink();
    off = link ? link.subscribeEvents((e) => onLinkEvent(e, qc, go)) : () => {};
  };
  const unsubscribe = subscribeActiveLink(attach);
  attach();
  return () => {
    unsubscribe();
    off();
  };
}

function Root() {
  const router = useRouter() as Router<any, any>;
  const qc = useQueryClient();
  const path = useRouterState({ select: (s) => s.location.pathname });
  // The page takes the height the keyboard leaves, on every screen.
  useViewportFit();

  useEffect(() => {
    const go: Go = (to) => void router.navigate({ to });
    const stop = followLink(qc, go);
    // A tap on a notification while the page is open comes through the worker.
    const sw = "serviceWorker" in navigator ? navigator.serviceWorker : null;
    const onMessage = (ev: MessageEvent) => {
      if (ev.data && ev.data.open) tappedNotice(String(ev.data.open), router.state.location.pathname, qc, go);
    };
    sw?.addEventListener("message", onMessage);
    return () => {
      stop();
      sw?.removeEventListener("message", onMessage);
    };
  }, [router, qc]);

  // Signed out anywhere (a sign-out here, an answer saying the session is gone): only sign-in is left.
  const me = useMe(mode() === "account");
  const out = me.data?.screen === "signin";
  useEffect(() => {
    if (out && path !== "/signin") void router.navigate({ to: "/signin" });
  }, [out, path, router]);

  return (
    <>
      <Outlet />
      <Resubscribe />
      <Toaster />
    </>
  );
}
