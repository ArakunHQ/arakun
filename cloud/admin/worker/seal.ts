// Sealing for what leaves this Worker and must come back unread and unchanged:
// an operator's authenticator key in the cloud Worker's database, and a new
// key in the setup cookie. AES-256-GCM with a key derived from SESSION_SECRET,
// so the database alone cannot make a code. The `context` is authenticated
// too, so a sealed value only opens where it was made for (one operator's
// row, the setup cookie).
//
// Changing SESSION_SECRET makes every stored key unreadable: the maintainer
// then forgets them (cloud/README.md), and each operator sets up again.

import { b64url, fromB64url } from "./util";

const IV_BYTES = 12;

async function aesKey(secret: string): Promise<CryptoKey> {
  const base = await crypto.subtle.importKey("raw", new TextEncoder().encode(secret), "HKDF", false, ["deriveKey"]);
  return crypto.subtle.deriveKey(
    { name: "HKDF", hash: "SHA-256", salt: new TextEncoder().encode("arakun-admin"), info: new TextEncoder().encode("seal") },
    base,
    { name: "AES-GCM", length: 256 },
    false,
    ["encrypt", "decrypt"],
  );
}

/// b64url(IV + ciphertext and tag).
export async function seal(plain: Uint8Array<ArrayBuffer>, context: string, secret: string): Promise<string> {
  const iv = crypto.getRandomValues(new Uint8Array(IV_BYTES));
  const sealed = await crypto.subtle.encrypt(
    { name: "AES-GCM", iv, additionalData: new TextEncoder().encode(context) },
    await aesKey(secret),
    plain,
  );
  const out = new Uint8Array(IV_BYTES + sealed.byteLength);
  out.set(iv);
  out.set(new Uint8Array(sealed), IV_BYTES);
  return b64url(out);
}

/// The plain bytes, or null: not ours, changed, another context or another secret.
export async function unseal(text: string, context: string, secret: string): Promise<Uint8Array<ArrayBuffer> | null> {
  const bytes = fromB64url(text);
  if (!bytes || bytes.length <= IV_BYTES) return null;
  try {
    const plain = await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: bytes.slice(0, IV_BYTES), additionalData: new TextEncoder().encode(context) },
      await aesKey(secret),
      bytes.slice(IV_BYTES),
    );
    return new Uint8Array(plain);
  } catch {
    return null;
  }
}
