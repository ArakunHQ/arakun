// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// A cloud machine's GitHub access: which requests are git's, the app's JWT,
// the token for one repository and who may have it, and the handler git's
// traffic goes through, against a fake GitHub App API.
import { env, runInDurableObject } from "cloudflare:test";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { CLOUD_FLAG } from "../src/cloud_access";
import { ANONYMOUS_MS, appJwt, basicAuth, GitHubGit, installUrl, parseGitRequest, pkcs1ToPkcs8, TOKEN_MARGIN_MS, TokenCache } from "../src/cloud_github";
import { MachineLife, newMachineKey, type MachineRecord } from "../src/cloud_machine";
import { fromB64url } from "../src/util";
import { ADMIN, call, signIn, signInByEmail } from "./helpers";

let nextId = 90_000;

/// What the fake GitHub App API knows and was asked.
const app = {
  /// `owner/repo` (lower case) → installation ID, for repositories it is on.
  installed: new Map<string, number>(),
  /// `owner/repo` (lower case) → GitHub login → permission.
  perms: new Map<string, Map<string, string>>(),
  /// GitHub user ID → login.
  logins: new Map<number, string>(),
  /// The bodies of the token requests, and the tokens revoked.
  minted: [] as { installation: number; body: { repositories: string[]; permissions: Record<string, string> } }[],
  revoked: [] as string[],
  /// What git's requests looked like when they reached github.com.
  git: [] as Request[],
  /// `owner/repo` (lower case) that github.com answers 401 without a token.
  private: new Set<string>(),
  /// The JWTs the app signed in with.
  jwts: [] as string[],
};

function repoKey(owner: string, repo: string): string {
  return `${owner}/${repo}`.toLowerCase();
}

beforeEach(() => {
  app.installed.clear();
  app.perms.clear();
  app.logins.clear();
  app.minted = [];
  app.revoked = [];
  app.git = [];
  app.jwts = [];
  app.private.clear();
  const prior = vi.mocked(globalThis.fetch).getMockImplementation()!;
  vi.mocked(globalThis.fetch).mockImplementation(async (input, init) => {
    const req = new Request(input, init);
    const url = new URL(req.url);
    const auth = req.headers.get("Authorization") ?? "";
    if (url.hostname === "github.com") {
      app.git.push(req);
      const git = parseGitRequest(url);
      if (git && app.private.has(repoKey(git.owner, git.repo)) && !auth) return new Response("Authentication required", { status: 401 });
      return new Response("pack", { status: 200 });
    }
    if (url.hostname !== "api.github.com") return prior(input, init);
    let m = /^\/repos\/([^/]+)\/([^/]+)\/installation$/.exec(url.pathname);
    if (m && req.method === "GET") {
      app.jwts.push(auth.replace(/^Bearer /, ""));
      const id = app.installed.get(repoKey(m[1]!, m[2]!));
      return id ? Response.json({ id }) : Response.json({ message: "Not Found" }, { status: 404 });
    }
    m = /^\/app\/installations\/(\d+)\/access_tokens$/.exec(url.pathname);
    if (m && req.method === "POST") {
      const body = await req.json<{ repositories: string[]; permissions: Record<string, string> }>();
      app.minted.push({ installation: Number(m[1]), body });
      const n = app.minted.length;
      return Response.json({ token: `ghs_${n}`, expires_at: new Date(Date.now() + 3600_000).toISOString() }, { status: 201 });
    }
    m = /^\/user\/(\d+)$/.exec(url.pathname);
    if (m && req.method === "GET" && auth.startsWith("Bearer ghs_")) {
      const login = app.logins.get(Number(m[1]));
      return login ? Response.json({ id: Number(m[1]), login }) : Response.json({ message: "Not Found" }, { status: 404 });
    }
    m = /^\/repos\/([^/]+)\/([^/]+)\/collaborators\/([^/]+)\/permission$/.exec(url.pathname);
    if (m && req.method === "GET") {
      const level = app.perms.get(repoKey(m[1]!, m[2]!))?.get(m[3]!.toLowerCase());
      return level ? Response.json({ permission: level }) : Response.json({ message: "Not Found" }, { status: 404 });
    }
    if (url.pathname === "/installation/token" && req.method === "DELETE") {
      app.revoked.push(auth.replace(/^Bearer /, ""));
      return new Response(null, { status: 204 });
    }
    return prior(input, init);
  });
});

