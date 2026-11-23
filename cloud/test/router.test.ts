import { SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";

const LABEL = "k7f3q2m4xwab4c5d";

describe("the one Worker", () => {
  it("serves the relay under a label", async () => {
    const res = await SELF.fetch(`https://${LABEL}.arakun.dev/_relay`);
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ label: LABEL });
  });

  it("serves the teams API on a host that is not a label", async () => {
    const res = await SELF.fetch("https://app.arakun.dev/healthz");
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ ok: true });
  });

  it("keeps the relay's paths out of the teams host", async () => {
    const res = await SELF.fetch("https://app.arakun.dev/_relay");
    expect(res.status).toBe(404);
  });
});
