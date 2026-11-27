// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// Operators' authenticator keys, kept by the cloud Worker at
// /admin/operators/<github_id>/totp (../../src/admin_totp.ts), sealed here
// first. The browser never reaches those routes: worker/proxy.ts refuses them.

import { seal, unseal } from "./seal";
import type { Operator } from "./session";
import { KEY_BYTES } from "./totp";

export type StoredKey = { key: Uint8Array<ArrayBuffer>; lastStep: number };

/// The key, "none" when the operator has not set one up, "unreadable" when it
/// does not open (SESSION_SECRET changed), or "error" when the cloud did not
/// answer as it should.
export async function loadKey(env: Env, op: Operator): Promise<StoredKey | "none" | "unreadable" | "error"> {
  const res = await cloud(env, op, "GET", "");
  if (!res) return "error";
  if (res.status === 404) {
    await drop(res);
    return "none";
  }
  if (!res.ok) {
    await drop(res);
    return "error";
  }
  let body: unknown;
  try {
    body = await res.json();
  } catch {
    return "error";
  }
  const { sealed, last_step } = (typeof body === "object" && body !== null ? body : {}) as Record<string, unknown>;
  if (typeof sealed !== "string" || typeof last_step !== "number") return "error";
  const key = await unseal(sealed, context(op), env.SESSION_SECRET!);
  if (!key || key.length !== KEY_BYTES) return "unreadable";
  return { key, lastStep: last_step };
}

/// Keeps the operator's first key, with the step of the code that confirmed
/// it. "exists" when another one got there first.
export async function addKey(env: Env, op: Operator, key: Uint8Array<ArrayBuffer>, step: number): Promise<"ok" | "exists" | "error"> {
  const res = await cloud(env, op, "POST", "", { sealed: await seal(key, context(op), env.SESSION_SECRET!), step });
  if (!res) return "error";
  await drop(res);
  return res.status === 201 ? "ok" : res.status === 409 ? "exists" : "error";
}

export async function replaceKey(env: Env, op: Operator, key: Uint8Array<ArrayBuffer>, step: number): Promise<boolean> {
  const res = await cloud(env, op, "PUT", "", { sealed: await seal(key, context(op), env.SESSION_SECRET!), step });
  if (!res) return false;
  await drop(res);
  return res.status === 204;
}

/// Takes a code's step: true only the first time, and only for a step newer
/// than the last one taken.
export async function useStep(env: Env, op: Operator, step: number): Promise<boolean> {
  const res = await cloud(env, op, "POST", "/use", { step });
  if (!res) return false;
  await drop(res);
  return res.status === 204;
}

/// A key sealed for one operator does not open as another's.
function context(op: Operator): string {
  return `totp:${op.githubId}`;
}

async function cloud(env: Env, op: Operator, method: string, rest: string, body?: unknown): Promise<Response | null> {
  if (!env.ADMIN_SECRET) return null;
  const headers = new Headers({
    Authorization: `Bearer ${env.ADMIN_SECRET}`,
    "X-Arakun-Operator-Id": String(op.githubId),
    "X-Arakun-Operator-Login": op.login,
  });
  if (body !== undefined) headers.set("Content-Type", "application/json");
  try {
    return await env.CLOUD.fetch(`${env.CLOUD_ORIGIN}/admin/operators/${op.githubId}/totp${rest}`, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      redirect: "manual",
    });
  } catch (e) {
    console.error(`${method} operator key:`, e instanceof Error ? e.message : "failed");
    return null;
  }
}

async function drop(res: Response): Promise<void> {
  await res.body?.cancel().catch(() => {});
}