/// An owner with the `cloud` flag and a machine record, signed in with GitHub.
async function owner(flag = true): Promise<{ accountId: number; githubId: number; login: string }> {
  const id = nextId++;
  const login = `dev${id}`;
  const me = await signIn(id, login);
  if (flag) expect((await call("PUT", `/admin/users/${me.account_id}/flags/${CLOUD_FLAG}`, ADMIN)).status).toBe(200);
  app.logins.set(id, login);
  return { accountId: me.account_id, githubId: id, login };
}

async function machineFor(accountId: number): Promise<MachineRecord> {
  return { ...(await newMachineKey()), githubId: accountId, name: "Cloud", base: "https://app.arakun.dev" };
}

/// Runs `body` with a machine's lifecycle, with no container.
async function withMachine<T>(m: MachineRecord, body: (life: MachineLife) => Promise<T>): Promise<T> {
  const stub = env.CLOUD_MACHINE!.getByName(m.label);
  return runInDurableObject(stub, async (_i, state) => {
    const life = new MachineLife(state.storage, undefined, env);
    if (!(await life.record())) await life.create(m);
    return body(life);
  });
}

function give(owner: string, repo: string, login: string, permission: string, installation = 77): void {
  app.installed.set(repoKey(owner, repo), installation);
  const perms = app.perms.get(repoKey(owner, repo)) ?? new Map<string, string>();
  perms.set(login.toLowerCase(), permission);
  app.perms.set(repoKey(owner, repo), perms);
}

describe("a git request on github.com", () => {
  const parse = (u: string) => parseGitRequest(new URL(u));

  it("names the repository and whether it pushes", () => {
    expect(parse("https://github.com/octo/hello.git/info/refs?service=git-upload-pack")).toEqual({ owner: "octo", repo: "hello", push: false });
    expect(parse("https://github.com/octo/hello/git-upload-pack")).toEqual({ owner: "octo", repo: "hello", push: false });
    expect(parse("https://github.com/octo/hello.git/info/refs?service=git-receive-pack")).toEqual({ owner: "octo", repo: "hello", push: true });
    expect(parse("https://github.com/octo/hello.git/git-receive-pack")).toEqual({ owner: "octo", repo: "hello", push: true });
    expect(parse("https://github.com/Octo-Org/my.repo.git/git-upload-pack")).toEqual({ owner: "Octo-Org", repo: "my.repo", push: false });
  });

  it("is nothing for the rest of github.com", () => {
    for (const u of [
      "https://github.com/octo/hello",
      "https://github.com/octo/hello.git/info/refs",
      "https://github.com/octo/hello.git/info/refs?service=other",
      "https://github.com/octo/hello/releases/download/v1/x.tar.gz",
      "https://github.com/octo/../git-upload-pack",
      "https://github.com/octo/.git/git-upload-pack",
      "https://github.com/login/oauth/access_token",
    ]) {
      expect(parse(u), u).toBeNull();
    }
  });
});

