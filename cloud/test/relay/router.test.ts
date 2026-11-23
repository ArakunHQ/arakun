import { SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { parsePcFrame, parsePhoneFrame } from "../../src/relay/frames";
import { labelFrom } from "../../src/relay/router";
import { Inbox, computer, stored } from "./helpers";
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

  it("come from the first level of a host under the base domain", () => {
    const env = { BASE_DOMAIN: "arakun.dev" };
    expect(labelFrom(`${LABEL}.arakun.dev`, env)).toBe(LABEL);
    expect(labelFrom(`${LABEL}.someone.workers.dev`, env)).toBeNull();
    expect(labelFrom(`${LABEL}.arakun.dev.evil.example`, env)).toBeNull();
    expect(labelFrom(`x.${LABEL}.arakun.dev`, env)).toBeNull();
    expect(labelFrom("www.arakun.dev", env)).toBeNull();
    expect(labelFrom("app.arakun.dev", env)).toBeNull();
    expect(labelFrom("arakun.dev", env)).toBeNull();
  });
});

describe("an old host under a label", () => {
  it("still serves the page, so an old page can say what to do", async () => {
    const res = await SELF.fetch(`https://${LABEL}.arakun.dev/`);
    expect(res.status).toBe(200);
    expect(res.headers.get("content-type")).toContain("text/html");
    expect(await res.text()).toContain("app.js");
    const js = await SELF.fetch(`https://${LABEL}.arakun.dev/app.js`);
    expect(js.status).toBe(200);
  });

  it("serves the page under the account host's policy, for its own host", async () => {
    for (const path of ["/", "/app.js"]) {
      const res = await SELF.fetch(`https://${LABEL}.arakun.dev${path}`);
      const csp = res.headers.get("Content-Security-Policy") ?? "";
      expect(csp, path).toContain("script-src 'self'");
      expect(csp, path).toContain(`connect-src 'self' wss://${LABEL}.arakun.dev`);
      expect(res.headers.get("X-Content-Type-Options"), path).toBe("nosniff");
      expect(res.headers.get("Referrer-Policy"), path).toBe("no-referrer");
    }
  });

  it("answers /_relay with 410 and update", async () => {
    const res = await SELF.fetch(`https://${LABEL}.arakun.dev/_relay`);
    expect(res.status).toBe(410);
    expect(await res.json()).toEqual({ update: true });
  });

  it("closes the computer's and the phone's sockets with update", async () => {
    const pc = await SELF.fetch(`https://${LABEL}.arakun.dev/pc`, { headers: { Upgrade: "websocket" } });
    const pcSocket = pc.webSocket!;
    const pcInbox = new Inbox(pcSocket, (b) => parsePcFrame(b) ?? { bad: true });
    pcSocket.accept();
    pcSocket.binaryType = "arraybuffer";
    expect(await pcInbox.control()).toEqual({ type: "bye", reason: "update" });
    expect((await pcInbox.closedWith()).code).toBe(4410);

    const ws = await SELF.fetch(`https://${LABEL}.arakun.dev/ws?d=dev-1&t=ticket`, { headers: { Upgrade: "websocket" } });
    const wsSocket = ws.webSocket!;
    const wsInbox = new Inbox(wsSocket, (b) => parsePhoneFrame(b) ?? { bad: true });
    wsSocket.accept();
    wsSocket.binaryType = "arraybuffer";
    expect(await wsInbox.control()).toEqual({ type: "bye", reason: "update" });
    expect((await wsInbox.closedWith()).code).toBe(4410);
  });

  it("does not reach the label's object", async () => {
    const me = await computer();
    const pc = await SELF.fetch(`https://${me.label}.arakun.dev/pc`, { headers: { Upgrade: "websocket" } });
    pc.webSocket!.accept();
    expect(await stored(me.label, "label")).toBeUndefined();
    expect((await SELF.fetch(`https://${LABEL}.arakun.dev/pc`)).status).toBe(426);
  });

  it("has no operator door and no account routes", async () => {
    expect((await SELF.fetch(`https://${LABEL}.arakun.dev/machines`)).status).toBe(404);
    expect((await SELF.fetch(`https://${LABEL}.arakun.dev/_admin/labels`, { headers: { Authorization: "Bearer test-secret" } })).status).toBe(404);
  });
});

describe("a host that is not a label", () => {
  it("is the account host, with nothing of the old relay", async () => {
    for (const host of ["www.arakun.dev", "arakun.dev", "app.arakun.dev"]) {
      expect((await SELF.fetch(`https://${host}/_relay`)).status, host).toBe(404);
      expect((await SELF.fetch(`https://${host}/pc`, { headers: { Upgrade: "websocket" } })).status, host).toBe(404);
    }
  });
});
