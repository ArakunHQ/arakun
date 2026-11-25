import { b64url, cookie, fromB64url } from "./util";

// `__Host-` makes the browser refuse the cookie unless it is Secure, has
// Path=/ and no Domain, so no sibling host can set it.
export const SESSION_COOKIE = "__Host-arakun_admin";
export const SESSION_SECONDS = 12 * 60 * 60;

export interface Operator {
  githubId: number;
  login: string;
}

/// Whether the ID is in the comma-separated list. Unset, empty or all junk: nobody.
export function allowed(ids: string | undefined, githubId: number): boolean {
  return (ids ?? "").split(",").some((part) => /^\d+$/.test(part.trim()) && Number(part.trim()) === githubId);
}

function hmacKey(secret: string, usage: "sign" | "verify"): Promise<CryptoKey> {
  return crypto.subtle.importKey("raw", new TextEncoder().encode(secret), { name: "HMAC", hash: "SHA-256" }, false, [usage]);
}

/// The cookie's value: b64url(JSON { github_id, login, exp }) + "." + b64url(HMAC-SHA256 of the first part).
export async function signSession(op: Operator, secret: string, now: number): Promise<string> {
  const payload = b64url(new TextEncoder().encode(JSON.stringify({ github_id: op.githubId, login: op.login, exp: now + SESSION_SECONDS })));
  const mac = await crypto.subtle.sign("HMAC", await hmacKey(secret, "sign"), new TextEncoder().encode(payload));
  return `${payload}.${b64url(new Uint8Array(mac))}`;
}

/// The operator of a request, or null: no cookie, a bad signature, expired, not on the list, or no SESSION_SECRET.
export async function readSession(request: Request, env: Env, now: number): Promise<Operator | null> {
  if (!env.SESSION_SECRET) return null;
  const value = cookie(request, SESSION_COOKIE);
  const dot = value?.indexOf(".") ?? -1;
  if (!value || dot < 0) return null;
  const payload = value.slice(0, dot);
  const mac = fromB64url(value.slice(dot + 1));
  if (!mac) return null;
  // verify compares in constant time.
  const ok = await crypto.subtle.verify("HMAC", await hmacKey(env.SESSION_SECRET, "verify"), mac, new TextEncoder().encode(payload));
  if (!ok) return null;
  const bytes = fromB64url(payload);
  if (!bytes) return null;
  let claims: unknown;
  try {
    claims = JSON.parse(new TextDecoder().decode(bytes));
  } catch {
    return null;
  }
  if (typeof claims !== "object" || claims === null) return null;
  const { github_id, login, exp } = claims as Record<string, unknown>;
  if (typeof github_id !== "number" || typeof login !== "string" || typeof exp !== "number") return null;
  if (now >= exp) return null;
  if (!allowed(env.ADMIN_GITHUB_IDS, github_id)) return null;
  return { githubId: github_id, login };
}

export function sessionCookie(value: string): string {
  return `${SESSION_COOKIE}=${value}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=${SESSION_SECONDS}`;
}

export function clearedCookie(): string {
  return `${SESSION_COOKIE}=; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=0`;
}