describe("the app's JWT", () => {
  it("is signed with the PKCS#1 key GitHub hands out, for nine minutes from a minute back", async () => {
    const jwt = await appJwt(env, 1_000_000);
    const [head, claims, sig] = jwt.split(".");
    const dec = (p: string) => JSON.parse(new TextDecoder().decode(fromB64url(p)!));
    expect(dec(head!)).toEqual({ alg: "RS256", typ: "JWT" });
    expect(dec(claims!)).toEqual({ iat: 999_940, exp: 1_000_540, iss: "4242" });
    const pub = await crypto.subtle.importKey("jwk", JSON.parse(env.TEST_GITHUB_APP_PUBLIC), { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" }, false, ["verify"]);
    const ok = await crypto.subtle.verify("RSASSA-PKCS1-v1_5", pub, fromB64url(sig!)!, new TextEncoder().encode(`${head}.${claims}`));
    expect(ok).toBe(true);
  });

  it("wraps a long PKCS#1 key with DER lengths of two bytes", () => {
    const out = pkcs1ToPkcs8(new Uint8Array(1200));
    expect([...out.slice(0, 4)]).toEqual([0x30, 0x82, 0x04, 0xc6]);
    expect(out.length).toBe(4 + 0x4c6);
  });
});

describe("the token cache", () => {
  it("gives a token back until it has ten minutes left, per repository and access", () => {
    const cache = new TokenCache();
    const read = { owner: "Octo", repo: "Hello", push: false };
    cache.put(read, "t1", 1_000_000 + TOKEN_MARGIN_MS + 1);
    expect(cache.get({ owner: "octo", repo: "hello", push: false }, 1_000_000)).toEqual({ token: "t1", expiresAt: 1_000_000 + TOKEN_MARGIN_MS + 1 });
    expect(cache.get({ ...read, push: true }, 1_000_000)).toBeNull();
    expect(cache.get(read, 1_000_001)).toBeNull();
  });

  it("lets a fetch go on without a token for a short while", () => {
    const cache = new TokenCache();
    const read = { owner: "octo", repo: "public", push: false };
    cache.putAnonymous(read, "no", 1_000_000);
    expect(cache.get(read, 1_000_000 + ANONYMOUS_MS - 1)).toEqual({ refused: "no", anonymous: true });
    expect(cache.get(read, 1_000_000 + ANONYMOUS_MS)).toBeNull();
  });
});

describe("a machine's access to a repository", () => {
  it("is a token for that one repository, read for a fetch and write for a push, made once", async () => {
    const me = await owner();
    give("octo", "hello", me.login, "write");
    const m = await machineFor(me.accountId);
    await withMachine(m, async (life) => {
      const read = await life.gitAccess({ owner: "octo", repo: "hello", push: false });
      expect(read).toMatchObject({ token: "ghs_1" });
      expect(await life.gitAccess({ owner: "octo", repo: "hello", push: false })).toMatchObject({ token: "ghs_1" });
      expect(await life.gitAccess({ owner: "octo", repo: "hello", push: true })).toMatchObject({ token: "ghs_2" });
    });
    expect(app.minted).toEqual([
      { installation: 77, body: { repositories: ["hello"], permissions: { contents: "read", metadata: "read" } } },
      { installation: 77, body: { repositories: ["hello"], permissions: { contents: "write", metadata: "read" } } },
    ]);
    // The JWTs name the app and come from its key.
    expect(app.jwts.length).toBe(2);
  });

  it("is refused with the install link where the app is not installed", async () => {
    const me = await owner();
    const m = await machineFor(me.accountId);
    const got = await withMachine(m, (life) => life.gitAccess({ owner: "octo", repo: "private", push: false }));
    // A fetch may still go on without a token: the repository may be public.
    expect(got).toEqual({ refused: expect.stringContaining(installUrl(env)), anonymous: true });
    expect(installUrl(env)).toBe("https://github.com/apps/arakun-cloud-test/installations/new");
    expect(app.minted).toEqual([]);
  });

  it("is refused when the owner's GitHub account may not do it, and the token is ended", async () => {
    const me = await owner();
    give("octo", "hello", me.login, "read");
    app.installed.set(repoKey("someone", "else"), 78);
    const m = await machineFor(me.accountId);
    await withMachine(m, async (life) => {
      expect(await life.gitAccess({ owner: "octo", repo: "hello", push: true })).toEqual({ refused: expect.stringContaining("cannot write to it"), anonymous: false });
      // Installed by someone else, on a repository the owner cannot see.
      expect(await life.gitAccess({ owner: "someone", repo: "else", push: false })).toEqual({ refused: expect.stringContaining("has no access to it"), anonymous: true });
      expect(await life.gitAccess({ owner: "octo", repo: "hello", push: false })).toMatchObject({ token: "ghs_3" });
    });
    expect(app.revoked).toEqual(["ghs_1", "ghs_2"]);
  });

  it("is refused for an account without the flag, or without GitHub", async () => {
    const me = await owner(false);
    give("octo", "hello", me.login, "admin");
    expect(await withMachine(await machineFor(me.accountId), (life) => life.gitAccess({ owner: "octo", repo: "hello", push: false }))).toEqual({
      refused: expect.stringContaining("may not run a cloud machine"),
    });
    const byEmail = await signInByEmail(`cloud${nextId++}@example.com`);
    expect((await call("PUT", `/admin/users/${byEmail.account_id}/flags/${CLOUD_FLAG}`, ADMIN)).status).toBe(200);
    expect(await withMachine(await machineFor(byEmail.account_id), (life) => life.gitAccess({ owner: "octo", repo: "hello", push: false }))).toEqual({
      refused: expect.stringContaining("sign in to Arakun with GitHub"),
      anonymous: true,
    });
    expect(app.minted).toEqual([]);
  });
});

describe("the handler of a machine's github.com traffic", () => {
  async function handler(label: string): Promise<GitHubGit> {
    return new GitHubGit({ props: { label } } as unknown as ExecutionContext, env);
  }

  async function machine(): Promise<{ label: string; login: string }> {
    const me = await owner();
    const m = await machineFor(me.accountId);
    await env.CLOUD_MACHINE!.getByName(m.label).create(m);
    return { label: m.label, login: me.login };
  }

  it("adds the token to git's requests and passes them on", async () => {
    const { label, login } = await machine();
    give("octo", "hello", login, "maintain");
    const h = await handler(label);
    const res = await h.fetch(
      new Request("https://github.com/octo/hello.git/git-receive-pack", { method: "POST", headers: { Authorization: "Basic bm9wZQ==" }, body: "pack" }),
    );
    expect(res.status).toBe(200);
    expect(app.git.length).toBe(1);
    expect(app.git[0]!.headers.get("Authorization")).toBe(basicAuth("ghs_1"));
    expect(app.git[0]!.redirect).toBe("manual");
    expect(await app.git[0]!.text()).toBe("pack");
  });

  it("refuses a private repository without the app in words git shows, with the link", async () => {
    const { label } = await machine();
    app.private.add(repoKey("octo", "nope"));
    const res = await (await handler(label)).fetch(new Request("https://github.com/octo/nope.git/info/refs?service=git-upload-pack"));
    expect(res.status).toBe(403);
    expect(res.headers.get("Content-Type")).toMatch(/^text\/plain/);
    expect(await res.text()).toContain(installUrl(env));
    expect(app.git.map((r) => r.headers.get("Authorization"))).toEqual([null]);
  });

  it("fetches a public repository without the app with no token, and asks GitHub once", async () => {
    const { label } = await machine();
    const h = await handler(label);
    const refs = await h.fetch(new Request("https://github.com/octo/open.git/info/refs?service=git-upload-pack"));
    expect(refs.status).toBe(200);
    const pack = await h.fetch(new Request("https://github.com/octo/open.git/git-upload-pack", { method: "POST", body: "want" }));
    expect(pack.status).toBe(200);
    expect(app.git.map((r) => r.headers.get("Authorization"))).toEqual([null, null]);
    expect(app.jwts.length).toBe(1);
    // A push gets no such pass.
    const push = await h.fetch(new Request("https://github.com/octo/open.git/info/refs?service=git-receive-pack"));
    expect(push.status).toBe(403);
    expect(app.git.length).toBe(2);
  });

  it("passes the rest of github.com on without a token, and nothing to another host", async () => {
    const { label } = await machine();
    const h = await handler(label);
    expect((await h.fetch(new Request("https://github.com/octo/hello/archive/main.zip"))).status).toBe(200);
    expect(app.git[0]!.headers.get("Authorization")).toBeNull();
    expect((await h.fetch(new Request("https://evil.example/octo/hello.git/git-upload-pack"))).status).toBe(421);
    expect((await h.fetch(new Request("http://github.com/octo/hello.git/git-upload-pack"))).status).toBe(421);
    expect(app.git.length).toBe(1);
    expect(app.minted).toEqual([]);
  });
});
