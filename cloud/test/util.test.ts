// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
import { describe, expect, it } from "vitest";
import { b64url, fromB64url } from "../src/util";

describe("fromB64url", () => {
  it("reads strict base64url without padding", () => {
    expect(fromB64url("AA")).toEqual(new Uint8Array([0]));
    expect(fromB64url("_-8")).toEqual(new Uint8Array([0xff, 0xef]));
    expect(fromB64url("")).toEqual(new Uint8Array([]));
    const bytes = crypto.getRandomValues(new Uint8Array(65));
    expect(fromB64url(b64url(bytes))).toEqual(bytes);
  });

  it("refuses a second spelling of the same bytes", () => {
    // "AB" decodes to the same zero byte as "AA": only its unused bits differ.
    expect(atob("AB==")).toBe(atob("AA=="));
    expect(fromB64url("AB")).toBeNull();
    expect(fromB64url("AAB")).toBeNull();
    expect(fromB64url("AA==")).toBeNull();
    expect(fromB64url("+/8")).toBeNull();
    expect(fromB64url("A")).toBeNull();
  });
});
