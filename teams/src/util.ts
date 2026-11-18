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
