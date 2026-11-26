import { describe, expect, it } from "vitest";
import { checkCode, codeAt, fromBase32, otpauthUri, toBase32 } from "../worker/totp";

// RFC 6238's SHA-1 test key, "12345678901234567890", as base32.
const KEY = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

function key(): Uint8Array<ArrayBuffer> {
  return fromBase32(KEY)!;
}

describe("totp", () => {
  it("reads base32 the way authenticator apps show it", () => {
    expect(new TextDecoder().decode(key())).toBe("12345678901234567890");
    expect(fromBase32("gezd gnbv gy3t qojq gezd gnbv gy3t qojq")).toEqual(key());
    expect(fromBase32("MZXW6===")).toEqual(new TextEncoder().encode("foo"));
    expect(fromBase32("")).toBeNull();
    expect(fromBase32("ABC1")).toBeNull();
    expect(fromBase32("ABC!")).toBeNull();
  });

  it("makes RFC 6238's codes, cut to six digits", async () => {
    // Appendix B: 94287082, 07081804, 14050471, 89005924, 69279037.
    for (const [time, code] of [
      [59, "287082"],
      [1111111109, "081804"],
      [1111111111, "050471"],
      [1234567890, "005924"],
      [2000000000, "279037"],
    ] as const) {
      expect(await codeAt(key(), Math.floor(time / 30))).toBe(code);
    }
  });

  it("takes the code of this step and the ones either side, and says which step it was", async () => {
    const now = 1234567890;
    const step = Math.floor(now / 30);
    for (const d of [-1, 0, 1]) expect(await checkCode(key(), await codeAt(key(), step + d), now)).toBe(step + d);
    for (const d of [-3, -2, 2, 3]) expect(await checkCode(key(), await codeAt(key(), step + d), now)).toBeNull();
    expect(await checkCode(key(), "005 924", now)).toBe(step);
  });

  it("refuses what is not six digits", async () => {
    const now = 1234567890;
    for (const code of ["", "00592", "0059245", "abcdef", "005924x", "٠٠٥٩٢٤"]) {
      expect(await checkCode(key(), code, now), JSON.stringify(code)).toBeNull();
    }
  });

  it("writes base32 that reads back, and the link apps read", () => {
    expect(toBase32(key())).toBe(KEY);
    expect(toBase32(new TextEncoder().encode("foo"))).toBe("MZXW6");
    const bytes = crypto.getRandomValues(new Uint8Array(20));
    expect(fromBase32(toBase32(bytes))).toEqual(bytes);
    expect(otpauthUri("octo-cat", key())).toBe(`otpauth://totp/Arakun%20admin%3Aocto-cat?secret=${KEY}&issuer=Arakun%20admin`);
  });
});
