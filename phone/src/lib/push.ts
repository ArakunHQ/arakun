// Telling this phone when the agent needs the person. Both need a secure
// origin: the Tailscale way in. Over plain http on the LAN the browser offers
// neither.

import { api } from "./api";

/**
 * Whether this phone can be told: "off" until the person decides, then "on" or
 * "denied"; "unsupported" where the browser cannot do it at all (plain http, no push).
 */
export function notifyState(): "on" | "off" | "denied" | "unsupported" {
  if (!window.isSecureContext || !("Notification" in window) || !("PushManager" in window)) return "unsupported";
  const p = Notification.permission;
  return p === "granted" ? "on" : p === "denied" ? "denied" : "off";
}

function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  return a.length === b.length && a.every((x, i) => x === b[i]);
}

/**
 * Subscribes this phone at its browser's push service with the computer's key
 * and hands the computer where to send. Needs the permission already given.
 * Throws when any step fails.
 */
export async function subscribePush(pushKey: string): Promise<void> {
  if (!pushKey) throw new Error("the computer sent no push key");
  const reg = await navigator.serviceWorker.ready;
  const key = Uint8Array.from(atob(pushKey.replace(/-/g, "+").replace(/_/g, "/")), (c) => c.charCodeAt(0));
  // The account host is one origin for every computer, so one subscription,
  // made with one computer's key. Another computer's key replaces it: the
  // browser refuses a second key, and notices follow the computer opened last.
  const old = await reg.pushManager.getSubscription();
  if (old && !sameBytes(new Uint8Array(old.options.applicationServerKey || []), key)) await old.unsubscribe();
  const sub = await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: key });
  const res = await api("/push", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ subscription: sub.toJSON() }),
  });
  if (!res.ok) throw new Error(`the computer answered ${res.status}`);
}
