// The push service for the native apps. A browser brings its own push service;
// the iOS and Android apps can only be woken through Apple (APNs) and Google
// (FCM), and only with Arakun's credentials, which cannot sit on every
// computer. So an app's subscription points here, and the computer sends to it
// exactly what it sends a browser's push service (RFC 8030, a VAPID-signed
// aes128gcm body). We pass the ciphertext on unread; the app holds the key.
//
//   POST /push/v1/apns/<hex token>          → 201, Apple's production service
//   POST /push/v1/apns-sandbox/<hex token>  → 201, Apple's development service
//   POST /push/v1/fcm/<token>               → 201, Firebase Cloud Messaging
//
// 410 when Apple or Google say the token is gone, so the computer forgets the
// subscription as it does a browser's. 503 while the credentials are unset.

import { b64url, discard, error, fromB64url, utf8, type Env } from "./util";

/// The largest sealed message we take. APNs and FCM carry 4 KB each; the body
/// grows by a third in base64 and the wrapper needs room. The computer keeps
/// its notices well under this (core/src/phone/push.rs, `notice`).
export const MAX_BODY = 2800;
/// The smallest: the RFC 8188 header with a 65-byte key, a delimiter and the tag.
const MIN_BODY = 16 + 4 + 1 + 65 + 1 + 16;
/// What a phone keeps for one that is off, at most; the computer asks an hour.
const MAX_TTL = 24 * 60 * 60;
/// What a VAPID token may claim, as the services allow.
const MAX_TOKEN_LIFE = 24 * 60 * 60;

const ROUTE = /^\/push\/v1\/(apns|apns-sandbox|fcm)\/([^/]+)$/;
/// APNs device tokens are hex, 32 bytes today; Apple says not to rely on the length.
const APNS_TOKEN = /^[0-9a-fA-F]{32,400}$/;
/// FCM registration tokens: letters, digits, `-`, `_` and `:`.
const FCM_TOKEN = /^[A-Za-z0-9_:-]{20,4096}$/;

/// Shown if the app's notification service cannot open the message (an old
/// app, a lost key): it still says something, and nothing of the session.
export const FALLBACK = { title: "Arakun", body: "Open Arakun to see what changed." };

/// The answer for a /push route, or null when the path is not one.
export async function push(request: Request, env: Env, url: URL): Promise<Response | null> {
  const match = ROUTE.exec(url.pathname);
  if (!match) return null;
  if (request.method !== "POST") return error(405, "method not allowed");
  const service = match[1] as "apns" | "apns-sandbox" | "fcm";
  let token: string;
  try {
    token = decodeURIComponent(match[2]!);
  } catch {
    return error(404, "no such subscription");
  }
  if (!(service === "fcm" ? FCM_TOKEN : APNS_TOKEN).test(token)) return error(404, "no such subscription");
  if (request.headers.get("Content-Encoding")?.toLowerCase() !== "aes128gcm") return error(415, "the body must be aes128gcm");
  if (!(await vapidOk(request.headers.get("Authorization"), url.origin))) return error(401, "bad vapid authorization");
  // A key per token: one subscription cannot crowd out another.
  if (!(await env.PUSH_LIMIT.limit({ key: `${service}:${token}` })).success) return error(429, "too many messages");
  const body = await bytes(request, MAX_BODY);
  if (!body) return error(413, "the message is too big");
  if (body.length < MIN_BODY) return error(400, "the message is too short");
  const ttl = Math.min(MAX_TTL, Math.max(0, Math.floor(Number(request.headers.get("TTL") ?? "0")) || 0));
  const urgent = (request.headers.get("Urgency") ?? "normal").toLowerCase() === "high";
  const sealed = b64url(body);
  const sent = service === "fcm" ? await toFcm(env, token, sealed, ttl, urgent) : await toApns(env, service === "apns-sandbox", token, sealed, ttl, urgent);
  switch (sent) {
    case "sent":
      return new Response(null, { status: 201 });
    case "gone":
      return error(410, "the subscription is gone");
    case "busy":
      return error(429, "the push service is busy");
    case "unset":
      return error(503, "push to this app is not set up");
    case "failed":
      return error(502, "the push service refused the message");
  }
}

type Sent = "sent" | "gone" | "busy" | "unset" | "failed";

/// The request's body, or null if it is over `max`, read no further than that.
async function bytes(request: Request, max: number): Promise<Uint8Array | null> {
  if (Number(request.headers.get("Content-Length") ?? 0) > max) {
    await request.body?.cancel().catch(() => {});
    return null;
  }
  if (!request.body) return new Uint8Array();
  const reader = request.body.getReader();
  const out = new Uint8Array(max);
  let size = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    if (size + value.byteLength > max) {
      await reader.cancel().catch(() => {});
      return null;
    }
    out.set(value, size);
    size += value.byteLength;
  }
  return out.slice(0, size);
}

