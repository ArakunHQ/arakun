// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// What the panel's page may ask of this Worker: /api/<rest> goes to the cloud
// Worker's /admin/<rest>, as the signed-in operator. The browser never holds
// ADMIN_SECRET; it is added here, with the operator's name for the audit log.
// Nothing else of the browser's request goes on: no cookie, no client header.

import type { Operator } from "./session";
import { error } from "./util";

/// Admin bodies are small JSON objects.
const MAX_BODY = 4096;
/// A grant or a decline is recorded under the operator's login, whatever the
/// page sent.
const DECISION = /^cloud\/requests\/[^/]+\/(grant|decline)$/;

/// /api/<rest> → the cloud Worker's /admin/<rest>, as the operator.
export async function proxy(request: Request, env: Env, op: Operator): Promise<Response> {
  if (!env.ADMIN_SECRET) return error(503, "the admin secret is not set");
  const url = new URL(request.url);
  // As the browser sent it, not decoded and encoded again. Only a path under
  // /admin/ on the cloud Worker may be reached, so no dot segment, empty
  // segment or encoded separator is let through.
  const rest = url.pathname.slice("/api/".length);
  if (!rest || /\.\.|\/\/|\\|%2e|%2f|%5c/i.test(rest)) return error(404, "not found");
  // The operators' authenticator keys are this Worker's to read and write
  // (worker/keys.ts), never the page's: a signed-in page could replace
  // another operator's key, or anyone's, without a code.
  if (/^operators(\/|$)/i.test(rest)) return error(404, "not found");

  const change = request.method !== "GET" && request.method !== "HEAD";
  if (change && request.headers.get("Origin") !== url.origin) return error(403, "the request comes from another origin");

  const headers = new Headers({
    Authorization: `Bearer ${env.ADMIN_SECRET}`,
    "X-Arakun-Operator-Id": String(op.githubId),
    "X-Arakun-Operator-Login": op.login,
  });
  let body: Uint8Array<ArrayBuffer> | undefined;
  if (change) {
    const read = await readBody(request);
    if (read === "too large") return error(413, "the body is too large");
    if (read.length > 0) {
      body = read;
      headers.set("Content-Type", request.headers.get("Content-Type") ?? "application/json");
    }
    if (request.method === "PUT" && DECISION.test(rest)) {
      body = new TextEncoder().encode(JSON.stringify({ by: op.login }));
      headers.set("Content-Type", "application/json");
    }
  }

  let res: Response;
  try {
    res = await env.CLOUD.fetch(`${env.CLOUD_ORIGIN}/admin/${rest}${url.search}`, { method: request.method, headers, body, redirect: "manual" });
  } catch (e) {
    console.error(`${request.method} /api/${rest}:`, e instanceof Error ? e.message : "failed");
    return error(502, "the cloud did not answer");
  }
  const out = new Headers({ "Cache-Control": "no-store" });
  const type = res.headers.get("Content-Type");
  if (type) out.set("Content-Type", type);
  // The Response constructor throws on a body with these statuses, and a
  // binding may still hand back a stream. Close it so it is not left open.
  if (res.status === 204 || res.status === 205 || res.status === 304) {
    await res.body?.cancel().catch(() => {});
    return new Response(null, { status: res.status, headers: out });
  }
  return new Response(res.body, { status: res.status, headers: out });
}

/// The body, or "too large" past the limit. A client may lie about the length,
/// so the stream is counted as it is read.
async function readBody(request: Request): Promise<Uint8Array<ArrayBuffer> | "too large"> {
  const length = Number(request.headers.get("Content-Length") ?? 0);
  if (length > MAX_BODY) return "too large";
  if (!request.body) return new Uint8Array(0);
  const reader = request.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.length;
    if (total > MAX_BODY) {
      await reader.cancel().catch(() => {});
      return "too large";
    }
    chunks.push(value);
  }
  const all = new Uint8Array(total);
  let at = 0;
  for (const chunk of chunks) {
    all.set(chunk, at);
    at += chunk.length;
  }
  return all;
}

/// The policy of the panel's pages: its own script and style, GitHub's avatars
/// and nothing else, and no framing.
export function pagePolicy(): string {
  return [
    "default-src 'self'",
    "script-src 'self'",
    "connect-src 'self'",
    "img-src 'self' https://avatars.githubusercontent.com",
    "style-src 'self' 'unsafe-inline'",
    "font-src 'self'",
    "frame-ancestors 'none'",
    "base-uri 'none'",
    "form-action 'self'",
  ].join("; ");
}

/// A page from the assets, with the policy and the headers that go with it.
export async function page(request: Request, env: Env): Promise<Response> {
  const res = await env.ASSETS.fetch(request);
  const out = new Response(res.body, res);
  out.headers.set("Content-Security-Policy", pagePolicy());
  out.headers.set("X-Content-Type-Options", "nosniff");
  out.headers.set("Referrer-Policy", "no-referrer");
  return out;
}
