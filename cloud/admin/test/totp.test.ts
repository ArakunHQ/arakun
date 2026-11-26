import { describe, expect, it } from "vitest";
import { checkCode, codeAt, fromBase32, totpKey } from "../worker/totp";

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

  it("takes the code of this step and the ones either side, and no other", async () => {
    const now = 1234567890;
    const step = Math.floor(now / 30);
    for (const d of [-1, 0, 1]) expect(await checkCode(key(), await codeAt(key(), step + d), now)).toBe(true);
    for (const d of [-3, -2, 2, 3]) expect(await checkCode(key(), await codeAt(key(), step + d), now)).toBe(false);
  });

  it("refuses what is not six digits", async () => {
    const now = 1234567890;
    for (const code of ["", "00592", "0059245", "abcdef", " 005924", "005924\n", "٠٠٥٩٢٤"]) {
      expect(await checkCode(key(), code, now), JSON.stringify(code)).toBe(false);
    }
  });

  it("finds an operator's key in the list, and none for anyone else", () => {
    expect(totpKey(`1001:${KEY}`, 1001)).toEqual(key());
    expect(totpKey(` 7:${KEY} , 1001 : ${KEY.toLowerCase()} `, 1001)).toEqual(key());
    expect(totpKey(`1001:${KEY}`, 1002)).toBeNull();
    expect(totpKey(`10011:${KEY}`, 1001)).toBeNull();
    expect(totpKey(undefined, 1001)).toBeNull();
    expect(totpKey("", 1001)).toBeNull();
    expect(totpKey(KEY, 1001)).toBeNull();
    // Too short to be a key, or not base32: the operator has none.
    expect(totpKey("1001:MZXW6", 1001)).toBeNull();
    expect(totpKey("1001:not-a-key", 1001)).toBeNull();
  });
});
