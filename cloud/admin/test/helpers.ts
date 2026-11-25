// A fake GitHub, after ../../test/helpers.ts. The Worker runs in the same
// isolate as the tests, so replacing the global fetch reaches it.
import { env } from "cloudflare:test";
import { vi } from "vitest";

/// The Worker's bindings with the client ID the fake GitHub knows.
export const testEnv: Env = { ...env, GITHUB_CLIENT_ID: "Iv-test" };

/// What the fake GitHub knows and what it was asked.
export const github = {
  /// Access token → the user GitHub would answer with.
  users: new Map<string, { id: number; login: string }>(),
  /// Code → the access token GitHub's web flow would give for it. A code is
  /// good once.
  codes: new Map<string, string>(),
  /// "throw": the network fails; a number: GitHub answers with that status.
  outage: null as "throw" | number | null,
  /// The same, for revoking a token only.
  revokeOutage: null as "throw" | number | null,
  /// "throw", a status, or "error" (a 200 with `{ error }`, as GitHub refuses a code).
  codeOutage: null as "throw" | "error" | number | null,
  /// The bodies the code exchange was sent.
  exchanges: [] as string[],
  /// Every call to the Applications API, and every other URL fetched.
  calls: [] as Request[],
  urls: [] as string[],
};

export function fakeGitHub(): void {
  github.users.clear();
  github.codes.clear();
  github.outage = null;
  github.revokeOutage = null;
  github.codeOutage = null;
  github.exchanges = [];
  github.calls = [];
  github.urls = [];
  vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
    const req = new Request(input, init);
    github.urls.push(req.url);
    if (req.url === "https://github.com/login/oauth/access_token" && req.method === "POST") return exchangeCode(req);
    if (req.url !== "https://api.github.com/applications/Iv-test/token" || (req.method !== "POST" && req.method !== "DELETE"))
      throw new Error(`unexpected fetch: ${req.method} ${req.url}`);
    const body = await req.text();
    github.calls.push(req);
    const outage = req.method === "DELETE" ? (github.revokeOutage ?? github.outage) : github.outage;
    if (outage === "throw") throw new TypeError("Network connection lost.");
    if (outage !== null) return new Response("unavailable", { status: outage });
    // What api.github.com answers to a wrong client ID or secret.
    if (req.headers.get("Authorization") !== `Basic ${btoa("Iv-test:client-secret")}`)
      return Response.json({ message: "Bad credentials" }, { status: 401 });
    const token = (JSON.parse(body) as { access_token?: string }).access_token;
    const user = token ? github.users.get(token) : undefined;
    // GitHub answers 404 for a token this app did not issue, or revoked.
    if (!user) return Response.json({ message: "Not Found" }, { status: 404 });
    if (req.method === "DELETE") {
      github.users.delete(token!);
      return new Response(null, { status: 204 });
    }
    // Real answers carry much more; the Worker must keep none of it.
    return Response.json({ id: 1, token: null, user: { ...user, email: "someone@example.com", name: "Some One" } });
  });
}

/// POST /login/oauth/access_token: GitHub answers 200 even for a bad code.
async function exchangeCode(req: Request): Promise<Response> {
  const body = await req.text();
  github.exchanges.push(body);
  if (github.codeOutage === "throw") throw new TypeError("Network connection lost.");
  if (typeof github.codeOutage === "number") return new Response("unavailable", { status: github.codeOutage });
  const sent = JSON.parse(body) as { client_id?: string; client_secret?: string; code?: string };
  const token = sent.code ? github.codes.get(sent.code) : undefined;
  if (github.codeOutage === "error" || sent.client_id !== "Iv-test" || sent.client_secret !== "client-secret" || !token)
    return Response.json({ error: "bad_verification_code", error_description: "The code passed is incorrect or expired." });
  github.codes.delete(sent.code!);
  return Response.json({ access_token: token, token_type: "bearer", scope: "" });
}

/// GitHub has issued this code for a token of this user.
export function issue(code: string, token: string, user: { id: number; login: string }): void {
  github.codes.set(code, token);
  github.users.set(token, user);
}

let nextIp = 1;

/// A different client IP per call, so tests don't share a rate limit.
export function freshIp(): string {
  const n = nextIp++;
  return `10.${(n >> 16) & 255}.${(n >> 8) & 255}.${n & 255}`;
}
