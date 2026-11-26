// The second step of an operator's sign-in: a six-digit code from an
// authenticator app (TOTP, RFC 6238, with the defaults every app uses: SHA-1,
// 30-second steps, six digits). Operators set up their own key, at their
// first sign-in or in Settings (worker/setup.ts); the cloud Worker keeps it
// sealed (worker/keys.ts).

export const STEP_SECONDS = 30;
const DIGITS = 6;
/// One step either side, for a phone clock that is a little off.
const DRIFT_STEPS = 1;
/// 160 bits, as RFC 4226 recommends; 32 base32 characters exactly.
export const KEY_BYTES = 20;
const BASE32 = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
/// What the authenticator app lists the key under.
const ISSUER = "Arakun admin";

/// The bytes of a base32 text (RFC 4648, the form authenticator apps show),
/// or null if it is not one. Spaces and padding are ignored, case too.
export function fromBase32(text: string): Uint8Array<ArrayBuffer> | null {
  const clean = text.replace(/[\s=]/g, "").toUpperCase();
  if (!clean || !/^[A-Z2-7]+$/.test(clean)) return null;
  const out: number[] = [];
  let bits = 0;
  let value = 0;
  for (const c of clean) {
    value = ((value << 5) | BASE32.indexOf(c)) & 0xffff;
    bits += 5;
    if (bits >= 8) {
      bits -= 8;
      out.push((value >> bits) & 255);
    }
  }
  return Uint8Array.from(out);
}

export function toBase32(bytes: Uint8Array): string {
  let out = "";
  let bits = 0;
  let value = 0;
  for (const b of bytes) {
    value = ((value << 8) | b) & 0xffff;
    bits += 8;
    while (bits >= 5) {
      bits -= 5;
      out += BASE32[(value >> bits) & 31];
    }
  }
  if (bits > 0) out += BASE32[(value << (5 - bits)) & 31];
  return out;
}

export function newKey(): Uint8Array<ArrayBuffer> {
  return crypto.getRandomValues(new Uint8Array(KEY_BYTES));
}

/// The link an authenticator app reads, from a QR code or a tap on a phone.
export function otpauthUri(login: string, key: Uint8Array): string {
  const label = encodeURIComponent(`${ISSUER}:${login}`);
  return `otpauth://totp/${label}?secret=${toBase32(key)}&issuer=${encodeURIComponent(ISSUER)}`;
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

/// The step whose code this is, at `now` (seconds) give or take a step, or
/// null. The step is what keeps a code from being taken twice.
export async function checkCode(key: Uint8Array<ArrayBuffer>, code: string, now: number): Promise<number | null> {
  const digits = code.replace(/\s/g, "");
  if (!new RegExp(`^\\d{${DIGITS}}$`).test(digits)) return null;
  const step = Math.floor(now / STEP_SECONDS);
  let matched: number | null = null;
  // Every step is tried, so the time taken does not say which one matched.
  for (let d = -DRIFT_STEPS; d <= DRIFT_STEPS; d++) {
    if (sameDigits(await codeAt(key, step + d), digits)) matched = step + d;
  }
  return matched;
}

function sameDigits(a: string, b: string): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return diff === 0;
}
