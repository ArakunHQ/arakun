// What would be an HTTP request to the computer, as `api()` in app.js: the
// link in use knows how (plain fetch with the token on the computer's own
// server, a request through the relay otherwise). This only knows which link
// that is.

import type { Link } from "./link";

let active: Link | null = null;
const listeners = new Set<() => void>();

/** The link the page uses from now on, or null when there is none. */
export function setActiveLink(link: Link | null): void {
  if (active === link) return;
  active = link;
  for (const fn of listeners) fn();
}

export function activeLink(): Link | null {
  return active;
}

export function subscribeActiveLink(fn: () => void): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}

export function api(path: string, init?: RequestInit): Promise<Response> {
  if (!active) return Promise.reject(new Error("not connected"));
  return active.fetch(path, init);
}
