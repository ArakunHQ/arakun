// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// The one way the panel talks to its Worker. The Worker answers /api/me itself
// and forwards the rest of /api/* to the cloud Worker, so every error comes as
// JSON { error } from one of the two, or from Cloudflare in front of them.

export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

const NOT_SIGNED_IN = "not signed in";

let onSignedOut: () => void = () => {};

/// Called on the admin Worker's own 401: the session cookie expired or was
/// signed out elsewhere. The panel uses it to leave for the sign-in screen.
export function setOnSignedOut(handler: () => void): void {
  onSignedOut = handler;
}

/// Calls /api/<path>. Throws ApiError with the server's `error` text; for the admin Worker's own 401 it also calls onSignedOut.
export async function api<T>(method: "GET" | "PUT" | "POST" | "DELETE", path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = { Accept: "application/json" };
  if (body !== undefined) headers["Content-Type"] = "application/json";
  const res = await fetch(`/api/${path}`, {
    method,
    // The session is a same-origin cookie; nothing else carries it.
    credentials: "same-origin",
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!res.ok) {
    const error = new ApiError(res.status, await errorText(res));
    // The cloud Worker's own 401 ({ error: "unauthorized" }, a wrong or unset
    // ADMIN_SECRET) passes through too, and the operator is still signed in.
    if (res.status === 401 && error.message === NOT_SIGNED_IN) onSignedOut();
    throw error;
  }
  if (res.status === 204) return undefined as T;
  try {
    return (await res.json()) as T;
  } catch {
    throw new ApiError(res.status, "the server sent an answer the panel could not read");
  }
}

/// The server's `error` text, or the status when the body is not ours (a
/// proxy's HTML page, say).
async function errorText(res: Response): Promise<string> {
  try {
    const body: unknown = await res.json();
    if (typeof body === "object" && body !== null && typeof (body as { error?: unknown }).error === "string") {
      return (body as { error: string }).error;
    }
  } catch {
    // Not JSON: fall through to the status.
  }
  return `the server answered ${res.status}`;
}
