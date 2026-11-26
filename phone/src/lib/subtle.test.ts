// The stand-in for WebCrypto (subtle.ts) under e2e.js, as on a page served
// over plain http: it must agree byte for byte with the vectors the Rust side
// wrote, and shake hands with a computer that uses real WebCrypto.
import { describe, expect, it, vi } from "vitest";
import e2eSource from "../../public/e2e.js?raw";
import vectorsText from "../../test/vectors/e2e.json?raw";
import { subtle } from "./subtle";

const vectors = JSON.parse(vectorsText);

/** e2e.js as on a page with no `crypto.subtle`: only the stand-in. */
function insecurePage(): any {
  const page: Record<string, unknown> = { E2E_SUBTLE: subtle };
  const noSubtle = { getRandomValues: <T extends ArrayBufferView>(a: T) => crypto.getRandomValues(a as any) as T };
  return new Function("globalThis", "crypto", `${e2eSource}\nreturn E2E;`)(page, noSubtle);
}

const E2E = insecurePage();
const bytes = (name: string): Uint8Array => E2E.fromB64url(vectors[name]);
const plain = (b: Uint8Array) => Array.from(b);

describe("the stand-in for WebCrypto", () => {
  it("is what e2e.js uses on a page without crypto.subtle", async () => {
    const digest = vi.spyOn(subtle, "digest");
    await E2E.labelOf(bytes("pc_pub"));
    expect(digest).toHaveBeenCalled();
    digest.mockRestore();
  });

  it("derives the label, trusts the computer's hello and the same keys", async () => {
    expect(await E2E.labelOf(bytes("pc_pub"))).toBe(vectors.label);
    const hello = JSON.parse(vectors.hello_pc);
    const pc = await E2E.verifyPc(hello, vectors.label, vectors.channel, bytes("e_phone"), bytes("nonce_phone"));
    expect(plain(pc.e)).toEqual(plain(bytes("e_pc")));
    await expect(E2E.verifyPc(hello, vectors.label, vectors.channel + 1, bytes("e_phone"), bytes("nonce_phone"))).rejects.toThrow("bad hello: sig");
    const keys = await E2E.deriveKeys(bytes("shared"), bytes("nonce_phone"), bytes("nonce_pc"));
    expect(plain(keys.phoneToPc)).toEqual(plain(bytes("key_phone_to_pc")));
    expect(plain(keys.pcToPhone)).toEqual(plain(bytes("key_pc_to_phone")));
  });

  it("seals the same bytes as the computer and opens what it sealed", async () => {
    const phone = await E2E.sessionFromKeys(bytes("key_phone_to_pc"), bytes("key_pc_to_phone"));
    const pc = await E2E.sessionFromKeys(bytes("key_pc_to_phone"), bytes("key_phone_to_pc"));
    for (const f of vectors.frames) {
      const sealed = E2E.fromB64url(f.sealed);
      const [from, to] = f.from === "pc" ? [pc, phone] : [phone, pc];
      expect(plain(await from.seal(f.plain))).toEqual(plain(sealed));
      expect(E2E.text(await to.open(sealed))).toBe(f.plain);
    }
    const forged = E2E.fromB64url(vectors.frames[1].sealed);
    forged[12] ^= 1;
    const fresh = await E2E.sessionFromKeys(bytes("key_phone_to_pc"), bytes("key_pc_to_phone"));
    await expect(fresh.open(forged)).rejects.toThrow("frame does not decrypt");
  });

  it("shakes hands with a computer that uses WebCrypto", async () => {
    const real = crypto.subtle;
    const identity = await real.generateKey({ name: "ECDSA", namedCurve: "P-256" }, false, ["sign"]);
    const pub = new Uint8Array(await real.exportKey("raw", identity.publicKey));
    const label = await E2E.labelOf(pub);
    const phone = await E2E.begin();
    const hello = JSON.parse(E2E.text(phone.hello));
    const ePhone = Uint8Array.from(E2E.fromB64url(hello.e));
    const noncePhone = Uint8Array.from(E2E.fromB64url(hello.nonce));

    const pcPair = await real.generateKey({ name: "ECDH", namedCurve: "P-256" }, false, ["deriveBits"]);
    const ePc = new Uint8Array(await real.exportKey("raw", pcPair.publicKey));
    const noncePc = crypto.getRandomValues(new Uint8Array(16));
    const signed = Uint8Array.from(E2E.transcript(label, 0, ePhone, ePc, noncePhone, noncePc));
    const sig = new Uint8Array(await real.sign({ name: "ECDSA", hash: "SHA-256" }, identity.privateKey, signed));
    const reply = E2E.utf8(JSON.stringify({ v: 1, pub: E2E.b64url(pub), e: E2E.b64url(ePc), nonce: E2E.b64url(noncePc), sig: E2E.b64url(sig), arakun: "t" }));
    const theirs = await real.importKey("raw", ePhone, { name: "ECDH", namedCurve: "P-256" }, false, []);
    const shared = new Uint8Array(await real.deriveBits({ name: "ECDH", public: theirs }, pcPair.privateKey, 256));
    const salt = new Uint8Array([...noncePhone, ...noncePc]);
    const ikm = await real.importKey("raw", shared, "HKDF", false, ["deriveBits"]);
    const bits = new Uint8Array(await real.deriveBits({ name: "HKDF", hash: "SHA-256", salt, info: new TextEncoder().encode("arakun-phone-v1 keys") }, ikm, 512));
    const pc = await E2E.sessionFromKeys(bits.slice(32), bits.slice(0, 32));

    expect(await E2E.helloLabel(reply)).toBe(label);
    const session = await phone.finish(reply, label, 0);
    expect(E2E.text(await session.open(await pc.seal("hello phone")))).toBe("hello phone");
    expect(E2E.text(await pc.open(await session.seal("hello computer")))).toBe("hello computer");
  });

  it("refuses a point that is not on the curve", async () => {
    const bad = new Uint8Array(65);
    bad[0] = 4;
    await expect(subtle.importKey("raw", bad, { name: "ECDH" })).rejects.toThrow();
  });
});
