import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ApiError, api, setOnSignedOut } from "./api";

const fetchMock = vi.fn<typeof fetch>();

beforeEach(() => {
  fetchMock.mockReset();
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  setOnSignedOut(() => {});
  vi.unstubAllGlobals();
});

function answer(status: number, body?: unknown): Response {
  if (body === undefined) return new Response(null, { status });
  return new Response(typeof body === "string" ? body : JSON.stringify(body), { status });
}

async function failure(call: Promise<unknown>): Promise<ApiError> {
  try {
    await call;
  } catch (e) {
    if (e instanceof ApiError) return e;
    throw e;
  }
  throw new Error("the call did not fail");
}

describe("api", () => {
  it("sends JSON with the same-origin cookie", async () => {
    fetchMock.mockResolvedValue(answer(200, { account_id: 1001, flags: ["cloud"] }));
    const out = await api<{ flags: string[] }>("PUT", "users/1001/plan", { plan: "pro" });
    expect(out).toEqual({ account_id: 1001, flags: ["cloud"] });
    expect(fetchMock).toHaveBeenCalledOnce();
    const [url, init] = fetchMock.mock.calls[0]!;
    expect(url).toBe("/api/users/1001/plan");
    expect(init?.method).toBe("PUT");
    expect(init?.credentials).toBe("same-origin");
    expect(new Headers(init?.headers).get("Content-Type")).toBe("application/json");
    expect(init?.body).toBe('{"plan":"pro"}');
  });

  it("sends no body and no content type for a GET", async () => {
    fetchMock.mockResolvedValue(answer(200, []));
    await api("GET", "machines");
    const [url, init] = fetchMock.mock.calls[0]!;
    expect(url).toBe("/api/machines");
    expect(init?.body).toBeUndefined();
    expect(new Headers(init?.headers).has("Content-Type")).toBe(false);
  });

  it("throws the server's error text", async () => {
    fetchMock.mockResolvedValue(answer(409, { error: "the request is granted, not pending" }));
    const e = await failure(api("PUT", "cloud/requests/1001/decline"));
    expect(e.status).toBe(409);
    expect(e.message).toBe("the request is granted, not pending");
  });

  it("falls back to the status when the body is not JSON", async () => {
    fetchMock.mockResolvedValue(answer(500, "<html>Internal Server Error</html>"));
    const e = await failure(api("GET", "users"));
    expect(e.status).toBe(500);
    expect(e.message).toBe("the server answered 500");
  });

  it("calls onSignedOut on a 401 and still throws", async () => {
    const signedOut = vi.fn();
    setOnSignedOut(signedOut);
    fetchMock.mockResolvedValue(answer(401, { error: "not signed in" }));
    const e = await failure(api("PUT", "cloud/requests/1001/grant"));
    expect(signedOut).toHaveBeenCalledOnce();
    expect(e.status).toBe(401);
    expect(e.message).toBe("not signed in");
  });

  it("keeps the operator signed in on the cloud Worker's own 401", async () => {
    const signedOut = vi.fn();
    setOnSignedOut(signedOut);
    fetchMock.mockResolvedValue(answer(401, { error: "unauthorized" }));
    const e = await failure(api("GET", "users"));
    expect(signedOut).not.toHaveBeenCalled();
    expect(e.status).toBe(401);
    expect(e.message).toBe("unauthorized");
  });

  it("keeps the operator signed in on a 401 that is not JSON", async () => {
    const signedOut = vi.fn();
    setOnSignedOut(signedOut);
    fetchMock.mockResolvedValue(answer(401, "<html>no</html>"));
    const e = await failure(api("GET", "users"));
    expect(signedOut).not.toHaveBeenCalled();
    expect(e.message).toBe("the server answered 401");
  });

  it("throws an ApiError for a 2xx answer that is not JSON", async () => {
    fetchMock.mockResolvedValue(answer(200, "<html>captive portal</html>"));
    const e = await failure(api("GET", "users"));
    expect(e.status).toBe(200);
    expect(e.message).toBe("the server sent an answer the panel could not read");
  });

  it("does not call onSignedOut for other errors", async () => {
    const signedOut = vi.fn();
    setOnSignedOut(signedOut);
    fetchMock.mockResolvedValue(answer(403, { error: "the request comes from another origin" }));
    await failure(api("DELETE", "notices/abc"));
    expect(signedOut).not.toHaveBeenCalled();
  });

  it('shows "the cloud did not answer" from a 502', async () => {
    fetchMock.mockResolvedValue(answer(502, { error: "the cloud did not answer" }));
    const e = await failure(api("GET", "cloud/requests?status=pending"));
    expect(e.status).toBe(502);
    expect(e.message).toBe("the cloud did not answer");
  });

  it("answers undefined for a 204", async () => {
    fetchMock.mockResolvedValue(answer(204));
    await expect(api("DELETE", "users/1001/block")).resolves.toBeUndefined();
  });
});
