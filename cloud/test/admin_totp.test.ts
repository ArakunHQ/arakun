import { env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { ADMIN, call, signIn } from "./helpers";

const PATH = "/admin/operators/1001/totp";
const SEALED = "A".repeat(70);
const JSON_ADMIN = { ...ADMIN, "Content-Type": "application/json" };
const NAMED = { ...JSON_ADMIN, "X-Arakun-Operator-Id": "1001", "X-Arakun-Operator-Login": "octocat" };

function send(method: string, path: string, body: unknown, headers = JSON_ADMIN): Promise<Response> {
  return call(method, path, headers, JSON.stringify(body));
}

async function log(): Promise<{ action: string; account_id: number | null; operator_login: string | null; detail: string | null }[]> {
  const rows = await env.DB.prepare("SELECT action, account_id, operator_login, detail FROM admin_log ORDER BY id").all();
  return rows.results as never;
}

describe("operator authenticator keys", () => {
  it("are kept once, read back, and not replaced by a second first key", async () => {
    expect((await call("GET", PATH, ADMIN)).status).toBe(404);
    expect((await send("POST", PATH, { sealed: SEALED, step: 100 }, NAMED)).status).toBe(201);
    const got = await call("GET", PATH, ADMIN);
    expect(await got.json()).toEqual({ sealed: SEALED, last_step: 100 });
    const again = await send("POST", PATH, { sealed: "B".repeat(70), step: 101 });
    expect(again.status).toBe(409);
    expect(await again.json()).toEqual({ error: "a key exists" });
    expect(await (await call("GET", PATH, ADMIN)).json()).toEqual({ sealed: SEALED, last_step: 100 });
    // The operator has no account here: the log names their GitHub ID only.
    expect(await log()).toEqual([{ action: "operator.totp.add", account_id: null, operator_login: "octocat", detail: '{"github_id":1001}' }]);
  });

  it("log the operator's account when their GitHub user has one", async () => {
    const op = await signIn(1001, "octocat");
    expect((await send("POST", PATH, { sealed: SEALED, step: 100 }, NAMED)).status).toBe(201);
    expect((await log()).map((r) => r.account_id)).toEqual([op.account_id]);
  });

  it("are replaced with PUT and forgotten with DELETE", async () => {
    expect((await send("PUT", PATH, { sealed: SEALED, step: 100 })).status).toBe(204);
    expect((await send("PUT", PATH, { sealed: "B".repeat(70), step: 120 })).status).toBe(204);
    expect(await (await call("GET", PATH, ADMIN)).json()).toEqual({ sealed: "B".repeat(70), last_step: 120 });
    expect((await call("DELETE", PATH, ADMIN)).status).toBe(204);
    expect((await call("GET", PATH, ADMIN)).status).toBe(404);
    // Forgetting none is fine, and not recorded.
    expect((await call("DELETE", PATH, ADMIN)).status).toBe(204);
    expect((await log()).map((r) => r.action)).toEqual(["operator.totp.replace", "operator.totp.replace", "operator.totp.reset"]);
  });

  it("take each code's step once, and only a newer one", async () => {
    expect((await send("POST", `${PATH}/use`, { step: 101 })).status).toBe(409);
    await send("POST", PATH, { sealed: SEALED, step: 100 });
    expect((await send("POST", `${PATH}/use`, { step: 100 })).status).toBe(409);
    expect((await send("POST", `${PATH}/use`, { step: 102 })).status).toBe(204);
    expect((await send("POST", `${PATH}/use`, { step: 102 })).status).toBe(409);
    expect((await send("POST", `${PATH}/use`, { step: 101 })).status).toBe(409);
    expect(await (await call("GET", PATH, ADMIN)).json()).toEqual({ sealed: SEALED, last_step: 102 });
  });

  it("refuse bad bodies and IDs, other methods, and calls without the secret", async () => {
    for (const body of [{}, { sealed: SEALED }, { sealed: "short", step: 1 }, { sealed: `${SEALED}!`, step: 1 }, { sealed: SEALED, step: 0 }, { sealed: SEALED, step: 1.5 }]) {
      expect((await send("POST", PATH, body)).status, JSON.stringify(body)).toBe(400);
    }
    expect((await send("POST", `${PATH}/use`, { step: "1" })).status).toBe(400);
    expect((await call("GET", "/admin/operators/abc/totp", ADMIN)).status).toBe(400);
    expect((await call("PATCH", PATH, ADMIN)).status).toBe(404);
    expect((await call("GET", `${PATH}/use`, ADMIN)).status).toBe(404);
    expect((await call("GET", PATH)).status).toBe(401);
    expect((await send("POST", PATH, { sealed: SEALED, step: 1 }, { "Content-Type": "application/json" } as never)).status).toBe(401);
  });
});
