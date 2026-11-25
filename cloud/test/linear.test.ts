import { env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { linear } from "../src/linear";
import { open, seal } from "../src/linear_seal";
import { as, call, flagged, signIn } from "./helpers";

describe("the /linear gate", () => {
  it("is not a page: an unknown /linear path is a JSON 404", async () => {
    const u = await flagged(1, "ada");
    const res = await call("GET", "/linear/nope", as(u.session));
    expect(res.status).toBe(404);
    expect(await res.json()).toEqual({ error: "not found" });
  });
  it("needs a session and the linear flag", async () => {
    const res = await call("GET", "/linear/link");
    expect(res.status).toBe(401);
    expect(await res.json()).toEqual({ error: "not signed in" });
    const u = await signIn(1, "ada");
    expect((await call("GET", "/linear/link", as(u.session))).status).toBe(403);
  });
  it("answers a flagged user", async () => {
    const u = await flagged(1, "ada");
    const res = await call("GET", "/linear/link", as(u.session));
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ links: [] });
  });
  it("refuses a blocked user", async () => {
    const u = await flagged(1, "ada");
    await env.DB.prepare("UPDATE users SET blocked_at = 1 WHERE github_id = 1").run();
    expect((await call("GET", "/linear/link", as(u.session))).status).toBe(403);
  });
  it("is 404 without all four secrets", async () => {
    const u = await flagged(1, "ada");
    const bare = { ...env, LINEAR_TOKEN_KEY: undefined } as unknown as typeof env;
    const req = new Request("https://teams.test/linear/link", { headers: as(u.session) });
    const res = await linear(req, bare, new URL(req.url));
    expect(res?.status).toBe(404);
  });
  it("leaves other paths alone", async () => {
    const req = new Request("https://teams.test/lineary");
    expect(await linear(req, env, new URL(req.url))).toBeNull();
  });
});

describe("token sealing", () => {
  it("seals a token so only the same workspace opens it", async () => {
    const sealed = await seal(env, "org-1", "lin_oauth_abc");
    expect(sealed).toMatch(/^v1\.[\w-]+\.[\w-]+$/);
    expect(sealed).not.toContain("lin_oauth_abc");
    expect(await open(env, "org-1", sealed)).toBe("lin_oauth_abc");
    expect(await open(env, "org-2", sealed)).toBeNull();
    expect(await open(env, "org-1", sealed.slice(0, -2) + "AA")).toBeNull();
    expect(await seal(env, "org-1", "lin_oauth_abc")).not.toBe(sealed); // a fresh IV each time
  });
  it("opens nothing that is not sealed, and refuses a bad key", async () => {
    expect(await open(env, "org-1", "garbage")).toBeNull();
    expect(await open(env, "org-1", "v2.AAAA.AAAA")).toBeNull();
    const bad = { ...env, LINEAR_TOKEN_KEY: "short" } as typeof env;
    await expect(seal(bad, "org-1", "x")).rejects.toThrow();
    expect(await open(bad, "org-1", await seal(env, "org-1", "x"))).toBeNull();
  });
});
