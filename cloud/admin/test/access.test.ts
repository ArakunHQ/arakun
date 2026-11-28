// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
import { beforeEach, describe, expect, it, vi } from "vitest";
import { readOperator } from "../worker/access";
import { AUD, EMAIL, TEAM, access, fakeAccess, rotate, strangerKey, testEnv, token } from "./helpers";

const now = () => Math.floor(Date.now() / 1000);

function request(jwt: string | null, url = "https://admin.test/api/me"): Request {
  return new Request(url, { headers: jwt === null ? {} : { "Cf-Access-Jwt-Assertion": jwt } });
}

function read(jwt: string | null, env: Env = testEnv, url?: string) {
  return readOperator(request(jwt, url), env, now());
}

beforeEach(async () => {
  await fakeAccess();
});

describe("readOperator", () => {
  it("takes the operator's email from a token the team signed", async () => {
    expect(await read(await token())).toEqual({ email: EMAIL });
  });

  it("takes an aud given as a string too", async () => {
    expect(await read(await token({ aud: AUD }))).toEqual({ email: EMAIL });
  });

  it("refuses a request without a token", async () => {
    expect(await read(null)).toBeNull();
    expect(await read("")).toBeNull();
  });

  it("refuses a token for another application or team", async () => {
    expect(await read(await token({ aud: ["other-aud"] }))).toBeNull();
    expect(await read(await token({ iss: "https://other.cloudflareaccess.com" }))).toBeNull();
  });

  it("refuses an expired token and one not valid yet", async () => {
    expect(await read(await token({ exp: now() - 120 }))).toBeNull();
    expect(await read(await token({ nbf: now() + 120 }))).toBeNull();
    expect(await read(await token({ exp: "never" }))).toBeNull();
  });

  it("refuses a service token, which has no email", async () => {
    expect(await read(await token({ email: undefined, common_name: "abc.access" }))).toBeNull();
    expect(await read(await token({ email: "not an email" }))).toBeNull();
  });

  it("refuses a token signed with a key the team does not publish", async () => {
    const stranger = await strangerKey();
    expect(await read(await token({}, { signWith: stranger.privateKey }))).toBeNull();
  });

  it("refuses a changed token", async () => {
    const [head, , sig] = (await token()).split(".");
    const body = btoa(JSON.stringify({ aud: [AUD], email: "evil@example.com", exp: now() + 60, iss: `https://${TEAM}` }))
      .replace(/\+/g, "-")
      .replace(/\//g, "_")
      .replace(/=+$/, "");
    expect(await read(`${head}.${body}.${sig}`)).toBeNull();
  });

  it("refuses another algorithm, an unsigned token and junk", async () => {
    expect(await read(await token({}, { header: { alg: "HS256" } }))).toBeNull();
    const [head, body] = (await token({}, { header: { alg: "none" } })).split(".");
    expect(await read(`${head}.${body}.`)).toBeNull();
    for (const junk of ["a.b.c", "abc", "a.b", "....", "x".repeat(9000)]) expect(await read(junk), junk).toBeNull();
  });

  it("fetches the team's keys once and keeps them", async () => {
    for (let i = 0; i < 3; i++) expect(await read(await token())).toEqual({ email: EMAIL });
    expect(access.fetches).toBe(1);
  });

  it("fetches the keys again for a key it has not seen, at most once a minute", async () => {
    expect(await read(await token())).not.toBeNull();
    await rotate("k2");
    // Fetched a moment ago: a new key ID waits for the next minute.
    expect(await read(await token({}, { kid: "k2" }))).toBeNull();
    expect(access.fetches).toBe(1);
    const later = now() + 61;
    const op = await readOperator(request(await token({ exp: later + 600 }, { kid: "k2" })), testEnv, later);
    expect(op).toEqual({ email: EMAIL });
    expect(access.fetches).toBe(2);
  });

  it("lets nobody in when the keys cannot be fetched", async () => {
    const quiet = vi.spyOn(console, "error").mockImplementation(() => {});
    access.outage = 503;
    expect(await read(await token())).toBeNull();
    access.outage = "throw";
    expect(await read(await token(), testEnv)).toBeNull();
    quiet.mockRestore();
  });

  it("lets nobody in while Access is not configured", async () => {
    const jwt = await token();
    for (const env of [
      { ...testEnv, ACCESS_AUD: undefined },
      { ...testEnv, ACCESS_TEAM_DOMAIN: undefined },
      { ...testEnv, ACCESS_AUD: "PLACEHOLDER" },
      { ...testEnv, ACCESS_TEAM_DOMAIN: "evil.example.com" },
    ])
      expect(await read(jwt, env)).toBeNull();
    expect(access.fetches).toBe(0);
  });

  it("takes DEV_OPERATOR_EMAIL only on localhost and only without Access", async () => {
    const dev = { ...testEnv, ACCESS_TEAM_DOMAIN: undefined, ACCESS_AUD: undefined, DEV_OPERATOR_EMAIL: EMAIL };
    expect(await read(null, dev, "http://localhost:5173/api/me")).toEqual({ email: EMAIL });
    expect(await read(null, dev, "http://127.0.0.1:5173/api/me")).toEqual({ email: EMAIL });
    expect(await read(null, dev, "https://admin.arakun.dev/api/me")).toBeNull();
    expect(await read(null, { ...testEnv, DEV_OPERATOR_EMAIL: EMAIL }, "http://localhost:5173/api/me")).toBeNull();
  });
});
