export type Env = Cloudflare.Env;

export const DAY = 24 * 60 * 60;

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

/// A user's flags, sorted so the answer is stable.
export function flagsQuery(db: D1Database, githubId: number): D1PreparedStatement {
  return db.prepare("SELECT flag FROM user_flags WHERE github_id = ? ORDER BY flag").bind(githubId);
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

/// A positive integer that fits in a JavaScript number without rounding.
export const GITHUB_ID = /^[1-9][0-9]{0,15}$/;

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
