// The primitives of e2e.js in pure JS (@noble, audited, MIT), for a page over
// plain http: the computer's own server on the local network. Browsers keep
// WebCrypto from such a page, so without these it could not seal anything.
// Loaded on demand, so a page with WebCrypto never downloads them.

import { gcm } from "@noble/ciphers/aes.js";
import { p256 } from "@noble/curves/nist.js";
import { hkdf } from "@noble/hashes/hkdf.js";
import { sha256 } from "@noble/hashes/sha2.js";

export const primitives = {
  async sha256(data: Uint8Array): Promise<Uint8Array> {
    return sha256(data);
  },
  /** The computer's signature over the handshake: r then s, over SHA-256 of `data`. */
  async verify(pub: Uint8Array, sig: Uint8Array, data: Uint8Array): Promise<boolean> {
    // ring makes high-S signatures half the time; WebCrypto takes them too.
    return p256.verify(sig, data, pub, { prehash: true, lowS: false });
  },
  async ecdh(): Promise<{ pub: Uint8Array; agree(peer: Uint8Array): Promise<Uint8Array> }> {
    const secret = p256.utils.randomSecretKey();
    return {
      pub: p256.getPublicKey(secret, false),
      // The x coordinate, as WebCrypto's deriveBits gives it. A peer key off
      // the curve throws.
      async agree(peer: Uint8Array) {
        return p256.getSharedSecret(secret, peer, true).slice(1);
      },
    };
  },
  async hkdf(ikm: Uint8Array, salt: Uint8Array, info: Uint8Array, len: number): Promise<Uint8Array> {
    return hkdf(sha256, ikm, salt, info, len);
  },
  async aesGcm(raw: Uint8Array) {
    return {
      async seal(iv: Uint8Array, aad: Uint8Array, plain: Uint8Array) {
        return gcm(raw, iv, aad).encrypt(plain);
      },
      async open(iv: Uint8Array, aad: Uint8Array, cipher: Uint8Array) {
        return gcm(raw, iv, aad).decrypt(cipher);
      },
    };
  },
};
