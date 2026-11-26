// The part of WebCrypto that e2e.js uses, in plain JavaScript (@noble). A page
// the computer serves over plain http on the network is not a secure context,
// and browsers leave `crypto.subtle` out there; this stands in for it. It is
// the same P-256, SHA-256, HKDF and AES-256-GCM, so the bytes on the wire are
// the same. Only `crypto.getRandomValues` is needed, which every page has.

import { gcm } from "@noble/ciphers/aes.js";
import { p256 } from "@noble/curves/nist.js";
import { hkdf } from "@noble/hashes/hkdf.js";
import { sha256 } from "@noble/hashes/sha2.js";

type Algo = string | { name: string; [k: string]: unknown };

/** A key as this stand-in keeps it: what it is for, and its bytes. */
interface Key {
  kind: "ecdsa" | "ecdh-public" | "ecdh-private" | "hkdf" | "aes";
  bytes: Uint8Array;
}

const nameOf = (a: Algo) => (typeof a === "string" ? a : a.name).toUpperCase();
const bytes = (b: BufferSource): Uint8Array =>
  b instanceof Uint8Array ? b : ArrayBuffer.isView(b) ? new Uint8Array(b.buffer, b.byteOffset, b.byteLength) : new Uint8Array(b);
// An ArrayBuffer of its own, as WebCrypto gives back.
const out = (b: Uint8Array): ArrayBuffer => b.slice().buffer as ArrayBuffer;

export const subtle = {
  async digest(algo: Algo, data: BufferSource): Promise<ArrayBuffer> {
    if (nameOf(algo) !== "SHA-256") throw new Error("only SHA-256");
    return out(sha256(bytes(data)));
  },

  async importKey(format: string, data: BufferSource, algo: Algo): Promise<Key> {
    if (format !== "raw") throw new Error("only raw keys");
    const raw = bytes(data).slice();
    switch (nameOf(algo)) {
      case "ECDSA":
      case "ECDH":
        // Refuses a point that is not on the curve, as WebCrypto does.
        p256.Point.fromBytes(raw);
        return { kind: nameOf(algo) === "ECDSA" ? "ecdsa" : "ecdh-public", bytes: raw };
      case "HKDF":
        return { kind: "hkdf", bytes: raw };
      case "AES-GCM":
        if (raw.length !== 32) throw new Error("only AES-256");
        return { kind: "aes", bytes: raw };
      default:
        throw new Error(`no ${nameOf(algo)} here`);
    }
  },

  async generateKey(algo: Algo): Promise<{ privateKey: Key; publicKey: Key }> {
    if (nameOf(algo) !== "ECDH") throw new Error("only ECDH keys");
    const secret = p256.utils.randomSecretKey();
    return {
      privateKey: { kind: "ecdh-private", bytes: secret },
      publicKey: { kind: "ecdh-public", bytes: p256.getPublicKey(secret, false) },
    };
  },

  async exportKey(format: string, key: Key): Promise<ArrayBuffer> {
    if (format !== "raw" || key.kind !== "ecdh-public") throw new Error("only raw public keys");
    return out(key.bytes);
  },

  async verify(algo: Algo, key: Key, sig: BufferSource, data: BufferSource): Promise<boolean> {
    if (nameOf(algo) !== "ECDSA" || key.kind !== "ecdsa") throw new Error("only ECDSA");
    try {
      // WebCrypto takes either half of s, so this does too.
      return p256.verify(bytes(sig), bytes(data), key.bytes, { lowS: false });
    } catch {
      return false;
    }
  },

  async deriveBits(algo: { name: string; [k: string]: unknown }, key: Key, length: number): Promise<ArrayBuffer> {
    switch (nameOf(algo)) {
      case "ECDH": {
        const peer = algo.public as Key;
        if (key.kind !== "ecdh-private" || peer?.kind !== "ecdh-public" || length !== 256) throw new Error("bad ECDH");
        // The x coordinate, as WebCrypto gives it: drop the compressed point's first byte.
        return out(p256.getSharedSecret(key.bytes, peer.bytes, true).subarray(1));
      }
      case "HKDF": {
        if (key.kind !== "hkdf" || nameOf(algo.hash as Algo) !== "SHA-256") throw new Error("bad HKDF");
        return out(hkdf(sha256, key.bytes, bytes(algo.salt as BufferSource), bytes(algo.info as BufferSource), length / 8));
      }
      default:
        throw new Error(`no ${algo.name} here`);
    }
  },

  async encrypt(algo: { name: string; iv: BufferSource; additionalData?: BufferSource }, key: Key, data: BufferSource): Promise<ArrayBuffer> {
    if (key.kind !== "aes") throw new Error("only AES-GCM");
    return out(gcm(key.bytes, bytes(algo.iv), algo.additionalData && bytes(algo.additionalData)).encrypt(bytes(data)));
  },

  async decrypt(algo: { name: string; iv: BufferSource; additionalData?: BufferSource }, key: Key, data: BufferSource): Promise<ArrayBuffer> {
    if (key.kind !== "aes") throw new Error("only AES-GCM");
    return out(gcm(key.bytes, bytes(algo.iv), algo.additionalData && bytes(algo.additionalData)).decrypt(bytes(data)));
  },
};

/** Gives e2e.js this stand-in, where the browser has no WebCrypto of its own. */
export function installSubtle(): void {
  const g = globalThis as { crypto?: Crypto; E2E_SUBTLE?: unknown };
  if (!g.crypto?.subtle) g.E2E_SUBTLE = subtle;
}
