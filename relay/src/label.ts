// A computer's label: the first level of its host, derived from its public key
// so nobody can claim a label that is not theirs. SHA-256 of the raw P-256 point
// (65 bytes), the first 80 bits, base32 in lower case: 16 characters.

const ALPHABET = "abcdefghijklmnopqrstuvwxyz234567";
export const LABEL_BYTES = 10;
export const LABEL_LENGTH = 16;
const LABEL_SHAPE = /^[a-z2-7]{16}$/;

export function isLabel(s: string): boolean {
  return LABEL_SHAPE.test(s);
}

export async function labelOf(publicKey: Uint8Array): Promise<string> {
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", publicKey as BufferSource));
  return base32(digest.subarray(0, LABEL_BYTES));
}

// RFC 4648 without padding; 10 bytes come out as 16 characters exactly.
export function base32(bytes: Uint8Array): string {
  let out = "";
  let bits = 0;
  let acc = 0;
  for (const b of bytes) {
    acc = (acc << 8) | b;
    bits += 8;
    while (bits >= 5) {
      out += ALPHABET[(acc >>> (bits - 5)) & 31];
      bits -= 5;
    }
  }
  if (bits > 0) out += ALPHABET[(acc << (5 - bits)) & 31];
  return out;
}

export function hex(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

export function fromHex(s: string): Uint8Array {
  const out = new Uint8Array(s.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(s.slice(i * 2, i * 2 + 2), 16);
  return out;
}
