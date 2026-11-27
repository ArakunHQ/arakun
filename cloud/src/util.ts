// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
export type Env = Cloudflare.Env;

export const DAY = 24 * 60 * 60;

/// The flag every team endpoint and channel socket needs. One name, so the
/// checks in the Worker, the SQL and the channel object cannot drift apart.
export const TEAMS_FLAG = "teams";

/// GitHub logins: letters, digits and hyphens, at most 39. Enterprise Managed
/// Users also have an underscore before their enterprise's short code. Sign-in
/// and invites share it, so anyone who can sign in can be invited.
export const GITHUB_LOGIN = /^[A-Za-z0-9_-]{1,39}$/;

/// Unix seconds, the unit every time in the database uses.
export function now(): number {
  return Math.floor(Date.now() / 1000);
}

export function error(status: number, message: string): Response {
  return Response.json({ error: message }, { status });
}

/// The token after `Bearer `, or null.
export function bearer(request: Request): string | null {
  const match = /^Bearer +(\S+)$/i.exec(request.headers.get("Authorization") ?? "");
  return match?.[1] ?? null;
}

export async function sha256(text: string): Promise<Uint8Array> {
  return new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)));
}

export function hex(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

export function b64url(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/// An account's flags, sorted so the answer is stable.
export function flagsQuery(db: D1Database, accountId: number): D1PreparedStatement {
  return db.prepare("SELECT flag FROM user_flags WHERE account_id = ? ORDER BY flag").bind(accountId);
}

export function flagList(result: D1Result<unknown>): string[] {
  return (result.results as { flag: string }[]).map((row) => row.flag);
}

/// The request's body as text, or null if it is over `maxBytes`. It reads the
/// stream and stops at the limit, so a large body is never held in memory,
/// with or without a Content-Length.
export async function bodyText(request: Request, maxBytes: number): Promise<string | null> {
  if (Number(request.headers.get("Content-Length") ?? 0) > maxBytes) {
    await request.body?.cancel().catch(() => {});
    return null;
  }
  if (!request.body) return "";
  const reader = request.body.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > maxBytes) {
      await reader.cancel().catch(() => {});
      return null;
    }
    chunks.push(value);
  }
  const bytes = new Uint8Array(size);
  let at = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, at);
    at += chunk.byteLength;
  }
  return new TextDecoder().decode(bytes);
}

/// Drops a response body we will not read. A failed cancel changes nothing
/// for the caller, so it must not turn the answer into an error.
export async function discard(res: Response): Promise<void> {
  await res.body?.cancel().catch(() => {});
}

/// Strict base64url without padding: anything that would not encode back to
/// the same text is refused, so one value has one spelling.
export function fromB64url(text: string): Uint8Array | null {
  if (!/^[A-Za-z0-9_-]*$/.test(text) || text.length % 4 === 1) return null;
  const bin = atob(text.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((text.length + 3) % 4));
  const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
  return b64url(bytes) === text ? bytes : null;
}

export function utf8(text: string): Uint8Array {
  return new TextEncoder().encode(text);
}

const CROCKFORD = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// Upper-case Crockford base32. The first character is at most 7: 48 bits of time.
export const ULID = /^[0-7][0-9A-HJKMNP-TV-Z]{25}$/;

/// A new ULID: 48 bits of milliseconds, then 80 random bits.
export function ulid(): string {
  let t = Date.now();
  let time = "";
  for (let i = 0; i < 10; i++) {
    time = CROCKFORD[t % 32] + time;
    t = Math.floor(t / 32);
  }
  // 256 is a multiple of 32, so the low five bits are uniform.
  return time + Array.from(crypto.getRandomValues(new Uint8Array(16)), (b) => CROCKFORD[b & 31]).join("");
}

/// The spelling of an account ID or a GitHub ID: a positive integer of at most 16 digits. Only
/// a format check: Number() rounds one above Number.MAX_SAFE_INTEGER. The
/// rounded value matches no account, since sign-in and the admin door take only
/// safe integers, so a lookup by it finds nothing; a caller that stores the
/// ID checks Number.isSafeInteger, as the admin door does.
export const NUMERIC_ID = /^[1-9][0-9]{0,15}$/;

/// The GitHub user ID of the account in the SQL expression `account`, as a
/// number, or null when it has no GitHub identity. For what the operator and
/// the webhooks show: an avatar, a profile link. Never to find an account.
export function GITHUB_ID_OF(account: string): string {
  return `(SELECT CAST(i.subject AS INTEGER) FROM identities i WHERE i.provider = 'github' AND i.account_id = ${account})`;
}

/// The account linked to the GitHub user whose numeric ID is the SQL
/// expression `githubId`, or null. Only for what names a GitHub user from
/// outside, such as an invite by login; everything else names the account.
/// Cast to an integer first: D1 binds a JS number as REAL, whose text is
/// "4242.0", not the "4242" the subject holds.
export function ACCOUNT_OF_GITHUB(githubId: string): string {
  return `(SELECT i.account_id FROM identities i WHERE i.provider = 'github' AND i.subject = CAST(CAST(${githubId} AS INTEGER) AS TEXT))`;
}

/// The request's JSON body, or null if it is missing, too big or not JSON.
export async function jsonBody(request: Request, maxBytes: number): Promise<unknown> {
  const text = await bodyText(request, maxBytes);
  if (text === null) return null;
  try {
    return JSON.parse(text);
  } catch {
    return null;
  }
}

export function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}
