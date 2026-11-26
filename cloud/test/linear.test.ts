import { env, SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { linear, routeFor } from "../src/linear";
import { open, seal } from "../src/linear_seal";
import { ADMIN, as, call, dump, flagged, linearFake, signIn } from "./helpers";
import { account, connectedComputer } from "./relay/helpers";

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

describe("linking a Linear user and routing", () => {
  const ORG = "11111111-1111-4111-8111-111111111111";
  const LU = "22222222-2222-4222-8222-222222222222";
  const TEAM = "33333333-3333-4333-8333-333333333333";
  const TEAM2 = "33333333-3333-4333-8333-444444444444";
  const PROJECT = "44444444-4444-4444-8444-444444444444";
  const json = (v: unknown) => JSON.stringify(v);
  const link = (session: string, token: string) => call("POST", "/linear/link", as(session), json({ token }));
  const put = (session: string, body: unknown) => call("PUT", "/linear/routes", as(session), json(body));
  const routes = async () =>
    (await env.DB.prepare("SELECT label, github_id, organization_id, kind, target_id FROM linear_routes ORDER BY label, kind, target_id").all()).results;
  const links = async () => (await env.DB.prepare("SELECT organization_id, linear_user_id, github_id FROM linear_links").all()).results;
  const proves = (token: string, userId = LU) => linearFake.tokens.set(token, { userId, organizationId: ORG, organizationName: "Acme" });
  /// An account with the flag, a computer, and a link to ORG.
  const setup = async () => {
    const owner = await account();
    await call("PUT", `/admin/users/${owner.github_id}/flags/linear`, ADMIN);
    const { me } = await connectedComputer(owner);
    proves(`lin_api_${owner.github_id}`, `22222222-2222-4222-8222-${String(owner.github_id).padStart(12, "0")}`);
    expect((await link(owner.session, `lin_api_${owner.github_id}`)).status).toBe(200);
    return { owner, label: me.label };
  };

  it("links the Linear user the token belongs to and forgets the token", async () => {
    const u = await flagged(1, "ada");
    proves("lin_api_ada");
    const res = await link(u.session, "lin_api_ada");
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ organization_id: ORG, linear_user_id: LU, installed: false });
    expect(await links()).toEqual([{ organization_id: ORG, linear_user_id: LU, github_id: 1 }]);
    expect(await dump(env.DB)).not.toContain("lin_api_ada");
    expect(await (await call("GET", "/linear/link", as(u.session))).json()).toEqual({
      links: [{ organization_id: ORG, linear_user_id: LU, installed: false }],
    });
  });
  it("says whether the app is installed in the workspace", async () => {
    const u = await flagged(1, "ada");
    proves("lin_api_ada");
    await env.DB.prepare(
      "INSERT INTO linear_workspaces (organization_id, name, app_user_id, access_token, refresh_token, expires_at, installed_by, installed_at) VALUES (?, 'Acme', 'app', 'a', 'r', 1, 1, 1)",
    )
      .bind(ORG)
      .run();
    expect(await (await link(u.session, "lin_api_ada")).json()).toMatchObject({ installed: true });
    expect(await (await call("GET", "/linear/link", as(u.session))).json()).toMatchObject({ links: [{ installed: true }] });
  });
  it("a second account proving the same Linear user takes the link over", async () => {
    const a = await flagged(1, "ada");
    const b = await flagged(2, "bob");
    proves("lin_api_x");
    expect((await link(a.session, "lin_api_x")).status).toBe(200);
    expect((await link(b.session, "lin_api_x")).status).toBe(200);
    expect(await links()).toEqual([{ organization_id: ORG, linear_user_id: LU, github_id: 2 }]);
    expect(await (await call("GET", "/linear/link", as(a.session))).json()).toEqual({ links: [] });
  });
  it("takes the first account's routes along when a link moves", async () => {
    const { owner, label } = await setup();
    const linked = await links();
    const lu = (linked[0] as { linear_user_id: string }).linear_user_id;
    expect((await put(owner.session, { label, organization_id: ORG, teams: [TEAM], projects: [] })).status).toBe(204);
    const other = await flagged(900, "eve");
    proves("lin_api_eve", lu);
    expect((await link(other.session, "lin_api_eve")).status).toBe(200);
    expect(await routes()).toEqual([]);
    expect(await links()).toEqual([{ organization_id: ORG, linear_user_id: lu, github_id: 900 }]);
  });
  it("refuses a token Linear refuses", async () => {
    const u = await flagged(1, "ada");
    const res = await link(u.session, "lin_api_nope");
    expect(res.status).toBe(400);
    expect(await res.json()).toEqual({ error: "linear refused that token" });
    expect(await links()).toEqual([]);
  });
  it("refuses a missing, odd or long token without calling Linear", async () => {
    const u = await flagged(1, "ada");
    for (const body of [json({}), json({ token: 5 }), json({ token: "" }), json({ token: "x".repeat(513) }), "nope"]) {
      const res = await call("POST", "/linear/link", as(u.session), body);
      expect(res.status).toBe(400);
    }
    expect(linearFake.calls).toEqual([]);
    expect(await links()).toEqual([]);
  });
  it("counts linking per account", async () => {
    // Its own account: the limiter keeps its count across tests.
    const u = await flagged(700, "rate");
    proves("lin_api_ada");
    let last = 0;
    for (let i = 0; i < 40 && last !== 429; i++) last = (await link(u.session, "lin_api_ada")).status;
    expect(last).toBe(429);
    expect(await (await link(u.session, "lin_api_ada")).json()).toEqual({ error: "too many requests" });
  });
  it("replaces a computer's routes", async () => {
    const { owner, label } = await setup();
    expect((await put(owner.session, { label, organization_id: ORG, teams: [TEAM, TEAM, TEAM2], projects: [PROJECT] })).status).toBe(204);
    expect(await routes()).toHaveLength(3);
    expect((await put(owner.session, { label, organization_id: ORG, teams: [TEAM2], projects: [] })).status).toBe(204);
    expect(await routes()).toEqual([{ label, github_id: owner.github_id, organization_id: ORG, kind: "team", target_id: TEAM2 }]);
    expect((await put(owner.session, { label, organization_id: ORG, teams: [], projects: [] })).status).toBe(204);
    expect(await routes()).toEqual([]);
  });
  it("refuses routes for a computer of another account, an unlinked workspace, or a bad id", async () => {
    const { owner, label } = await setup();
    const other = await flagged(900, "eve");
    const body = { label, organization_id: ORG, teams: [TEAM], projects: [] };
    const stranger = await put(other.session, body);
    expect(stranger.status).toBe(404);
    expect(await stranger.json()).toEqual({ error: "no such computer" });
    const unlinked = await put(owner.session, { ...body, organization_id: TEAM });
    expect(unlinked.status).toBe(409);
    expect(await unlinked.json()).toEqual({ error: "link linear first" });
    const bads = [
      { ...body, teams: ["nope"] },
      { ...body, teams: [5] },
      { ...body, teams: "x" },
      { ...body, projects: undefined },
      { ...body, organization_id: "nope" },
      { ...body, teams: Array.from({ length: 201 }, (_, i) => `33333333-3333-4333-8333-${String(i).padStart(12, "0")}`) },
    ];
    for (const bad of bads) {
      const res = await put(owner.session, bad);
      expect(res.status).toBe(400);
      expect(await res.json()).toEqual({ error: "bad routes" });
    }
    const big = await call("PUT", "/linear/routes", as(owner.session), json({ ...body, pad: "x".repeat(33_000) }));
    expect(big.status).toBe(400);
    expect(await routes()).toEqual([]);
  });
  it("a bad replacement leaves the earlier routes alone", async () => {
    const { owner, label } = await setup();
    await put(owner.session, { label, organization_id: ORG, teams: [TEAM], projects: [] });
    expect((await put(owner.session, { label, organization_id: ORG, teams: ["nope"], projects: [] })).status).toBe(400);
    expect(await routes()).toHaveLength(1);
  });
  it("picks the project route over the team route, then the computer seen last", async () => {
    const owner = await account();
    await call("PUT", `/admin/users/${owner.github_id}/flags/linear`, ADMIN);
    const a = (await connectedComputer(owner)).me.label;
    const b = (await connectedComputer(owner)).me.label;
    proves("lin_api_o", LU);
    expect((await link(owner.session, "lin_api_o")).status).toBe(200);
    await put(owner.session, { label: a, organization_id: ORG, teams: [TEAM], projects: [] });
    await put(owner.session, { label: b, organization_id: ORG, teams: [], projects: [PROJECT] });
    const id = owner.github_id;
    expect(await routeFor(env, id, ORG, { teamId: TEAM, projectId: PROJECT })).toMatchObject({ label: b });
    expect(await routeFor(env, id, ORG, { teamId: TEAM, projectId: null })).toMatchObject({ label: a });
    expect(await routeFor(env, id + 1000, ORG, { teamId: TEAM, projectId: PROJECT })).toBeNull();
    expect(await routeFor(env, id, TEAM, { teamId: TEAM, projectId: null })).toBeNull();
    // Equal rows: the newest last_seen_at wins.
    await put(owner.session, { label: b, organization_id: ORG, teams: [TEAM], projects: [] });
    await env.DB.prepare("UPDATE machines SET last_seen_at = 100 WHERE label = ?").bind(a).run();
    await env.DB.prepare("UPDATE machines SET last_seen_at = 200 WHERE label = ?").bind(b).run();
    expect(await routeFor(env, id, ORG, { teamId: TEAM, projectId: null })).toMatchObject({ label: b });
    await env.DB.prepare("UPDATE machines SET last_seen_at = 300 WHERE label = ?").bind(a).run();
    expect(await routeFor(env, id, ORG, { teamId: TEAM, projectId: null })).toMatchObject({ label: a });
  });
  it("does not route to a machine its account no longer owns", async () => {
    const { owner, label } = await setup();
    await put(owner.session, { label, organization_id: ORG, teams: [TEAM], projects: [] });
    await signIn(999, "mallory");
    await env.DB.prepare("UPDATE machines SET github_id = 999 WHERE label = ?").bind(label).run();
    expect(await routeFor(env, owner.github_id, ORG, { teamId: TEAM, projectId: null })).toBeNull();
  });
  it("forgetting a computer drops its routes", async () => {
    const { owner, label } = await setup();
    await put(owner.session, { label, organization_id: ORG, teams: [TEAM], projects: [] });
    expect((await call("DELETE", `/machines/${label}`, as(owner.session))).status).toBe(204);
    expect(await routes()).toEqual([]);
    expect(await routeFor(env, owner.github_id, ORG, { teamId: TEAM, projectId: null })).toBeNull();
  });
  it("unlinking drops that workspace's routes", async () => {
    const { owner, label } = await setup();
    await put(owner.session, { label, organization_id: ORG, teams: [TEAM], projects: [] });
    const res = await call("DELETE", `/linear/link/${ORG}`, as(owner.session));
    expect(res.status).toBe(204);
    expect(await links()).toEqual([]);
    expect(await routes()).toEqual([]);
    expect(await routeFor(env, owner.github_id, ORG, { teamId: TEAM, projectId: null })).toBeNull();
    expect((await call("DELETE", `/linear/link/${ORG}`, as(owner.session))).status).toBe(204);
    expect((await call("DELETE", "/linear/link/nope", as(owner.session))).status).toBe(404);
  });
  it("unlinking leaves another account's link alone", async () => {
    const a = await flagged(1, "ada");
    const b = await flagged(2, "bob");
    proves("lin_api_x");
    await link(a.session, "lin_api_x");
    expect((await call("DELETE", `/linear/link/${ORG}`, as(b.session))).status).toBe(204);
    expect(await links()).toHaveLength(1);
  });
});
