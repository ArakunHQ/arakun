import { SELF, env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { labelFrom } from "../../src/relay/router";
import { base32, fromHex, isLabel, labelOf } from "../../src/relay/label";
import vector from "../../../phone/test/vectors/label.json";

const LABEL = "k7f3q2m4xwab4c5d";

describe("labels", () => {
  it("derive from the public key as the shared vector says", async () => {
    expect(await labelOf(fromHex(vector.public_key_hex))).toBe(vector.label);
    expect(isLabel(vector.label)).toBe(true);
  });

  it("have one shape: 16 base32 characters in lower case", () => {
    expect(isLabel(LABEL)).toBe(true);
    expect(isLabel("www")).toBe(false);
    expect(isLabel("K7F3Q2M4XWAB4C5D")).toBe(false);
    expect(isLabel("k7f3q2m4xwab4c5")).toBe(false);
    expect(isLabel("k7f3q2m4xwab4c5d1")).toBe(false);
    expect(isLabel("k7f3q9m2xwab4c50")).toBe(false); // 0, 1, 8 and 9 are not in the alphabet
  });

  it("base32 turns 10 bytes into 16 characters, RFC 4648 style", () => {
    expect(base32(new Uint8Array(10))).toBe("aaaaaaaaaaaaaaaa");
    expect(base32(new Uint8Array([0xff, 0xff, 0xff, 0xff, 0xff]))).toBe("77777777");
    expect(base32(new TextEncoder().encode("foobar"))).toBe("mzxw6ytboi");
  });

  it("come from the first level of the host, or from the override", () => {
    expect(labelFrom(`${LABEL}.arakun.dev`, {})).toBe(LABEL);
    expect(labelFrom(`${LABEL}.someone.workers.dev`, {})).toBe(LABEL);
    expect(labelFrom("www.arakun.dev", {})).toBeNull();
    expect(labelFrom("arakun.dev", {})).toBeNull();
    expect(labelFrom("localhost", { LABEL_OVERRIDE: LABEL })).toBe(LABEL);
    expect(labelFrom(`${LABEL}.arakun.dev`, { LABEL_OVERRIDE: "nope" })).toBeNull();
  });
});

describe("the Worker", () => {
  it("serves the phone page under a label", async () => {
    const res = await SELF.fetch(`https://${LABEL}.arakun.dev/`);
    expect(res.status).toBe(200);
    expect(res.headers.get("content-type")).toContain("text/html");
    expect(await res.text()).toContain("app.js");
    const js = await SELF.fetch(`https://${LABEL}.arakun.dev/app.js`);
    expect(js.status).toBe(200);
  });

  it("tells the page which label it is under", async () => {
    const res = await SELF.fetch(`https://${LABEL}.arakun.dev/_relay`);
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ label: LABEL });
  });

  it("knows nothing about hosts without a label", async () => {
    for (const host of ["www.arakun.dev", "arakun.dev", "docs.arakun.dev"]) {
      const res = await SELF.fetch(`https://${host}/`);
      expect(res.status, host).toBe(404);
    }
  });

  it("hands the sockets to the label's object, which wants a websocket", async () => {
    const res = await SELF.fetch(`https://${LABEL}.arakun.dev/pc`);
    expect(res.status).toBe(426);
    expect(env.RELAY).toBeDefined();
  });
});
