// The second step of an operator's sign-in: a six-digit code from an
// authenticator app (TOTP, RFC 6238, with the defaults every app uses: SHA-1,
// 30-second steps, six digits). Each operator's key is in the Worker secret
// ADMIN_TOTP_SECRETS as `<github_id>:<base32 key>` entries, comma-separated.
// scripts/totp-secret.mjs makes a key.

const STEP_SECONDS = 30;
const DIGITS = 6;
/// One step either side, for a phone clock that is a little off.
const DRIFT_STEPS = 1;
/// RFC 4226 asks for at least 128 bits; the script makes 160.
const MIN_KEY_BYTES = 16;
const BASE32 = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// The bytes of a base32 text (RFC 4648, the form authenticator apps show),
/// or null if it is not one. Spaces and padding are ignored, case too.
export function fromBase32(text: string): Uint8Array<ArrayBuffer> | null {
  const clean = text.replace(/[\s=]/g, "").toUpperCase();
  if (!clean || !/^[A-Z2-7]+$/.test(clean)) return null;
  const out: number[] = [];
  let bits = 0;
  let value = 0;
  for (const c of clean) {
    value = (value << 5) | BASE32.indexOf(c);
    bits += 5;
    if (bits >= 8) {
      bits -= 8;
      out.push((value >> bits) & 255);
    }
  }
  return Uint8Array.from(out);
}

/// The operator's key, or null: not in the list, or the entry is not a key.
export function totpKey(secrets: string | undefined, githubId: number): Uint8Array<ArrayBuffer> | null {
  for (const part of (secrets ?? "").split(",")) {
    const colon = part.indexOf(":");
    if (colon < 0) continue;
    const id = part.slice(0, colon).trim();
    if (!/^\d+$/.test(id) || Number(id) !== githubId) continue;
    const key = fromBase32(part.slice(colon + 1));
    return key && key.length >= MIN_KEY_BYTES ? key : null;
  }
  return null;
}

/// The code for one time step (RFC 4226's HOTP with the step as the counter).
export async function codeAt(key: Uint8Array<ArrayBuffer>, step: number): Promise<string> {
  const counter = new Uint8Array(8);
  new DataView(counter.buffer).setBigUint64(0, BigInt(step));
  const hmac = await crypto.subtle.importKey("raw", key, { name: "HMAC", hash: "SHA-1" }, false, ["sign"]);
  const mac = new Uint8Array(await crypto.subtle.sign("HMAC", hmac, counter));
  const at = mac[mac.length - 1]! & 15;
  const n = ((mac[at]! & 127) << 24) | (mac[at + 1]! << 16) | (mac[at + 2]! << 8) | mac[at + 3]!;
  return String(n % 10 ** DIGITS).padStart(DIGITS, "0");
}

/// Whether the code is the key's at `now` (seconds), give or take a step.
export async function checkCode(key: Uint8Array<ArrayBuffer>, code: string, now: number): Promise<boolean> {
  if (!new RegExp(`^\\d{${DIGITS}}$`).test(code)) return false;
  const step = Math.floor(now / STEP_SECONDS);
  let ok = false;
  // Every step is tried, so the time taken does not say which one matched.
  for (let d = -DRIFT_STEPS; d <= DRIFT_STEPS; d++) {
    if (sameDigits(await codeAt(key, step + d), code)) ok = true;
  }
  return ok;
}

function sameDigits(a: string, b: string): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return diff === 0;
}
