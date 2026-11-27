// Sign-in with a six-digit code mailed to the address, for people without
// GitHub. The code is typed where it was asked for: in the app, or on the
// page. It is not a link: a mail scanner may open links, a link could sign in
// whatever browser opens it, and the desktop app could not receive one.
//
//   POST /auth/email          { email }                       → { sent: true, expires_in }
//   POST /auth/email/verify   { email, code, name?, kind? }   → the answer of POST /auth/github
//   POST /auth/email/web      { email, code }                 → 204 with the session cookie
//
// A code lasts 10 minutes, works once and takes 5 tries. Each address gets at
// most 5 codes an hour, so at most 25 guesses an hour at its codes, and each
// client IP a few codes a minute. Nothing in any answer says whether the
// address has an account: a code goes out either way, and a right code signs
// in to the account or makes one.

import { endReplacedSession, platform, withSessionCookie } from "./web_auth";
import { KINDS, MAX_NAME, newSession, signedIn, signInAllowed } from "./auth";
import { disposable } from "./disposable";
import { bodyText, error, hex, sha256, now, type Env } from "./util";

export const CODE_SECONDS = 600;
export const MAX_TRIES = 5;
export const MAX_SENDS = 5;
export const SEND_WINDOW = 3600;
/// RFC 5321's limit on a path, which is also what the column of beta sign-ups takes.
const MAX_ADDRESS = 254;
const MAX_BODY = 2048;
/// One `@`, something on each side, and a dot in the domain; no spaces or
/// control characters. The mail service is the real test: a code it cannot
/// deliver is never typed.
const ADDRESS = /^[^\s@\p{Cc}]+@[^\s@\p{Cc}]+\.[^\s@\p{Cc}]+$/u;
const DOMAIN = /^[a-z0-9-]+(\.[a-z0-9-]+)+$/;
const CODE = /^[0-9]{6}$/;
const WRONG_CODE = "the code is wrong or has run out; ask for a new one";

export async function emailAuth(request: Request, env: Env, url: URL): Promise<Response | null> {
  if (request.method !== "POST") return null;
  switch (url.pathname) {
    case "/auth/email":
      return sendCode(request, env);
    case "/auth/email/verify":
      return verify(request, env, false);
    case "/auth/email/web":
      return verify(request, env, true);
    default:
      return null;
  }
}

/// The address as it is kept and compared: trimmed and in lower case. Mail
/// providers treat the local part as one in any case, though the standard
/// lets them not. Null if it does not look like an address.
export function normalAddress(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const address = value.trim().toLowerCase();
  if (address.length > MAX_ADDRESS || !ADDRESS.test(address)) return null;
  const domain = address.slice(address.lastIndexOf("@") + 1);
  return DOMAIN.test(domain) ? address : null;
}

async function sendCode(request: Request, env: Env): Promise<Response> {
  // Each code is a mail sent, so the limit per IP is lower than sign-in's.
  const ip = request.headers.get("CF-Connecting-IP") ?? "unknown";
  if (!(await env.EMAIL_CODE_LIMIT.limit({ key: ip })).success) return error(429, "too many codes asked for, try again in a minute");
  const body = await readBody(request);
  if (body === "too big") return error(413, "the body is too big");
  const address = normalAddress(body?.["email"]);
  if (!address) return error(400, 'expected {"email": "..."} with an email address');
  if (await disposable(address.slice(address.lastIndexOf("@") + 1), env.CONFIG)) {
    return error(400, "that address is from a throwaway mail service; use one you keep");
  }
  if (!env.EMAIL || !env.EMAIL_FROM) return error(503, "sign-in by email is not set up");

  const t = now();
  const code = newCode();
  const [, saved] = await env.DB.batch([
    // Rows whose code and window are both over, so the table holds only the
    // last hour's addresses.
    env.DB.prepare("DELETE FROM email_codes WHERE window_start <= ? AND expires_at <= ?").bind(t - SEND_WINDOW, t),
    // A new code replaces the one before, and its tries start again. Past
    // the sends of the window, nothing changes and no mail goes; a window
    // that is over starts again with this send.
    env.DB.prepare(
      `INSERT INTO email_codes (address, code_hash, expires_at, tries, sends, window_start) VALUES (?1, ?2, ?3, 0, 1, ?4)
       ON CONFLICT (address) DO UPDATE SET code_hash = ?2, expires_at = ?3, tries = 0,
         sends = CASE WHEN window_start <= ?4 - ?6 THEN 1 ELSE sends + 1 END,
         window_start = CASE WHEN window_start <= ?4 - ?6 THEN ?4 ELSE window_start END
       WHERE sends < ?5 OR window_start <= ?4 - ?6`,
    ).bind(address, await codeHash(address, code), t + CODE_SECONDS, t, MAX_SENDS, SEND_WINDOW),
  ]);
  if (saved!.meta.changes === 0) return error(429, "too many codes for this address, try again in an hour");

  try {
    await env.EMAIL.send(codeMail(env.EMAIL_FROM, address, code));
  } catch (e) {
    // The code and the address stay out of the log.
    console.error("sending a sign-in code failed:", (e as { code?: unknown }).code ?? (e as Error).message);
    return error(502, "could not send the code, try again");
  }
  return Response.json({ sent: true, expires_in: CODE_SECONDS });
}

