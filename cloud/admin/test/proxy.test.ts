import { beforeEach, describe, expect, it, vi } from "vitest";
import worker from "../worker/index";
import { SESSION_COOKIE, signSession } from "../worker/session";
import wranglerToml from "../wrangler.toml?raw";
import { testEnv } from "./helpers";

const ORIGIN = "https://admin.test";

/// What the fake cloud Worker was asked, and what it answers next.
let seen: { url: string; method: string; headers: Headers; body: string }[] = [];
let answer: () => Response | Promise<Response> = () => Response.json({ ok: true });

function fakeCloud(): Fetcher {
  return {
    fetch: async (input: RequestInfo | URL, init?: RequestInit) => {
      const req = new Request(input, init);
      seen.push({ url: req.url, method: req.method, headers: req.headers, body: await req.text() });
      return answer();
    },
  } as unknown as Fetcher;
}

const pageFetcher = { fetch: async () => new Response("<html></html>", { headers: { "Content-Type": "text/html" } }) } as unknown as Fetcher;
const env: Env = { ...testEnv, CLOUD: fakeCloud(), ASSETS: pageFetcher, CLOUD_ORIGIN: "https://cloud.test" };

beforeEach(() => {
  seen = [];
  answer = () => Response.json({ ok: true });
});

async function signedIn(): Promise<string> {
  const value = await signSession({ githubId: 1001, login: "octocat" }, "test-session-secret", Math.floor(Date.now() / 1000));
  return `${SESSION_COOKIE}=${value}`;
}

async function api(path: string, init: RequestInit = {}, e: Env = env, withSession = true): Promise<Response> {
  const headers = new Headers(init.headers);
  if (withSession) headers.set("Cookie", [await signedIn(), headers.get("Cookie")].filter(Boolean).join("; "));
  return worker.fetch(new Request(`${ORIGIN}${path}`, { ...init, headers }), e);
}

const change = { method: "POST", headers: { Origin: ORIGIN, "Content-Type": "application/json" }, body: "{}" };

