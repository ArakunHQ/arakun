import { createExecutionContext, env, SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import worker from "../src/index";

const LABEL = "k7f3q2m4xwab4c5d";

describe("the one Worker", () => {
  it("tells an old app or page under a label to update", async () => {
    const res = await SELF.fetch(`https://${LABEL}.arakun.dev/_relay`);
    expect(res.status).toBe(410);
    expect(await res.json()).toEqual({ update: true });
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

  it("serves the phone page on the account host", async () => {
    const res = await SELF.fetch("https://app.arakun.dev/");
    expect(res.status).toBe(200);
    expect(res.headers.get("Content-Type")).toMatch(/^text\/html/);
    expect(await res.text()).toContain('<script src="relay.js"></script>');
  });

  it("serves the page with a policy that keeps it to its own host", async () => {
    const res = await SELF.fetch("https://app.arakun.dev/");
    const csp = res.headers.get("Content-Security-Policy") ?? "";
    for (const part of [
      "default-src 'self'",
      "script-src 'self'",
      "connect-src 'self' wss://app.arakun.dev",
      "img-src 'self' data: blob: https:",
      "style-src 'self' 'unsafe-inline'",
      "frame-ancestors 'none'",
      "base-uri 'none'",
      "form-action 'self' https://github.com",
    ]) {
      expect(csp, part).toContain(part);
    }
    expect(csp).not.toContain("unsafe-eval");
    expect(res.headers.get("X-Content-Type-Options")).toBe("nosniff");
    expect(res.headers.get("Referrer-Policy")).toBe("no-referrer");
    // Its script and service worker too.
    for (const path of ["/assets/app.js", "/sw.js"]) {
      const file = await SELF.fetch(`https://app.arakun.dev${path}`);
      expect(file.headers.get("Content-Security-Policy"), path).toBe(csp);
    }
  });

  it("serves the page's script on the account host", async () => {
    const res = await SELF.fetch("https://app.arakun.dev/assets/app.js");
    expect(res.status).toBe(200);
    expect(res.headers.get("Content-Type")).toMatch(/javascript/);
    expect(res.headers.get("Cache-Control")).not.toBe("no-store");
    // The bundle reads the globals relay.js sets.
    expect(await res.text()).toContain("RELAY");
  });

  it("keeps unknown API paths JSON", async () => {
    for (const path of ["/machines/nope/x", "/auth/nope", "/me/x", "/sessions/x", "/notices/x", "/admin/nope", "/teams/x/y/z", "/_relay"]) {
      const res = await SELF.fetch(`https://app.arakun.dev${path}`);
      // The operator's routes ask for the admin secret before anything else.
      expect(res.status, path).toBe(path.startsWith("/admin/") ? 401 : 404);
      expect(res.headers.get("Content-Type"), path).toMatch(/^application\/json/);
      expect(res.headers.get("Cache-Control"), path).toBe("no-store");
    }
  });

  it("answers a page that is not there with the assets' 404, not the API's", async () => {
    const res = await SELF.fetch("https://app.arakun.dev/nope.txt");
    expect(res.status).toBe(404);
    expect(res.headers.get("Content-Type") ?? "").not.toMatch(/^application\/json/);
  });

  it("does not serve the page for a request that changes something", async () => {
    const res = await SELF.fetch("https://app.arakun.dev/", { method: "POST" });
    expect(res.status).toBe(404);
    expect(res.headers.get("Content-Type")).toMatch(/^application\/json/);
  });
});

describe("the account host", () => {
  const withHost = (url: string) => worker.fetch(new Request(url), { ...env, ACCOUNT_HOST: "app.arakun.dev" }, createExecutionContext());

  it("is any host that is not a label while ACCOUNT_HOST is unset", async () => {
    for (const host of ["app.arakun.dev", "other.arakun.dev", "arakun-cloud.someone.workers.dev"]) {
      const res = await SELF.fetch(`https://${host}/healthz`);
      expect(res.status, host).toBe(200);
    }
  });

  it("is only ACCOUNT_HOST once it is set, and an old label host still says update", async () => {
    expect((await withHost("https://app.arakun.dev/healthz")).status).toBe(200);
    expect((await withHost("https://app.arakun.dev/")).status).toBe(200);
    for (const path of ["/healthz", "/", "/me", "/auth/github/start", "/machines"]) {
      const res = await withHost(`https://other.arakun.dev${path}`);
      expect(res.status, path).toBe(404);
      expect(res.headers.get("Content-Type"), path).toMatch(/^application\/json/);
    }
    expect((await withHost(`https://${LABEL}.arakun.dev/_relay`)).status).toBe(410);
  });
});
