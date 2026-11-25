import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import * as api from "./api";
import { notifyState, subscribePush } from "./push";

// "AQID" is the bytes 1, 2, 3 in base64url.
const KEY = "AQID";

function fakePush(existing: unknown = null) {
  const sub = { toJSON: () => ({ endpoint: "https://push.example/1" }) };
  const pushManager = { getSubscription: vi.fn(async () => existing), subscribe: vi.fn(async (_o: PushSubscriptionOptionsInit) => sub) };
  vi.stubGlobal("navigator", { ...navigator, serviceWorker: { ready: Promise.resolve({ pushManager }) } });
  return pushManager;
}

beforeEach(() => {
  vi.spyOn(api, "api").mockResolvedValue(new Response("{}", { status: 200 }));
});
afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("subscribePush", () => {
  it("subscribes with the computer's key, then posts the subscription to /push", async () => {
    const pm = fakePush();
    await subscribePush(KEY);
    const opts = pm.subscribe.mock.calls[0][0];
    expect(opts.userVisibleOnly).toBe(true);
    expect(Array.from(opts.applicationServerKey as Uint8Array)).toEqual([1, 2, 3]);
    const [path, init] = vi.mocked(api.api).mock.calls[0];
    expect(path).toBe("/push");
    expect(init?.method).toBe("POST");
    expect(JSON.parse(String(init?.body))).toEqual({ subscription: { endpoint: "https://push.example/1" } });
    expect(pm.subscribe.mock.invocationCallOrder[0]).toBeLessThan(vi.mocked(api.api).mock.invocationCallOrder[0]);
  });

  it("drops a subscription made with another computer's key first", async () => {
    const old = { options: { applicationServerKey: new Uint8Array([9]) }, unsubscribe: vi.fn(async () => true) };
    fakePush(old);
    await subscribePush(KEY);
    expect(old.unsubscribe).toHaveBeenCalled();
  });

  it("fails when the computer does not take it", async () => {
    fakePush();
    vi.mocked(api.api).mockResolvedValue(new Response("no", { status: 500 }));
    await expect(subscribePush(KEY)).rejects.toThrow();
  });

  it("fails without a key", async () => {
    fakePush();
    await expect(subscribePush("")).rejects.toThrow();
  });
});

describe("notifyState", () => {
  it("is unsupported where the browser has no push", () => {
    expect(notifyState()).toBe("unsupported");
  });

  it("follows the permission where it has", () => {
    vi.stubGlobal("isSecureContext", true);
    vi.stubGlobal("PushManager", class {});
    vi.stubGlobal("Notification", { permission: "default" });
    expect(notifyState()).toBe("off");
    vi.stubGlobal("Notification", { permission: "granted" });
    expect(notifyState()).toBe("on");
    vi.stubGlobal("Notification", { permission: "denied" });
    expect(notifyState()).toBe("denied");
  });
});