/// RFC 8292: `vapid t=<ES256 JWT>, k=<the key that signed it>`, made out to
/// this origin and not expired. Anyone can make a key, so this does not say
/// who sent it; it keeps out what no Web Push sender would send.
export async function vapidOk(header: string | null, origin: string): Promise<boolean> {
  const t = /(?:^|[\s,])t=([A-Za-z0-9_.-]+)/.exec(header ?? "")?.[1];
  const k = /(?:^|[\s,])k=([A-Za-z0-9_-]+)/.exec(header ?? "")?.[1];
  if (!header?.toLowerCase().startsWith("vapid ") || !t || !k) return false;
  const [head, claims, sig, extra] = t.split(".");
  if (!head || !claims || !sig || extra !== undefined) return false;
  const key = fromB64url(k);
  const signature = fromB64url(sig);
  if (key?.length !== 65 || signature?.length !== 64) return false;
  let parsed: { alg?: unknown; aud?: unknown; exp?: unknown };
  let algorithm: { alg?: unknown };
  try {
    algorithm = JSON.parse(new TextDecoder().decode(fromB64url(head) ?? new Uint8Array()));
    parsed = JSON.parse(new TextDecoder().decode(fromB64url(claims) ?? new Uint8Array()));
  } catch {
    return false;
  }
  if (algorithm?.alg !== "ES256" || parsed?.aud !== origin || typeof parsed.exp !== "number") return false;
  const now = Math.floor(Date.now() / 1000);
  if (parsed.exp <= now || parsed.exp > now + MAX_TOKEN_LIFE) return false;
  try {
    const verifier = await crypto.subtle.importKey("raw", key, { name: "ECDSA", namedCurve: "P-256" }, false, ["verify"]);
    return await crypto.subtle.verify({ name: "ECDSA", hash: "SHA-256" }, verifier, signature, utf8(`${head}.${claims}`));
  } catch {
    // Not a point on the curve.
    return false;
  }
}

// ---- Apple ----

/// Apple wants a new provider token at most every 20 minutes and at least
/// every 60; one isolate keeps one for 40.
const APNS_TOKEN_FOR_MS = 40 * 60 * 1000;
let apnsToken: { jwt: string; at: number; key: string } | null = null;

async function toApns(env: Env, sandbox: boolean, device: string, sealed: string, ttl: number, urgent: boolean): Promise<Sent> {
  if (!env.APNS_KEY || !env.APNS_KEY_ID || !env.APNS_TEAM_ID || !env.APNS_TOPIC) return "unset";
  const jwt = await apnsJwt(env.APNS_KEY, env.APNS_KEY_ID, env.APNS_TEAM_ID);
  if (!jwt) return "unset";
  const host = sandbox ? "api.sandbox.push.apple.com" : "api.push.apple.com";
  const payload = {
    // mutable-content wakes the app's notification service, which opens `e`
    // and puts the session's title and text in place of these.
    aps: { alert: FALLBACK, "mutable-content": 1, sound: "default" },
    e: sealed,
  };
  let res: Response;
  try {
    res = await fetch(`https://${host}/3/device/${device}`, {
      method: "POST",
      headers: {
        authorization: `bearer ${jwt}`,
        "apns-topic": env.APNS_TOPIC,
        "apns-push-type": "alert",
        "apns-priority": urgent ? "10" : "5",
        // 0 tells Apple to try once and keep nothing, as a TTL of 0 asks.
        "apns-expiration": ttl ? String(Math.floor(Date.now() / 1000) + ttl) : "0",
        "content-type": "application/json",
      },
      body: JSON.stringify(payload),
    });
  } catch (e) {
    console.error("push: APNs could not be reached:", e);
    return "failed";
  }
  if (res.ok) {
    await discard(res);
    return "sent";
  }
  const reason = await res
    .json<{ reason?: string }>()
    .then((j) => j.reason ?? "")
    .catch(() => "");
  // A token from the other service (a development build's, say) is as good
  // as gone here: the app subscribes again with the right path.
  if (res.status === 410 || reason === "BadDeviceToken" || reason === "DeviceTokenNotForTopic" || reason === "Unregistered") return "gone";
  if (res.status === 429) return "busy";
  // Our own key was refused: the next call makes a new token.
  if (res.status === 403) apnsToken = null;
  console.error(`push: APNs answered ${res.status} ${reason}`);
  return "failed";
}

async function apnsJwt(pem: string, keyId: string, teamId: string): Promise<string | null> {
  const now = Date.now();
  const cacheKey = `${teamId}/${keyId}`;
  if (apnsToken && apnsToken.key === cacheKey && now - apnsToken.at < APNS_TOKEN_FOR_MS) return apnsToken.jwt;
  const key = await importPkcs8(pem, { name: "ECDSA", namedCurve: "P-256" });
  if (!key) return null;
  const jwt = await signJwt({ alg: "ES256", kid: keyId }, { iss: teamId, iat: Math.floor(now / 1000) }, key, { name: "ECDSA", hash: "SHA-256" });
  apnsToken = { jwt, at: now, key: cacheKey };
  return jwt;
}

