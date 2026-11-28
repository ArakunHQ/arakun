// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
import type { QueryClient } from "@tanstack/react-query";
import { Link, Outlet, createRootRouteWithContext, useRouter, type ErrorComponentProps, type LinkProps } from "@tanstack/react-router";
import { meQuery } from "../queries";
import type { Me } from "../types";

export interface RouterContext {
  queryClient: QueryClient;
}

/// The screens in the navigation. Each task that adds a screen adds it here,
/// so the list never links to a route that does not exist.
export const NAV: { to: LinkProps["to"]; label: string }[] = [
  { to: "/", label: "Cloud access" },
  { to: "/beta", label: "Phone betas" },
  { to: "/users", label: "Users" },
  { to: "/machines", label: "Machines" },
  { to: "/notices", label: "Notices" },
  { to: "/activity", label: "Activity" },
];

export const Route = createRootRouteWithContext<RouterContext>()({
  // Cloudflare Access signs the operator in before the page loads. A 401 here
  // means the Worker did not accept what Access sent, and the error screen
  // says so.
  beforeLoad: async ({ context }) => {
    const me: Me = await context.queryClient.ensureQueryData(meQuery);
    return { me };
  },
  component: Shell,
  errorComponent: Failed,
  notFoundComponent: NotFound,
});

function Shell() {
  const me = Route.useRouteContext({ select: (c) => c.me });
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
        <div className="operator">
          <span className="operator-login">{me.email}</span>
          {/* Cloudflare Access answers this path itself and ends its session. */}
          <a className="button quiet" href="/cdn-cgi/access/logout">
            Sign out
          </a>
        </div>
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
