// Linear's tokens are stored sealed: AES-GCM under LINEAR_TOKEN_KEY, with the
// organization id as additional data, so a token copied to another
// workspace's row does not open.
//
// Format: v1.<base64url iv>.<base64url ciphertext>

import { b64url, fromB64url, utf8, type Env } from "./util";

const VERSION = "v1";

/// The key, or null when the secret is not 32 bytes of base64url.
async function key(env: Env): Promise<CryptoKey | null> {
  const raw = fromB64url(env.LINEAR_TOKEN_KEY ?? "");
  if (!raw || raw.length !== 32) return null;
  return crypto.subtle.importKey("raw", raw, "AES-GCM", false, ["encrypt", "decrypt"]);
}

export async function seal(env: Env, organizationId: string, plain: string): Promise<string> {
  const k = await key(env);
  if (!k) throw new Error("LINEAR_TOKEN_KEY is not 32 bytes of base64url");
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const body = await crypto.subtle.encrypt({ name: "AES-GCM", iv, additionalData: utf8(organizationId) }, k, utf8(plain));
  return `${VERSION}.${b64url(iv)}.${b64url(new Uint8Array(body))}`;
}

/// The token, or null for anything that is not ours, was changed, belongs to
/// another workspace, or was sealed under another key.
export async function open(env: Env, organizationId: string, sealed: string): Promise<string | null> {
  const parts = sealed.split(".");
  if (parts.length !== 3 || parts[0] !== VERSION) return null;
  const iv = fromB64url(parts[1]!);
  const body = fromB64url(parts[2]!);
  if (!iv || iv.length !== 12 || !body) return null;
  const k = await key(env);
  if (!k) return null;
  try {
    const plain = await crypto.subtle.decrypt({ name: "AES-GCM", iv, additionalData: utf8(organizationId) }, k, body);
    return new TextDecoder().decode(plain);
  } catch {
    return null;
  }
}
