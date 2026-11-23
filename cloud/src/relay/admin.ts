// The operator's view, with no accounts to log into: a bearer secret set with
// `wrangler secret put ADMIN_SECRET`. Durable Objects cannot be listed, so each
// one leaves a card in KV under `labels/<label>` when its computer claims (at
// most once a day); the card's fields travel as KV metadata so one `list` call
// shows them all.
//
//   GET  /_admin/labels           → { labels: [{ label, claimed_at, last_seen, phones, today, blocked }] }
//   POST /_admin/block/<label>    → adds the label to `blocked` and drops its sockets
//   POST /_admin/unblock/<label>  → removes it

import type { Env } from "../util";
import { isLabel } from "./label";

/// What a label's object says about itself, once a day.
export interface LabelCard {
  claimed_at: number;
  last_seen: number;
  phones: number;
  today: number;
}

export async function admin(request: Request, env: Env, path: string): Promise<Response> {
  if (!(await authorized(request, env))) return new Response("unauthorized", { status: 401 });
  const [, , action, label] = path.split("/");
  if (request.method === "GET" && action === "labels" && !label) return labels(env);
  if (request.method === "POST" && (action === "block" || action === "unblock") && label) {
    if (!isLabel(label)) return new Response("not a label", { status: 400 });
    return setBlocked(env, label, action === "block");
  }
  return new Response("not found", { status: 404 });
}

/// `Authorization: Bearer <ADMIN_SECRET>`, compared without leaking its length
/// through timing. No secret configured means no admin at all.
async function authorized(request: Request, env: Env): Promise<boolean> {
  const secret = env.ADMIN_SECRET;
  const header = request.headers.get("Authorization") ?? "";
  const given = /^Bearer\s+/i.test(header) ? header.replace(/^Bearer\s+/i, "") : "";
  if (!secret || !given) return false;
  const a = new TextEncoder().encode(secret);
  const b = new TextEncoder().encode(given);
  if (a.length !== b.length) return false;
  return crypto.subtle.timingSafeEqual(a, b);
}

async function labels(env: Env): Promise<Response> {
  const blocked = await blockedList(env);
  const out: (LabelCard & { label: string; blocked: boolean })[] = [];
  let cursor: string | undefined;
  do {
    const page = await env.CONFIG.list<LabelCard>({ prefix: "labels/", cursor });
    for (const key of page.keys) {
      const label = key.name.slice("labels/".length);
      const card = key.metadata ?? { claimed_at: 0, last_seen: 0, phones: 0, today: 0 };
      out.push({ label, ...card, blocked: blocked.includes(label) });
    }
    cursor = page.list_complete ? undefined : page.cursor;
  } while (cursor);
  out.sort((x, y) => y.last_seen - x.last_seen);
  return Response.json({ labels: out });
}

async function setBlocked(env: Env, label: string, block: boolean): Promise<Response> {
  const list = (await blockedList(env)).filter((l) => l !== label);
  if (block) list.push(label);
  await env.CONFIG.put("blocked", JSON.stringify(list));
  if (block) {
    // Whoever is connected under the label goes now, not at the next connection.
    const headers = { "x-arakun-label": label };
    await env.RELAY.get(env.RELAY.idFromName(label)).fetch("https://relay/_block", { method: "POST", headers });
  }
  return Response.json({ label, blocked: block });
}

async function blockedList(env: Env): Promise<string[]> {
  const list = await env.CONFIG.get<unknown>("blocked", "json").catch(() => null);
  return Array.isArray(list) ? list.filter((l): l is string => typeof l === "string") : [];
}
