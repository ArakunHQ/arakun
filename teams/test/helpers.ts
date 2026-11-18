// A fake GitHub and short ways to call the Worker under test.
import { SELF } from "cloudflare:test";
import { vi } from "vitest";

export const ADMIN = { Authorization: "Bearer test-secret" };

/// What the fake GitHub knows and what it was asked. The Worker runs in the
/// same isolate as the tests, so replacing the global fetch reaches it.
export const github = {
  /// Access token → the user GitHub would answer with.
  users: new Map<string, { id: number; login: string }>(),
  /// "throw": the network fails; a number: GitHub answers with that status.
  outage: null as "throw" | number | null,
  calls: [] as Request[],
  /// The body of each call, read once by the fake.
  bodies: [] as string[],
};

export function fakeGitHub(): void {
  github.users.clear();
  github.outage = null;
  github.calls = [];
  github.bodies = [];
  vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
    const req = new Request(input, init);
    if (req.url !== "https://api.github.com/applications/Iv-test/token" || req.method !== "POST")
      throw new Error(`unexpected fetch: ${req.method} ${req.url}`);
    const body = await req.text();
    github.calls.push(req);
    github.bodies.push(body);
    if (github.outage === "throw") throw new TypeError("Network connection lost.");
    if (github.outage !== null) return new Response("unavailable", { status: github.outage });
    if (req.headers.get("Authorization") !== `Basic ${btoa("Iv-test:client-secret")}`)
      return Response.json({ message: "Requires authentication" }, { status: 401 });
    const token = (JSON.parse(body) as { access_token?: string }).access_token;
    const user = token ? github.users.get(token) : undefined;
    // GitHub answers 404 for a token this app did not issue.
    if (!user) return Response.json({ message: "Not Found" }, { status: 404 });
    // Real answers carry much more; the Worker must keep none of it.
    return Response.json({ id: 1, token: null, user: { ...user, email: "someone@example.com", name: "Some One" } });
  });
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