// ---- Google ----

/// A service account's access token, while it is good, per isolate.
let fcmToken: { token: string; until: number; account: string } | null = null;

interface ServiceAccount {
  project_id: string;
  client_email: string;
  private_key: string;
  token_uri?: string;
}

async function toFcm(env: Env, device: string, sealed: string, ttl: number, urgent: boolean): Promise<Sent> {
  const account = serviceAccount(env.FCM_SERVICE_ACCOUNT);
  if (!account) return "unset";
  const token = await fcmAccess(account);
  if (!token) return "failed";
  const message = {
    message: {
      token: device,
      // Data only: the app decides what to show once it has opened `e`.
      data: { e: sealed },
      android: { priority: urgent ? "HIGH" : "NORMAL", ttl: `${ttl}s` },
    },
  };
  let res: Response;
  try {
    res = await fetch(`https://fcm.googleapis.com/v1/projects/${encodeURIComponent(account.project_id)}/messages:send`, {
      method: "POST",
      headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
      body: JSON.stringify(message),
    });
  } catch (e) {
    console.error("push: FCM could not be reached:", e);
    return "failed";
  }
  if (res.ok) {
    await discard(res);
    return "sent";
  }
  const code = await res
    .json<{ error?: { status?: string; details?: { errorCode?: string }[] } }>()
    .then((j) => j.error?.details?.find((d) => d.errorCode)?.errorCode ?? j.error?.status ?? "")
    .catch(() => "");
  if (res.status === 404 || code === "UNREGISTERED" || code === "SENDER_ID_MISMATCH") return "gone";
  if (res.status === 429 || code === "QUOTA_EXCEEDED") return "busy";
  if (res.status === 401) fcmToken = null;
  console.error(`push: FCM answered ${res.status} ${code}`);
  return "failed";
}

function serviceAccount(json: string | undefined): ServiceAccount | null {
  if (!json) return null;
  try {
    const a = JSON.parse(json) as Partial<ServiceAccount>;
    if (typeof a.project_id !== "string" || typeof a.client_email !== "string" || typeof a.private_key !== "string") return null;
    return a as ServiceAccount;
  } catch {
    return null;
  }
}

/// An OAuth access token for FCM, from the service account's key (RFC 7523).
async function fcmAccess(account: ServiceAccount): Promise<string | null> {
  const now = Math.floor(Date.now() / 1000);
  if (fcmToken && fcmToken.account === account.client_email && fcmToken.until > now + 60) return fcmToken.token;
  const key = await importPkcs8(account.private_key, { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" });
  if (!key) return null;
  const tokenUri = account.token_uri ?? "https://oauth2.googleapis.com/token";
  const assertion = await signJwt(
    { alg: "RS256", typ: "JWT" },
    { iss: account.client_email, scope: "https://www.googleapis.com/auth/firebase.messaging", aud: tokenUri, iat: now, exp: now + 3600 },
    key,
    { name: "RSASSA-PKCS1-v1_5" },
  );
  try {
    const res = await fetch(tokenUri, {
      method: "POST",
      headers: { "content-type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({ grant_type: "urn:ietf:params:oauth:grant-type:jwt-bearer", assertion }).toString(),
    });
    if (!res.ok) {
      console.error(`push: Google refused the service account: ${res.status}`);
      await discard(res);
      return null;
    }
    const j = await res.json<{ access_token?: string; expires_in?: number }>();
    if (typeof j.access_token !== "string") return null;
    fcmToken = { token: j.access_token, until: now + (j.expires_in ?? 3600), account: account.client_email };
    return j.access_token;
  } catch (e) {
    console.error("push: Google could not be reached:", e);
    return null;
  }
}

// ---- Both ----

/// A PEM "PRIVATE KEY" (PKCS#8), as Apple's .p8 and Google's JSON hold it.
async function importPkcs8(pem: string, algorithm: SubtleCryptoImportKeyAlgorithm): Promise<CryptoKey | null> {
  const body = pem
    .replace(/\\n/g, "\n")
    .replace(/-----(BEGIN|END) PRIVATE KEY-----/g, "")
    .replace(/\s+/g, "");
  try {
    const der = Uint8Array.from(atob(body), (c) => c.charCodeAt(0));
    return await crypto.subtle.importKey("pkcs8", der, algorithm, false, ["sign"]);
  } catch (e) {
    console.error("push: the private key does not import:", e);
    return null;
  }
}

async function signJwt(header: object, claims: object, key: CryptoKey, algorithm: SubtleCryptoSignAlgorithm): Promise<string> {
  const signing = `${b64url(utf8(JSON.stringify(header)))}.${b64url(utf8(JSON.stringify(claims)))}`;
  const sig = new Uint8Array(await crypto.subtle.sign(algorithm, key, utf8(signing)));
  return `${signing}.${b64url(sig)}`;
}

/// Forgets the cached provider and access tokens, for tests that change the secrets.
export function forgetTokens(): void {
  apnsToken = null;
  fcmToken = null;
}