async function verify(request: Request, env: Env, web: boolean): Promise<Response> {
  // The page's sign-in sets the cookie, so it must come from the page: a
  // foreign page could otherwise sign the browser in to the attacker's
  // account with a code the attacker asked for.
  if (web && request.headers.get("Origin") !== new URL(request.url).origin) return error(403, "the request comes from another origin");
  if (!(await signInAllowed(request, env))) return error(429, "too many sign-ins, try again in a minute");
  const body = await readBody(request);
  if (body === "too big") return error(413, "the body is too big");
  const address = normalAddress(body?.["email"]);
  const rawCode = body?.["code"];
  // People copy the code with a space in the middle, or one around it.
  const code = typeof rawCode === "string" ? rawCode.replace(/\s/g, "") : null;
  if (!address || code === null || !CODE.test(code)) return error(400, 'expected {"email": "...", "code": "123456"}');
  let name: string | null = null;
  let kind = "app";
  if (web) kind = "phone";
  else {
    const { name: n, kind: k } = body!;
    if (n !== undefined && (typeof n !== "string" || n.trim().length === 0 || n.length > MAX_NAME)) return error(400, `name must be 1 to ${MAX_NAME} characters`);
    if (k !== undefined && (typeof k !== "string" || !KINDS.includes(k))) return error(400, `kind must be one of ${KINDS.join(", ")}`);
    name = n === undefined ? null : n.trim();
    if (k !== undefined) kind = k;
  }

  if (!(await useCode(env, address, code))) return error(401, WRONG_CODE);
  const made = await newSession(env, { provider: "email", subject: address, login: address, email: address }, web ? platform(request.headers.get("User-Agent") ?? "") : (name ?? address), kind);
  if (!web) return signedIn(env, made);
  await endReplacedSession(request, env, made.account_id, made.session);
  return withSessionCookie(new Response(null, { status: 204 }), made.session);
}

/// Whether the code is the address's live one. Every try counts, right or
/// wrong, before the code is compared, so tries at once cannot pass the limit.
/// A right code is used up in a second write that only one request wins.
async function useCode(env: Env, address: string, code: string): Promise<boolean> {
  const t = now();
  const row = await env.DB.prepare(
    "UPDATE email_codes SET tries = tries + 1 WHERE address = ? AND code_hash IS NOT NULL AND expires_at > ? AND tries < ? RETURNING code_hash",
  )
    .bind(address, t, MAX_TRIES)
    .first<{ code_hash: string }>();
  // Hashes are all one length, and the right one is never sent anywhere, so
  // comparing them leaks nothing.
  const hash = await codeHash(address, code);
  if (!row || row.code_hash !== hash) return false;
  const used = await env.DB.prepare("UPDATE email_codes SET code_hash = NULL WHERE address = ? AND code_hash = ?").bind(address, hash).run();
  return used.meta.changes === 1;
}

/// Six digits, each of the million as likely.
export function newCode(): string {
  const buf = new Uint32Array(1);
  // 2^32 is not a multiple of a million: values from the last partial run
  // would make the low codes likelier, so they are drawn again.
  const limit = 2 ** 32 - (2 ** 32 % 1_000_000);
  do crypto.getRandomValues(buf);
  while (buf[0]! >= limit);
  return String(buf[0]! % 1_000_000).padStart(6, "0");
}

/// With the address in it, so one hash is no use for another address's row.
async function codeHash(address: string, code: string): Promise<string> {
  return hex(await sha256(`${address}\n${code}`));
}

export function codeMail(from: string, to: string, code: string) {
  const text = [
    `Your Arakun sign-in code is ${code}`,
    "",
    "Type it where you asked for it. It works once, for 10 minutes.",
    "",
    "If you did not ask for it, you can ignore this email. Nobody can sign in without the code.",
  ].join("\n");
  // The code is six digits, so nothing here needs escaping.
  const html = `<!doctype html>
<html><body style="margin:0;padding:32px 16px;background:#ffffff;color:#0a0a0a;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Helvetica,Arial,sans-serif">
<div style="max-width:420px;margin:0 auto">
<p style="font-size:15px;margin:0 0 16px">Your Arakun sign-in code is</p>
<p style="font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:32px;letter-spacing:6px;font-weight:600;margin:0 0 16px">${code}</p>
<p style="font-size:15px;margin:0 0 16px">Type it where you asked for it. It works once, for 10 minutes.</p>
<p style="font-size:13px;color:#737373;margin:0">If you did not ask for it, you can ignore this email. Nobody can sign in without the code.</p>
</div></body></html>`;
  return { from: { email: from, name: "Arakun" }, to, subject: `${code} is your Arakun sign-in code`, text, html };
}

async function readBody(request: Request): Promise<Record<string, unknown> | "too big" | null> {
  try {
    const text = await bodyText(request, MAX_BODY);
    if (text === null) return "too big";
    const body: unknown = JSON.parse(text);
    return typeof body === "object" && body !== null && !Array.isArray(body) ? (body as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}
