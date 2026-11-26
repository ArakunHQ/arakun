import { b64url, cookie, fromB64url } from "./util";

// `__Host-` makes the browser refuse the cookie unless it is Secure, has
// Path=/ and no Domain, so no sibling host can set it.
export const SESSION_COOKIE = "__Host-arakun_admin";
export const SESSION_SECONDS = 12 * 60 * 60;
/// Between GitHub's answer and the code. The code step's cookie only lets the
/// browser give a code (POST /auth/code), or set up a first key
/// (worker/setup.ts), so it is short: time to install an app, no more.
export const CODE_STEP_COOKIE = "__Host-arakun_admin_code";
export const CODE_STEP_SECONDS = 10 * 60;

export interface Operator {
  githubId: number;
  login: string;
}

/// GitHub's login shape. The login goes into a header the panel forwards, so
/// nothing else may be signed or read back.
const LOGIN = /^[A-Za-z0-9-]{1,39}$/;

function validId(id: unknown): id is number {
  return typeof id === "number" && Number.isSafeInteger(id) && id > 0;
}

/// Whether the ID is in the comma-separated list. Unset, empty or all junk: nobody.
/// An entry that is not a positive safe integer is junk.
export function allowed(ids: string | undefined, githubId: number): boolean {
  return (ids ?? "").split(",").some((part) => {
    const text = part.trim();
    if (!/^\d+$/.test(text)) return false;
    const id = Number(text);
    return validId(id) && id === githubId;
  });
}

function hmacKey(secret: string, usage: "sign" | "verify"): Promise<CryptoKey> {
  return crypto.subtle.importKey("raw", new TextEncoder().encode(secret), { name: "HMAC", hash: "SHA-256" }, false, [usage]);
}

/// What a signed value is for. It goes into the MAC, so a value made for one
/// cookie never reads as the other: the code step's cookie is not a session.
type Purpose = "session" | "code";

function signed(purpose: Purpose, payload: string): Uint8Array<ArrayBuffer> {
  return new TextEncoder().encode(`${purpose}.${payload}`);
}

/// b64url(JSON { github_id, login, exp }) + "." + b64url(HMAC-SHA256 of the purpose, a dot and the first part).
async function sign(purpose: Purpose, op: Operator, secret: string, exp: number): Promise<string> {
  if (!validId(op.githubId) || !LOGIN.test(op.login)) throw new Error("not an operator GitHub would name");
  const payload = b64url(new TextEncoder().encode(JSON.stringify({ github_id: op.githubId, login: op.login, exp })));
  const mac = await crypto.subtle.sign("HMAC", await hmacKey(secret, "sign"), signed(purpose, payload));
  return `${payload}.${b64url(new Uint8Array(mac))}`;
}

/// The operator a value was signed for, or null: a bad signature, the wrong
/// purpose, expired, not on the list, or no SESSION_SECRET.
async function read(purpose: Purpose, value: string | null, env: Env, now: number): Promise<Operator | null> {
  if (!env.SESSION_SECRET) return null;
  const dot = value?.indexOf(".") ?? -1;
  if (!value || dot < 0) return null;
  const payload = value.slice(0, dot);
  const mac = fromB64url(value.slice(dot + 1));
  if (!mac) return null;
  // verify compares in constant time.
  const ok = await crypto.subtle.verify("HMAC", await hmacKey(env.SESSION_SECRET, "verify"), mac, signed(purpose, payload));
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
  if (!validId(github_id) || typeof login !== "string" || !LOGIN.test(login) || typeof exp !== "number") return null;
  if (now >= exp) return null;
  if (!allowed(env.ADMIN_GITHUB_IDS, github_id)) return null;
  return { githubId: github_id, login };
}

/// The session cookie's value. Only a right code from the operator's
/// authenticator app gets one (worker/auth.ts).
export function signSession(op: Operator, secret: string, now: number): Promise<string> {
  return sign("session", op, secret, now + SESSION_SECONDS);
}

/// The operator of a request, or null.
export function readSession(request: Request, env: Env, now: number): Promise<Operator | null> {
  return read("session", cookie(request, SESSION_COOKIE), env, now);
}

/// The code step's cookie value: GitHub said who this is, and the code is still to come.
export function signCodeStep(op: Operator, secret: string, now: number): Promise<string> {
  return sign("code", op, secret, now + CODE_STEP_SECONDS);
}

/// The operator waiting to give a code, or null.
export function readCodeStep(request: Request, env: Env, now: number): Promise<Operator | null> {
  return read("code", cookie(request, CODE_STEP_COOKIE), env, now);
}

export function codeStepCookie(value: string): string {
  return `${CODE_STEP_COOKIE}=${value}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=${CODE_STEP_SECONDS}`;
}

export function clearedCodeStepCookie(): string {
  return `${CODE_STEP_COOKIE}=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0`;
}

export function sessionCookie(value: string): string {
  return `${SESSION_COOKIE}=${value}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=${SESSION_SECONDS}`;
}

export function clearedCookie(): string {
  return `${SESSION_COOKIE}=; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=0`;
}
