import { env, SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { linear } from "../src/linear";
import { open, seal } from "../src/linear_seal";
import { as, call, flagged, linearFake, signIn } from "./helpers";

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

describe("installing Arakun in a workspace", () => {
  const cookieOf = (s: string) => ({ Cookie: `__Host-arakun_session=${s}` });
  const begin = async (u: { session: string }) => {
    const res = await SELF.fetch("https://teams.test/linear/install", { redirect: "manual", headers: cookieOf(u.session) });
    const set = res.headers.get("Set-Cookie") ?? "";
    const value = /^__Host-arakun_linear_state=([^;]*)/.exec(set)?.[1] ?? "";
    return { res, set, value, state: value.split(".")[0] ?? "" };
  };
  const callback = (u: { session: string }, query: string, state?: string) =>
    call("GET", `/linear/callback?${query}`, { ...cookieOf(u.session), ...(state === undefined ? {} : { Cookie: `__Host-arakun_session=${u.session}; __Host-arakun_linear_state=${state}` }) });
  const rows = () => env.DB.prepare("SELECT * FROM linear_workspaces").all<Record<string, unknown>>();

  it("sends the admin to Linear's consent page and keeps the state in a cookie", async () => {
    const u = await flagged(1, "ada");
    const { res, set, value, state } = await begin(u);
    expect(res.status).toBe(302);
    const to = new URL(res.headers.get("Location")!);
    expect(to.origin + to.pathname).toBe("https://linear.app/oauth/authorize");
    expect(to.searchParams.get("client_id")).toBe("lin-test");
    expect(to.searchParams.get("redirect_uri")).toBe("https://teams.test/linear/callback");
    expect(to.searchParams.get("response_type")).toBe("code");
    expect(to.searchParams.get("scope")).toBe("read,write,app:assignable,app:mentionable");
    expect(to.searchParams.get("actor")).toBe("app");
    expect(to.searchParams.get("state")).toBe(state);
    expect(state).toMatch(/^[\w-]{22}$/);
    expect(value).toBe(`${state}.1`);
    expect(set).toContain("HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=600");
  });
  it("needs the flag to start", async () => {
    const u = await signIn(1, "ada");
    expect((await call("GET", "/linear/install", cookieOf(u.session))).status).toBe(403);
  });
  it("stores one sealed workspace and says so", async () => {
    const u = await flagged(1, "ada");
    const { value, state } = await begin(u);
    const res = await callback(u, `code=abc&state=${state}`, value);
    expect(res.status).toBe(200);
    expect(res.headers.get("Content-Type")).toBe("text/plain; charset=utf-8");
    expect(await res.text()).toBe("Arakun is installed in Fake. You can close this page.");
    expect(res.headers.getSetCookie().some((c) => /^__Host-arakun_linear_state=;.*Max-Age=0/.test(c))).toBe(true);
    const all = (await rows()).results;
    expect(all).toHaveLength(1);
    expect(all[0]).toMatchObject({ organization_id: "org-fake", name: "Fake", app_user_id: "app-user", installed_by: 1 });
    expect(String(all[0]!.access_token)).toMatch(/^v1\./);
    expect(String(all[0]!.access_token)).not.toContain("lin_oauth");
  });
  it("refuses another state, no cookie, or another account, and stores nothing", async () => {
    const u = await flagged(1, "ada");
    const other = await flagged(2, "bob");
    const { value, state } = await begin(u);
    const bad = { error: "bad state" };
    const wrong = await callback(u, "code=abc&state=other", value);
    expect(wrong.status).toBe(400);
    expect(await wrong.json()).toEqual(bad);
    expect(wrong.headers.getSetCookie().some((c) => c.includes("Max-Age=0"))).toBe(true);
    const none = await callback(u, `code=abc&state=${state}`);
    expect(none.status).toBe(400);
    expect(await none.json()).toEqual(bad);
    const mine = await callback(other, `code=abc&state=${state}`, value);
    expect(mine.status).toBe(400);
    expect(await mine.json()).toEqual(bad);
    expect((await rows()).results).toHaveLength(0);
  });
  it("answers 400 when Linear sends an error", async () => {
    const u = await flagged(1, "ada");
    const { value, state } = await begin(u);
    const res = await callback(u, `error=access_denied&state=${state}`, value);
    expect(res.status).toBe(400);
    expect(await res.json()).toEqual({ error: "linear refused the install" });
    expect((await rows()).results).toHaveLength(0);
  });
  it("answers 502 when Linear refuses the code, and stores nothing", async () => {
    const u = await flagged(1, "ada");
    const { value, state } = await begin(u);
    linearFake.refuse = 500;
    const res = await callback(u, `code=abc&state=${state}`, value);
    expect(res.status).toBe(502);
    expect(await res.json()).toEqual({ error: "linear refused the install" });
    expect((await rows()).results).toHaveLength(0);
  });
  it("keeps one row when the same workspace is installed twice", async () => {
    const u = await flagged(1, "ada");
    for (let i = 0; i < 2; i++) {
      const { value, state } = await begin(u);
      expect((await callback(u, `code=abc&state=${state}`, value)).status).toBe(200);
    }
    const all = (await rows()).results;
    expect(all).toHaveLength(1);
    expect(await open(env, "org-fake", String(all[0]!.access_token))).toBe("lin_oauth_issued2");
  });
});
