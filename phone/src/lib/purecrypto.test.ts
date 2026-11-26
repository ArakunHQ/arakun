// The pure-JS primitives must give what WebCrypto gives: the Rust side's
// vectors, run through e2e.js with them in place.
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import e2eScript from "../../public/e2e.js?raw";
import vectors from "../../test/vectors/e2e.json";
import { primitives } from "./purecrypto";

let E2E: any;

beforeAll(() => {
  // A plain script that sets a global, as the page loads it.
  new Function(e2eScript)();
  E2E = (globalThis as any).E2E;
  E2E.usePrimitives(primitives);
});

afterAll(() => E2E.usePrimitives(null));

const bytes = (name: string) => E2E.fromB64url((vectors as Record<string, any>)[name]);

describe("pure-JS primitives", () => {
  it("trust the computer's hello and derive the same keys", async () => {
    expect(await E2E.labelOf(bytes("pc_pub"))).toBe(vectors.label);
    const pc = await E2E.verifyPc(JSON.parse(vectors.hello_pc), vectors.label, vectors.channel, bytes("e_phone"), bytes("nonce_phone"));
    expect(pc.e).toEqual(bytes("e_pc"));
    await expect(
      E2E.verifyPc(JSON.parse(vectors.hello_pc), vectors.label, vectors.channel + 1, bytes("e_phone"), bytes("nonce_phone")),
    ).rejects.toThrow("bad hello: sig");
    const keys = await E2E.deriveKeys(bytes("shared"), bytes("nonce_phone"), bytes("nonce_pc"));
    expect(keys.phoneToPc).toEqual(bytes("key_phone_to_pc"));
    expect(keys.pcToPhone).toEqual(bytes("key_pc_to_phone"));
  });

  it("seal to the same bytes and open the computer's frames", async () => {
    const phone = await E2E.sessionFromKeys(bytes("key_phone_to_pc"), bytes("key_pc_to_phone"));
    const pc = await E2E.sessionFromKeys(bytes("key_pc_to_phone"), bytes("key_phone_to_pc"));
    for (const f of vectors.frames) {
      const sealed = E2E.fromB64url(f.sealed);
      const [from, to] = f.from === "pc" ? [pc, phone] : [phone, pc];
      expect(await from.seal(f.plain)).toEqual(sealed);
      expect(E2E.text(await to.open(sealed))).toBe(f.plain);
    }
    const forged = Uint8Array.from(await phone.seal("x"));
    forged[10] ^= 1;
    await expect(pc.open(forged)).rejects.toThrow("frame does not decrypt");
  });

  it("agree on a secret with a WebCrypto key, and refuse a point off the curve", async () => {
    const mine = await primitives.ecdh();
    const theirs = await crypto.subtle.generateKey({ name: "ECDH", namedCurve: "P-256" }, false, ["deriveBits"]);
    const theirPub = new Uint8Array(await crypto.subtle.exportKey("raw", theirs.publicKey));
    const myKey = await crypto.subtle.importKey("raw", mine.pub as BufferSource, { name: "ECDH", namedCurve: "P-256" }, false, []);
    const expected = new Uint8Array(await crypto.subtle.deriveBits({ name: "ECDH", public: myKey }, theirs.privateKey, 256));
    expect(await mine.agree(theirPub)).toEqual(expected);
    const off = new Uint8Array(65);
    off[0] = 4;
    off[1] = 1;
    await expect(mine.agree(off)).rejects.toThrow();
  });
});
