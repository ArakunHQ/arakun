// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// Who the operator is: Cloudflare Access signs them in before a request
// reaches this Worker, and sends its token in Cf-Access-Jwt-Assertion. The
// token is checked here as well, not just trusted for being there: its
// signature against the team's keys, the panel's Access application (aud),
// the team (iss) and its times. Only a person's token, one with an email, is
// an operator; a service token is not.
// https://developers.cloudflare.com/cloudflare-one/identity/authorization-cookie/validating-json/

import { fromB64url } from "./util";

export interface Operator {
  email: string;
}

export const ACCESS_HEADER = "Cf-Access-Jwt-Assertion";

/// The same shape the cloud Worker takes in X-Arakun-Operator-Email
/// (src/admin_log.ts).
const EMAIL = /^[^\s@"<>]{1,64}@[A-Za-z0-9.-]{1,190}$/;
/// A team domain as Access names it: <team>.cloudflareaccess.com.
const TEAM_DOMAIN = /^[a-z0-9-]+\.cloudflareaccess\.com$/;
/// Clocks differ a little between Access and the Worker.
const SKEW = 60;
/// How long the team's keys are kept before they are fetched again. Access
/// rotates them every six weeks and publishes the next one ahead, so an hour
/// is plenty; a token with a key not seen yet fetches them at once.
const KEYS_SECONDS = 60 * 60;
/// At most one early fetch a minute, so tokens with made-up key IDs cannot
/// make the Worker fetch on every request.
const REFETCH_SECONDS = 60;
const MAX_TOKEN = 8192;

interface Keys {
  team: string;
  fetchedAt: number;
  byKid: Map<string, CryptoKey>;
}

let cached: Keys | null = null;

/// Forgets the team's keys, for tests.
export function forgetKeys(): void {
  cached = null;
}

/// The operator of a request, or null. Fails closed: with ACCESS_TEAM_DOMAIN
/// or ACCESS_AUD unset, nobody gets in, except a local dev run (see devOperator).
export async function readOperator(request: Request, env: Env, now: number): Promise<Operator | null> {
  const team = env.ACCESS_TEAM_DOMAIN;
  const aud = env.ACCESS_AUD;
  if (!team || !aud || team === "PLACEHOLDER" || aud === "PLACEHOLDER") return devOperator(request, env);
  if (!TEAM_DOMAIN.test(team)) return null;
  const token = request.headers.get(ACCESS_HEADER);
  if (!token || token.length > MAX_TOKEN) return null;
  return verify(token, team, aud, now);
}

/// For `npm run dev`, where no Access is in front: DEV_OPERATOR_EMAIL from
/// .dev.vars, only while Access is not configured and only on localhost. The
/// production Worker has ACCESS_AUD in wrangler.toml, so it never gets here.
function devOperator(request: Request, env: Env): Operator | null {
  const email = env.DEV_OPERATOR_EMAIL;
  const host = new URL(request.url).hostname;
  if (!email || !EMAIL.test(email) || (host !== "localhost" && host !== "127.0.0.1")) return null;
  return { email };
}

async function verify(token: string, team: string, aud: string, now: number): Promise<Operator | null> {
  const parts = token.split(".");
  if (parts.length !== 3) return null;
  const [headerText, payloadText, sigText] = parts as [string, string, string];
  const header = decode(headerText);
  const claims = decode(payloadText);
  const sig = fromB64url(sigText);
  if (!header || !claims || !sig) return null;
  if (header["alg"] !== "RS256" || typeof header["kid"] !== "string") return null;

  const key = await keyFor(team, header["kid"], now);
  if (!key) return null;
  const signed = new TextEncoder().encode(`${headerText}.${payloadText}`);
  if (!(await crypto.subtle.verify("RSASSA-PKCS1-v1_5", key, sig, signed))) return null;

  const auds = Array.isArray(claims["aud"]) ? claims["aud"] : [claims["aud"]];
  if (!auds.includes(aud)) return null;
  if (claims["iss"] !== `https://${team}`) return null;
  const exp = claims["exp"];
  if (typeof exp !== "number" || now >= exp + SKEW) return null;
  const nbf = claims["nbf"];
  if (nbf !== undefined && (typeof nbf !== "number" || now + SKEW < nbf)) return null;
  // A service token's has common_name and no email.
  const email = claims["email"];
  if (typeof email !== "string" || !EMAIL.test(email)) return null;
  return { email };
}

function decode(part: string): Record<string, unknown> | null {
  const bytes = fromB64url(part);
  if (!bytes) return null;
  try {
    const value: unknown = JSON.parse(new TextDecoder().decode(bytes));
    return typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}

/// The team's key with this ID, fetching the keys when they are old or the
/// ID is new to them.
async function keyFor(team: string, kid: string, now: number): Promise<CryptoKey | null> {
  const fresh = cached?.team === team && now - cached.fetchedAt < KEYS_SECONDS;
  const key = fresh ? cached!.byKid.get(kid) : undefined;
  if (key) return key;
  if (fresh && now - cached!.fetchedAt < REFETCH_SECONDS) return null;
  const keys = await fetchKeys(team, now);
  if (keys) cached = keys;
  return cached?.team === team ? (cached.byKid.get(kid) ?? null) : null;
}

async function fetchKeys(team: string, now: number): Promise<Keys | null> {
  try {
    const res = await fetch(`https://${team}/cdn-cgi/access/certs`);
    if (!res.ok) {
      console.error(`Access keys: ${res.status}`);
      return null;
    }
    const body = (await res.json()) as { keys?: unknown };
    const byKid = new Map<string, CryptoKey>();
    for (const jwk of Array.isArray(body.keys) ? body.keys : []) {
      if (typeof jwk !== "object" || jwk === null) continue;
      const { kid, kty, n, e } = jwk as Record<string, unknown>;
      if (typeof kid !== "string" || kty !== "RSA" || typeof n !== "string" || typeof e !== "string") continue;
      const key = await crypto.subtle.importKey("jwk", { kty, n, e, alg: "RS256", ext: true }, { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" }, false, [
        "verify",
      ]);
      byKid.set(kid, key);
    }
    return { team, fetchedAt: now, byKid };
  } catch (e) {
    console.error("Access keys:", e instanceof Error ? e.message : "failed");
    return null;
  }
}
