// Linear delegation: Linear hands an issue to Arakun and the Worker passes it
// to the user's computer. Every path is under /linear.
//
//   GET /linear/link   → { links: [] }, for a signed-in user with the flag
//
// Without all four LINEAR_* secrets every /linear path is a 404. All routes
// but the webhook and the OAuth callback (which Linear calls) need a session
// with the `linear` flag.

import { session, type Session } from "./auth";
import { error, type Env } from "./util";

export const FLAG = "linear";

/// The ids Linear uses are UUIDs.
export const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

/// Whether the app is set up: a Worker without its secrets does not pretend
/// to take part.
export function configured(env: Env): boolean {
  return Boolean(env.LINEAR_CLIENT_ID && env.LINEAR_CLIENT_SECRET && env.LINEAR_WEBHOOK_SECRET && env.LINEAR_TOKEN_KEY);
}

/// The session, or the 401 or 403 to send.
export async function signedIn(request: Request, env: Env): Promise<Session | Response> {
  const s = await session(request, env, { flag: FLAG });
  if (!s) return error(401, "not signed in");
  if (s.blocked || !s.flagged) return error(403, "Linear is not available for this account");
  return s;
}

/// The answer for a /linear route, or null when the path is not one.
export async function linear(request: Request, env: Env, url: URL): Promise<Response | null> {
  const { pathname } = url;
  if (pathname !== "/linear" && !pathname.startsWith("/linear/")) return null;
  if (!configured(env)) return error(404, "not found");
  const key = `${request.method} ${pathname}`;
  // Linear's own calls carry no session; later tasks add them here.
  const who = await signedIn(request, env);
  if (who instanceof Response) return who;
  switch (key) {
    case "GET /linear/link":
      return Response.json({ links: [] });
    default:
      return error(404, "not found");
  }
}
