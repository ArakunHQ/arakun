// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// Copies of the few helpers of ../../src/util.ts: this Worker imports nothing
// from the cloud Worker.

/// The Worker's own JSON answers: never cached, never sniffed as another type.
export function json(body: unknown, status = 200): Response {
  return Response.json(body, { status, headers: { "Cache-Control": "no-store", "X-Content-Type-Options": "nosniff" } });
}

export function error(status: number, message: string): Response {
  return json({ error: message }, status);
}

/// The bytes of a base64url text, or null if it is not one.
export function fromB64url(text: string): Uint8Array<ArrayBuffer> | null {
  if (!/^[A-Za-z0-9_-]*$/.test(text)) return null;
  try {
    const s = atob(text.replace(/-/g, "+").replace(/_/g, "/"));
    return Uint8Array.from(s, (c) => c.charCodeAt(0));
  } catch {
    return null;
  }
}
