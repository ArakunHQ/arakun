// Operators set up their own authenticator app, in two places:
//
// - At the first sign-in: GitHub said who this is and there is no key yet,
//   so the code step's cookie may set up the first one. It may never replace
//   one, or a taken-over GitHub account could swap the key for its own.
// - In Settings, signed in: a new key replaces the old one, and the request
//   needs a code from the old app too, so a stolen session cookie cannot.
//
// POST /auth/setup/start makes a key, shows it once and keeps it in a sealed
// cookie. POST /auth/setup with a code from the new app proves the app has it,
// and only then is the key stored. A key the app never got is never stored.

import { codeAllowed, smallJson, takeCode, text } from "./auth";
import { addKey, loadKey, replaceKey } from "./keys";
import { seal, unseal } from "./seal";
import { clearedCodeStepCookie, readCodeStep, readSession, sessionCookie, signSession, type Operator } from "./session";
import { checkCode, fromBase32, KEY_BYTES, newKey, otpauthUri, toBase32 } from "./totp";
import { cookie, error, json } from "./util";

const SETUP_COOKIE = "__Host-arakun_admin_setup";
/// Time to install an app and scan the code.
const SETUP_SECONDS = 10 * 60;
const SETUP_CONTEXT = "setup";

interface Who {
  op: Operator;
  /// Signed in (Settings), or only past GitHub (the first sign-in).
  signedIn: boolean;
}

async function who(request: Request, env: Env, now: number): Promise<Who | null> {
  const session = await readSession(request, env, now);
  if (session) return { op: session, signedIn: true };
  const step = await readCodeStep(request, env, now);
  return step ? { op: step, signedIn: false } : null;
}

function sameOrigin(request: Request): boolean {
  return request.headers.get("Origin") === new URL(request.url).origin;
}

/// POST /auth/setup/start → { key, uri }: the new key as base32, to type, and
/// as an otpauth:// link, for the QR code.
export async function setupStart(request: Request, env: Env): Promise<Response> {
  if (!sameOrigin(request)) return error(403, "the request comes from another origin");
  const now = Math.floor(Date.now() / 1000);
  const w = await who(request, env, now);
  if (!w || !env.SESSION_SECRET) return error(401, "sign-in expired");
  if (!w.signedIn) {
    const stored = await loadKey(env, w.op);
    if (stored === "error") return error(502, "the cloud did not answer");
    if (stored !== "none") return error(409, "a key exists");
  }
  const key = newKey();
  const claims = JSON.stringify({ github_id: w.op.githubId, exp: now + SETUP_SECONDS, key: toBase32(key) });
  const sealed = await seal(new TextEncoder().encode(claims), SETUP_CONTEXT, env.SESSION_SECRET);
  const res = json({ key: toBase32(key), uri: otpauthUri(w.op.login, key) });
  res.headers.append("Set-Cookie", `${SETUP_COOKIE}=${sealed}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=${SETUP_SECONDS}`);
  return res;
}

/// POST /auth/setup with { code } from the new app, and { current_code } from
/// the old one when replacing a key.
export async function setupFinish(request: Request, env: Env): Promise<Response> {
  if (!sameOrigin(request)) return error(403, "the request comes from another origin");
  const now = Math.floor(Date.now() / 1000);
  const w = await who(request, env, now);
  if (!w || !env.SESSION_SECRET) return error(401, "sign-in expired");
  const key = await readSetup(request, env.SESSION_SECRET, w.op, now);
  if (!key) return error(401, "setup expired");
  if (!(await codeAllowed(env, w.op))) return error(429, "too many codes; wait a minute");
  const body = await smallJson(request);
  const code = text(body?.["code"]);
  if (code === null) return error(400, "no code");
  const step = await checkCode(key, code, now);
  if (step === null) return error(401, "wrong code");

  if (!w.signedIn) {
    const added = await addKey(env, w.op, key, step);
    if (added === "exists") return error(409, "a key exists");
    if (added === "error") return error(502, "the cloud did not answer");
    const res = done();
    res.headers.append("Set-Cookie", sessionCookie(await signSession(w.op, env.SESSION_SECRET, now)));
    res.headers.append("Set-Cookie", clearedCodeStepCookie());
    return res;
  }

  const stored = await loadKey(env, w.op);
  if (stored === "error") return error(502, "the cloud did not answer");
  // A key that does not open cannot check a code; a session made with
  // today's SESSION_SECRET already passed one.
  if (typeof stored === "object") {
    const current = text(body?.["current_code"]);
    if (current === null) return error(400, "no current code");
    const taken = await takeCode(env, w.op, stored, current, now);
    if (taken === "wrong") return error(401, "wrong current code");
    if (taken === "used") return error(409, "code used");
  }
  if (!(await replaceKey(env, w.op, key, step))) return error(502, "the cloud did not answer");
  return done();
}

/// 204, and the setup cookie is spent.
function done(): Response {
  const res = new Response(null, { status: 204, headers: { "Cache-Control": "no-store" } });
  res.headers.append("Set-Cookie", clearedSetupCookie());
  return res;
}

export function clearedSetupCookie(): string {
  return `${SETUP_COOKIE}=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0`;
}

/// The new key, if the setup cookie is this operator's and not expired.
async function readSetup(request: Request, secret: string, op: Operator, now: number): Promise<Uint8Array<ArrayBuffer> | null> {
  const value = cookie(request, SETUP_COOKIE);
  if (!value) return null;
  const plain = await unseal(value, SETUP_CONTEXT, secret);
  if (!plain) return null;
  let claims: unknown;
  try {
    claims = JSON.parse(new TextDecoder().decode(plain));
  } catch {
    return null;
  }
  const { github_id, exp, key } = (typeof claims === "object" && claims !== null ? claims : {}) as Record<string, unknown>;
  if (github_id !== op.githubId || typeof exp !== "number" || now >= exp || typeof key !== "string") return null;
  const bytes = fromBase32(key);
  return bytes && bytes.length === KEY_BYTES ? bytes : null;
}
