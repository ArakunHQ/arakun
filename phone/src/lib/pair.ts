// Pairing on the computer's own server: a six-digit code, from the QR link or
// typed, traded for a token that stays on this phone. The trade goes inside the
// sealed socket (sealed.ts); the QR link also names the computer's key, which
// is then the only one trusted.

import { openSealed, type SealedOptions } from "./sealed";
import { keepLabel, keepToken } from "./token";

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

/** Trades `code` for a token and keeps it, with the label of the key that answered. `label`, from the QR link, is the only key trusted. */
export function pair(code: string, label = "", WebSocketImpl?: SealedOptions["WebSocketImpl"]): Promise<Paired> {
  return new Promise((resolve) => {
    const link = openSealed(
      { who: { code, name: deviceName() }, label, WebSocketImpl },
      {
        onopen: ({ label: shown, paired }) => {
          link.close();
          if (!paired?.token) return resolve({ ok: false, error: "Could not pair." });
          keepToken(paired.token);
          keepLabel(shown);
          resolve({ ok: true, host: paired.host });
        },
        onmessage: () => {},
        onclose: ({ kind, detail }) => {
          if (kind === "refused" && detail === "bad_code") {
            return resolve({ ok: false, error: "That code is wrong or has expired. Make a new one in Arakun." });
          }
          if (kind === "untrusted") {
            return resolve({ ok: false, error: "The computer at this address is not the one that made the code. Pair from Arakun's own QR code." });
          }
          const why = location.protocol === "https:" ? "Is Tailscale on, on the phone and on the computer?" : "Is the phone on the same network?";
          resolve({ ok: false, error: `Could not reach Arakun. ${why}` });
        },
      },
    );
  });
}
