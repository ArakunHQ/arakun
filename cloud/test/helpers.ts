// A fake GitHub and short ways to call the Worker under test.
import { SELF } from "cloudflare:test";
import { vi } from "vitest";

export const ADMIN = { Authorization: "Bearer test-secret" };

/// What the fake GitHub knows and what it was asked. The Worker runs in the
/// same isolate as the tests, so replacing the global fetch reaches it.
export const github = {
  /// Access token → the user GitHub would answer with.
  users: new Map<string, { id: number; login: string }>(),
  /// Lower-case login → who has it now, for GET /users/<login>. Signing in
  /// adds the user; a test can give a login to someone else.
  accounts: new Map<string, { id: number; login: string; type?: string }>(),
  /// "throw": the network fails; a number: GitHub answers with that status.
  outage: null as "throw" | number | null,
  /// The same, for revoking a token only.
  revokeOutage: null as "throw" | number | null,
  calls: [] as Request[],
  /// The body of each call, read once by the fake.
  bodies: [] as string[],
};

export function fakeGitHub(): void {
  github.users.clear();
  github.accounts.clear();
  github.outage = null;
  github.revokeOutage = null;
  github.calls = [];
  github.bodies = [];
  vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
    const req = new Request(input, init);
    const lookup = /^https:\/\/api\.github\.com\/users\/([^/]+)$/.exec(req.url);
    if (lookup && req.method === "GET") return lookupUser(req, decodeURIComponent(lookup[1]!));
    if (req.url !== "https://api.github.com/applications/Iv-test/token" || (req.method !== "POST" && req.method !== "DELETE"))
      throw new Error(`unexpected fetch: ${req.method} ${req.url}`);
    const body = await req.text();
    github.calls.push(req);
    github.bodies.push(body);
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

/// GET /users/<login>: public data, asked with the app's credentials.
function lookupUser(req: Request, login: string): Response {
  github.calls.push(req);
  github.bodies.push("");
  if (github.outage === "throw") throw new TypeError("Network connection lost.");
  if (github.outage !== null) return new Response("unavailable", { status: github.outage });
  if (req.headers.get("Authorization") !== `Basic ${btoa("Iv-test:client-secret")}`)
    return Response.json({ message: "Bad credentials" }, { status: 401 });
  const account = github.accounts.get(login.toLowerCase());
  if (!account) return Response.json({ message: "Not Found" }, { status: 404 });
  return Response.json({ type: "User", ...account, name: "Some One" });
}

/// Tells the fake GitHub that this account has the login now.
export function onGitHub(id: number, login: string, type = "User"): void {
  github.accounts.set(login.toLowerCase(), { id, login, type });
}

let nextIp = 1;

/// A different client IP per call, so tests don't share a rate limit.
export function freshIp(): string {
  const n = nextIp++;
  return `10.${(n >> 16) & 255}.${(n >> 8) & 255}.${n & 255}`;
}

export function signInRequest(accessToken: unknown, ip = freshIp()): Promise<Response> {
  return SELF.fetch("https://teams.test/auth/github", {
    method: "POST",
    headers: { "Content-Type": "application/json", "CF-Connecting-IP": ip },
    body: JSON.stringify({ access_token: accessToken }),
  });
}

export interface SignedIn {
  session: string;
  expires_at: number;
  github_id: number;
  login: string;
  flags: string[];
}

/// Signs in as a user GitHub knows, and returns the Worker's answer.
export async function signIn(id: number, login: string, accessToken = `gho_${id}_${login}`): Promise<SignedIn> {
  github.users.set(accessToken, { id, login });
  onGitHub(id, login);
  const res = await signInRequest(accessToken);
  if (res.status !== 200) throw new Error(`sign-in failed: ${res.status} ${await res.text()}`);
  return res.json();
}

export function call(method: string, path: string, headers: Record<string, string> = {}): Promise<Response> {
  return SELF.fetch(`https://teams.test${path}`, { method, headers });
}

export function as(session: string): Record<string, string> {
  return { Authorization: `Bearer ${session}` };
}

export function nowSeconds(): number {
  return Math.floor(Date.now() / 1000);
}

/// Waits for the next rate-limit window when this one has less than `roomMs`
/// left. The simulated limiter counts in windows fixed to the clock, so a
/// test that sends its burst across the end of one sees every call pass.
export async function roomInWindow(periodSeconds: 10 | 60, roomMs = 3_000): Promise<void> {
  const period = periodSeconds * 1000;
  const left = period - (Date.now() % period);
  if (left < roomMs) await new Promise((r) => setTimeout(r, left + 50));
}
