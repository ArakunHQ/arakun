// Pairing on the computer's own server: a six-digit code, from the QR link or
// typed, traded for a token that stays on this phone.

import { keepToken } from "./token";

/** What the computer will call this phone. */
export function deviceName(ua: string = navigator.userAgent, touches: number = navigator.maxTouchPoints): string {
  if (/iPhone/.test(ua)) return "iPhone";
  // iPad Safari calls itself a Mac; the touchscreen gives it away.
  if (/iPad/.test(ua) || (/Macintosh/.test(ua) && touches > 1)) return "iPad";
  if (/Android/.test(ua)) return /Mobile/.test(ua) ? "Android phone" : "Android tablet";
  if (/Windows/.test(ua)) return "Browser on Windows";
  if (/Macintosh/.test(ua)) return "Browser on a Mac";
  if (/Linux/.test(ua)) return "Browser on Linux";
  return "Browser";
}

export type Paired = { ok: true; host: string } | { ok: false; error: string };

/** Trades `code` for a token and keeps it. */
export async function pair(code: string): Promise<Paired> {
  try {
    const res = await fetch("/pair", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ code, name: deviceName() }),
    });
    if (!res.ok) {
      return { ok: false, error: res.status === 403 ? "That code is wrong or has expired. Make a new one in Arakun." : `Could not pair (${res.status}).` };
    }
    const data = (await res.json()) as { token: string; host?: string };
    keepToken(data.token);
    return { ok: true, host: data.host || "" };
  } catch {
    const why = location.protocol === "https:" ? "Is Tailscale on, on the phone and on the computer?" : "Is the phone on the same network?";
    return { ok: false, error: `Could not reach Arakun. ${why}` };
  }
}