describe("forwarding", () => {
  it("never forwards the page to the operators' authenticator keys", async () => {
    for (const path of ["/api/operators/1001/totp", "/api/operators/1002/totp/use", "/api/OPERATORS/1001/totp", "/api/operators"]) {
      for (const method of ["GET", "PUT", "POST", "DELETE"]) {
        const res = await api(path, method === "GET" ? {} : { ...change, method });
        expect(res.status, `${method} ${path}`).toBe(404);
      }
    }
    expect(seen).toEqual([]);
  });

  it("forwards a signed-in operator's call with the secret and their name", async () => {
    const res = await api("/api/users?q=octo");
    expect(res.status).toBe(200);
    expect(seen).toHaveLength(1);
    expect(seen[0]!.url).toBe("https://cloud.test/admin/users?q=octo");
    expect(seen[0]!.method).toBe("GET");
    expect(seen[0]!.headers.get("Authorization")).toBe("Bearer test-secret");
    expect(seen[0]!.headers.get("X-Arakun-Operator-Id")).toBe("1001");
    expect(seen[0]!.headers.get("X-Arakun-Operator-Login")).toBe("octocat");
  });

  it("does not pass on the cookie or the client's headers", async () => {
    await api("/api/users", {
      headers: {
        Cookie: "other=1",
        "X-Arakun-Operator-Id": "9",
        "X-Arakun-Operator-Login": "evil",
        Authorization: "Bearer x",
        "X-Custom": "y",
      },
    });
    const h = seen[0]!.headers;
    expect(h.get("Cookie")).toBeNull();
    expect(h.get("X-Custom")).toBeNull();
    expect(h.get("Authorization")).toBe("Bearer test-secret");
    expect(h.get("X-Arakun-Operator-Id")).toBe("1001");
    expect(h.get("X-Arakun-Operator-Login")).toBe("octocat");
  });

  it("puts the operator's login in by, whatever the body said", async () => {
    for (const action of ["grant", "decline"]) {
      seen = [];
      const res = await api(`/api/cloud/requests/7/${action}`, { ...change, method: "PUT", body: JSON.stringify({ by: "someone" }) });
      expect(res.status).toBe(200);
      expect(seen[0]!.url).toBe(`https://cloud.test/admin/cloud/requests/7/${action}`);
      expect(JSON.parse(seen[0]!.body)).toEqual({ by: "octocat" });
    }
  });

  it("passes another change's body on as it is", async () => {
    await api("/api/users/1001/flags", { ...change, method: "PUT", body: '{"a":1}' });
    expect(seen[0]!.body).toBe('{"a":1}');
    expect(seen[0]!.headers.get("Content-Type")).toBe("application/json");
  });

  it("answers 401 without a session and asks the cloud nothing", async () => {
    const res = await api("/api/users", {}, env, false);
    expect(res.status).toBe(401);
    expect(await res.json()).toEqual({ error: "not signed in" });
    const post = await api("/api/users", change, env, false);
    expect(post.status).toBe(401);
    expect(seen).toEqual([]);
  });

  it("refuses a change from another origin, and one without Origin", async () => {
    for (const headers of [{ Origin: "https://evil.test" }, {} as Record<string, string>]) {
      const res = await api("/api/users/1001/flags", { method: "PUT", headers, body: "{}" });
      expect(res.status).toBe(403);
      expect(await res.json()).toEqual({ error: "the request comes from another origin" });
    }
    expect(seen).toEqual([]);
    expect((await api("/api/users")).status).toBe(200);
  });

  it("refuses a body over 4096 bytes", async () => {
    const res = await api("/api/users/1001/flags", { ...change, method: "PUT", body: "x".repeat(4097) });
    expect(res.status).toBe(413);
    expect(seen).toEqual([]);
    const ok = await api("/api/users/1001/flags", { ...change, method: "PUT", body: "x".repeat(4096) });
    expect(ok.status).toBe(200);
  });

  it("refuses a body over 4096 bytes that has no Content-Length", async () => {
    const stream = new ReadableStream<Uint8Array>({
      start(c) {
        c.enqueue(new Uint8Array(4097));
        c.close();
      },
    });
    const res = await api("/api/users/1001/flags", { ...change, method: "PUT", body: stream, duplex: "half" } as RequestInit);
    expect(res.status).toBe(413);
    expect(seen).toEqual([]);
  });

  it("answers /api/me itself, for GET only", async () => {
    for (const method of ["POST", "PUT", "DELETE"]) {
      const res = await api("/api/me", { method, headers: { Origin: ORIGIN } });
      expect(res.status).toBe(405);
    }
    expect(seen).toEqual([]);
  });

  it("marks its own JSON answers no-store and nosniff", async () => {
    for (const res of [await api("/api/me"), await api("/api/users", {}, env, false), await api("/api/users", {}, { ...env, ADMIN_SECRET: undefined })]) {
      expect(res.headers.get("Cache-Control")).toBe("no-store");
      expect(res.headers.get("X-Content-Type-Options")).toBe("nosniff");
    }
  });

  it("refuses a path that leaves /admin/", async () => {
    for (const path of ["/api/", "/api/users//1", "/api/users%2F1"]) {
      const res = await api(path);
      expect(res.status, path).toBe(404);
    }
    // The URL parser resolves a dot segment, encoded or not, before the Worker sees it,
    // so that one lands on the page, never on the cloud.
    for (const path of ["/api/users/../../x", "/api/%2E%2e/x"]) expect((await api(path)).status, path).toBe(200);
    expect(seen).toEqual([]);
  });

  it("hands back the cloud's status and error text", async () => {
    answer = () => Response.json({ error: "no such user" }, { status: 404 });
    const res = await api("/api/users/5");
    expect(res.status).toBe(404);
    expect(await res.json()).toEqual({ error: "no such user" });
    expect(res.headers.get("Content-Type")).toContain("application/json");
    expect(res.headers.get("Cache-Control")).toBe("no-store");
  });

  it("answers 502 when the cloud does not answer", async () => {
    answer = () => {
      throw new TypeError("Network connection lost.");
    };
    const quiet = vi.spyOn(console, "error").mockImplementation(() => {});
    const res = await api("/api/users");
    expect(res.status).toBe(502);
    expect(await res.json()).toEqual({ error: "the cloud did not answer" });
    quiet.mockRestore();
  });

  it("answers 503 without the admin secret", async () => {
    const res = await api("/api/users", {}, { ...env, ADMIN_SECRET: undefined });
    expect(res.status).toBe(503);
    expect(seen).toEqual([]);
  });

  it("never sends the secret to the browser", async () => {
    for (const res of [await api("/api/users"), await api("/api/me"), await api("/api/users", {}, env, false)]) {
      expect([...res.headers].join("\n")).not.toContain("test-secret");
      expect(await res.text()).not.toContain("test-secret");
    }
  });

  it("answers /api/me itself", async () => {
    const res = await api("/api/me");
    expect(await res.json()).toEqual({ github_id: 1001, login: "octocat" });
    expect(seen).toEqual([]);
  });

  it("answers an empty answer with no body, even if the binding hands back a stream", async () => {
    // The Response constructor refuses a body with a 204, so the fake gives the
    // proxy a 204 whose body getter is a stream, as a binding may.
    let cancelled = false;
    answer = () => {
      const res = new Response(null, { status: 204, headers: { "Content-Type": "application/json" } });
      const body = new ReadableStream({ cancel: () => void (cancelled = true) });
      Object.defineProperty(res, "body", { value: body });
      return res;
    };
    const res = await api("/api/users/2002/block", { method: "DELETE", headers: { Origin: ORIGIN } });
    expect(res.status).toBe(204);
    expect(res.body).toBeNull();
    expect(cancelled).toBe(true);
  });
});

describe("pages", () => {
  // The asset routing runs before the Worker unless every request is sent to
  // it, and a test that calls worker.fetch cannot see that.
  it("sends every request to the Worker first, in both environments", () => {
    const blocks = wranglerToml.split(/^\[/m).filter((b) => /^(env\.production\.)?assets\]/.test(b));
    expect(blocks).toHaveLength(2);
    for (const block of blocks) expect(block).toMatch(/^run_worker_first = true$/m);
  });

  it("serves the page with its policy, also for a path of the single-page app", async () => {
    for (const path of ["/", "/users/1001"]) {
      const res = await api(path, {}, env, false);
      expect(res.status).toBe(200);
      expect(res.headers.get("Content-Security-Policy")).toBe(
        "default-src 'self'; script-src 'self'; connect-src 'self'; img-src 'self' https://avatars.githubusercontent.com; style-src 'self' 'unsafe-inline'; font-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
      );
      expect(res.headers.get("X-Content-Type-Options")).toBe("nosniff");
      expect(res.headers.get("Referrer-Policy")).toBe("no-referrer");
      expect(await res.text()).toBe("<html></html>");
    }
    expect(seen).toEqual([]);
  });
});
