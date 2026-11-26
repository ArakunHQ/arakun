// Pairing on the computer's own server: a six-digit code, from the QR link or
// typed, traded for a token that stays on this phone. The trade happens inside
// the sealed socket (sealed.ts), checked against the key the QR link names;
// a typed code trusts the key that answers, and keeps it from then on.

import { openSealed, type SealedOptions } from "./sealed";
import { keepPin, keepToken } from "./token";

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

/** How long the computer may take to answer, handshake and all. */
const PAIR_MS = 15_000;

/**
 * Trades `code` for a token and keeps it, with the label of the key that
 * signed the handshake. `label` is the one the QR link named, or "".
 */
export function pair(code: string, label = "", WebSocketImpl?: typeof WebSocket): Promise<Paired> {
  const unreachable = () => {
    const why = location.protocol === "https:" ? "Is Tailscale on, on the phone and on the computer?" : "Is the phone on the same network?";
    return { ok: false as const, error: `Could not reach Arakun. ${why}` };
  };
  return new Promise((resolve) => {
    let done = false;
    const finish = (p: Paired) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      link.close();
      resolve(p);
    };
    const timer = setTimeout(() => finish(unreachable()), PAIR_MS);
    const opts: SealedOptions = { label, greeting: { pair: { code, name: deviceName() } }, WebSocketImpl };
    const link = openSealed(opts, {
      onopen(answer, pinned) {
        if (answer.type !== "paired" || !answer.token) return finish({ ok: false, error: "Could not pair." });
        keepToken(answer.token);
        keepPin(pinned);
        finish({ ok: true, host: answer.host || "" });
      },
      onmessage() {},
      onclose({ kind, detail }) {
        if (kind === "refused" && detail === "code") return finish({ ok: false, error: "That code is wrong or has expired. Make a new one in Arakun." });
        if (kind === "untrusted") return finish({ ok: false, error: "The computer that answered is not the one that made this code. Make a new one in Arakun and scan it." });
        finish(unreachable());
      },
    });
  });
}
