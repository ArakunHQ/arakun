import type { QueryClient } from "@tanstack/react-query";
import {
  Link,
  Outlet,
  createRootRouteWithContext,
  redirect,
  useRouter,
  useRouterState,
  type ErrorComponentProps,
  type LinkProps,
} from "@tanstack/react-router";
import { ApiError } from "../api";
import { meQuery, useLogout } from "../queries";
import type { Me } from "../types";

export interface RouterContext {
  queryClient: QueryClient;
}

/// The screens in the navigation. Each task that adds a screen adds it here,
/// so the list never links to a route that does not exist.
export const NAV: { to: LinkProps["to"]; label: string }[] = [
  { to: "/", label: "Cloud access" },
  { to: "/users", label: "Users" },
  { to: "/machines", label: "Machines" },
  { to: "/notices", label: "Notices" },
  { to: "/activity", label: "Activity" },
];

export const Route = createRootRouteWithContext<RouterContext>()({
  beforeLoad: async ({ context, location }) => {
    if (location.pathname === "/sign-in") return { me: null };
    // The Worker sends GitHub's sign-in back to / with the outcome: the
    // sign-in screen says what went wrong.
    const signIn = (location.search as { sign_in?: unknown }).sign_in;
    if (signIn === "denied" || signIn === "failed") throw redirect({ to: "/sign-in", search: { sign_in: signIn } });
    try {
      const me: Me | null = await context.queryClient.ensureQueryData(meQuery);
      return { me };
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) throw redirect({ to: "/sign-in" });
      throw e;
    }
  },
  component: Root,
  errorComponent: Failed,
  notFoundComponent: NotFound,
});

function Root() {
  const signingIn = useRouterState({ select: (s) => s.location.pathname === "/sign-in" });
  return signingIn ? <Outlet /> : <Shell />;
}

function Shell() {
  const me = Route.useRouteContext({ select: (c) => c.me });
  const logout = useLogout();
  return (
    <div className="shell">
      <header className="bar">
        <Link to="/" className="wordmark">
          Arakun <span className="wordmark-sub">admin</span>
        </Link>
        <nav className="nav" aria-label="Screens">
          {NAV.map((item) => (
            <Link key={String(item.to)} to={item.to} className="nav-item" activeOptions={{ exact: item.to === "/", includeSearch: false }}>
              {item.label}
            </Link>
          ))}
        </nav>
        {me && (
          <div className="operator">
            <img className="avatar" src={`https://avatars.githubusercontent.com/u/${me.github_id}?s=48`} alt="" />
            <span className="operator-login">{me.login}</span>
            <button type="button" className="button quiet" disabled={logout.isPending} onClick={() => logout.mutate()}>
              Sign out
            </button>
          </div>
        )}
        {logout.error && (
          <p className="message" data-tone="error" role="alert">
            {logout.error.message}
          </p>
        )}
      </header>
      <main className="main">
        <Outlet />
      </main>
    </div>
  );
}

function Failed({ error }: ErrorComponentProps) {
  const router = useRouter();
  return (
    <main className="main">
      <h1>The page did not load</h1>
      <p className="message" data-tone="error" role="alert">
        {error instanceof Error ? error.message : String(error)}
      </p>
      <button type="button" className="button" onClick={() => void router.invalidate()}>
        Try again
      </button>
    </main>
  );
}

function NotFound() {
  return (
    <>
      <h1>No page here</h1>
      <p>
        <Link to="/">Go to Cloud access</Link>
      </p>
    </>
  );
}
