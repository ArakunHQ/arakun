import { act, renderHook } from "@testing-library/react";
import { createElement, type ReactNode } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { RelayHandlers } from "./globals";
import { createLink, type Link, LinkContext, type LinkEvent, LOCAL_OPEN_MS, LOCAL_PING_MS, LOCAL_SILENT_MS, useLink } from "./link";
import { TOKEN_KEY } from "./token";

// A socket that does only what the test says: open, a message, close.
class FakeSocket extends EventTarget {
  static OPEN = 1;
  static all: FakeSocket[] = [];
  readyState = 0;
  sent: any[] = [];
  closed = false;
  constructor(readonly url: string) {
    super();
    FakeSocket.all.push(this);
  }
  send(data: string) {
    this.sent.push(JSON.parse(data));
  }
  close() {
    if (this.closed) return;
    this.closed = true;
    this.readyState = 3;
    this.dispatchEvent(new Event("close"));
  }
  open() {
    this.readyState = 1;
    this.dispatchEvent(new Event("open"));
  }
  say(msg: object) {
    this.dispatchEvent(new MessageEvent("message", { data: JSON.stringify(msg) }));
  }
}

const last = () => FakeSocket.all[FakeSocket.all.length - 1];
const flush = () => vi.advanceTimersByTimeAsync(0);

function setVisibility(v: "visible" | "hidden") {
  Object.defineProperty(document, "visibilityState", { configurable: true, get: () => v });
  document.dispatchEvent(new Event("visibilitychange"));
}

let fetchMe: ReturnType<typeof vi.fn>;
let link: Link | null = null;

function local(): Link {
  link = createLink({ mode: "local", WebSocketImpl: FakeSocket as any, fetchImpl: fetchMe as any });
  return link;
}

// Opens a local link up to an open socket.
async function linkedLocal(): Promise<{ link: Link; ws: FakeSocket }> {
  const l = local();
  await flush();
  const ws = last();
  ws.open();
  return { link: l, ws };
}

beforeEach(() => {
  vi.useFakeTimers();
  FakeSocket.all = [];
  localStorage.setItem(TOKEN_KEY, "tok");
  fetchMe = vi.fn(async () => new Response(JSON.stringify({ id: "p1", host: "mbp", push_key: "k" }), { status: 200 }));
  setVisibility("visible");
});

afterEach(() => {
  link?.close();
  link = null;
  localStorage.clear();
  delete (globalThis as any).RELAY;
  vi.useRealTimers();
});

describe("local link", () => {
  it("asks /me with the token, then opens the socket", async () => {
    const { link: l, ws } = await linkedLocal();
    expect(fetchMe).toHaveBeenCalledWith("/me", expect.objectContaining({ headers: { Authorization: "Bearer tok" } }));
    expect(ws.url).toMatch(/\/ws\?t=tok$/);
    expect(l.getState()).toMatchObject({ connected: true, host: "mbp", pushKey: "k" });
  });

  it("pings, and with no word for 40 s closes the socket and reconnects", async () => {
    const { link: l, ws } = await linkedLocal();
    await vi.advanceTimersByTimeAsync(LOCAL_PING_MS);
    expect(ws.sent).toContainEqual({ type: "ping" });
    await vi.advanceTimersByTimeAsync(LOCAL_SILENT_MS);
    expect(ws.closed).toBe(true);
    expect(l.getState().connected).toBe(false);
    await vi.advanceTimersByTimeAsync(500);
    expect(FakeSocket.all.length).toBe(2);
  });

  it("a pong keeps it open", async () => {
    const { ws } = await linkedLocal();
    for (let i = 0; i < 5; i++) {
      await vi.advanceTimersByTimeAsync(LOCAL_PING_MS);
      ws.say({ type: "pong" });
    }
    expect(ws.closed).toBe(false);
  });

  it("gives up on a socket that does not open in time", async () => {
    local();
    await flush();
    await vi.advanceTimersByTimeAsync(LOCAL_OPEN_MS);
    expect(last().closed).toBe(true);
    await vi.advanceTimersByTimeAsync(500);
    expect(FakeSocket.all.length).toBe(2);
  });

  it("waits longer after each failure", async () => {
    fetchMe.mockRejectedValue(new Error("offline"));
    local();
    await flush();
    expect(fetchMe).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(500);
    expect(fetchMe).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(500);
    expect(fetchMe).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(500);
    expect(fetchMe).toHaveBeenCalledTimes(3);
  });

  it("a refused token is forgotten and says so", async () => {
    fetchMe.mockResolvedValue(new Response("", { status: 403 }));
    const events: LinkEvent[] = [];
    local().subscribeEvents((e) => events.push(e));
    await flush();
    expect(events).toContainEqual({ type: "unpaired", message: "This phone is no longer paired. Pair it again from Arakun." });
    expect(localStorage.getItem(TOKEN_KEY)).toBeNull();
    expect(FakeSocket.all.length).toBe(0);
  });

  it("with no token it asks to pair", async () => {
    localStorage.clear();
    const events: LinkEvent[] = [];
    local().subscribeEvents((e) => events.push(e));
    await flush();
    expect(events).toContainEqual({ type: "unpaired", message: "" });
    expect(fetchMe).not.toHaveBeenCalled();
  });

  it("fetch carries the token", async () => {
    const l = local();
    await l.fetch("/phone", { method: "POST" });
    expect(fetchMe).toHaveBeenCalledWith("/phone", { method: "POST", headers: { Authorization: "Bearer tok" } });
  });
});

describe("visibility", () => {
  it("visible tells the computer and goes now when a retry waits", async () => {
    const { ws } = await linkedLocal();
    setVisibility("visible");
    expect(ws.sent).toContainEqual({ type: "visible", visible: true });

    fetchMe.mockRejectedValue(new Error("offline"));
    ws.close();
    await flush();
    const calls = fetchMe.mock.calls.length;
    setVisibility("visible");
    await flush();
    expect(fetchMe.mock.calls.length).toBe(calls + 1);
  });

  it("hidden tells the computer", async () => {
    const { ws } = await linkedLocal();
    setVisibility("hidden");
    expect(ws.sent).toContainEqual({ type: "visible", visible: false });
  });

  it("a link made while hidden says so once it is up", async () => {
    setVisibility("hidden");
    const { ws } = await linkedLocal();
    expect(ws.sent).toContainEqual({ type: "visible", visible: false });
  });
});

describe("messages", () => {
  beforeEach(() => {
    (globalThis as any).RELAY = {
      noticeOutcome: (pending: string, msg: { type?: string }) => (pending && msg.type === "error" ? "elsewhere" : "unrelated"),
      noticeElsewhere: () => "That notification is from another computer.",
    };
  });

  it("reduces into the state and tells subscribers", async () => {
    const { link: l, ws } = await linkedLocal();
    const seen = vi.fn();
    l.subscribe(seen);
    ws.say({ type: "inbox", host: "mbp", sessions: [{ id: "s1", title: "T" }], places: [] });
    expect(seen).toHaveBeenCalled();
    expect(l.getState().inbox).toEqual([{ id: "s1", title: "T" }]);
  });

  it("open sends and starts the session; linked opens it again", async () => {
    const { link: l, ws } = await linkedLocal();
    l.send({ type: "open", session: "s1" });
    expect(ws.sent).toContainEqual({ type: "open", session: "s1" });
    expect(l.getState().session).toMatchObject({ id: "s1", entries: [] });
    ws.say({ type: "entries", id: "s1", from: 0, count: 1, entries: [{ n: 0 }] });
    ws.close();
    await vi.advanceTimersByTimeAsync(500);
    last().open();
    expect(last().sent).toContainEqual({ type: "open", session: "s1" });
    expect(l.getState().session!.entries).toEqual([{ n: 0 }]);
  });

  it("a session from a notification that this computer lacks is dropped", async () => {
    const { link: l, ws } = await linkedLocal();
    const events: LinkEvent[] = [];
    l.subscribeEvents((e) => events.push(e));
    l.send({ type: "open", session: "s9" });
    l.watchNotice("s9");
    ws.say({ type: "error", message: "no such session" });
    expect(l.getState().session).toBeNull();
    expect(events).toEqual([
      { type: "session-elsewhere", id: "s9" },
      { type: "toast", text: "That notification is from another computer." },
    ]);
  });

  it("an error is a toast", async () => {
    const { link: l, ws } = await linkedLocal();
    const events: LinkEvent[] = [];
    l.subscribeEvents((e) => events.push(e));
    ws.say({ type: "error", message: "nope" });
    expect(events).toEqual([
      { type: "error", message: "nope" },
      { type: "toast", text: "nope" },
    ]);
  });

  it("started opens the session and says so", async () => {
    const { link: l, ws } = await linkedLocal();
    const events: LinkEvent[] = [];
    l.subscribeEvents((e) => events.push(e));
    ws.say({ type: "started", id: "s5" });
    expect(l.getState().session).toMatchObject({ id: "s5" });
    expect(ws.sent).toContainEqual({ type: "open", session: "s5" });
    expect(events).toContainEqual({ type: "started", id: "s5" });
  });
});

describe("relay link", () => {
  let handlers: RelayHandlers;
  let relayLink: { send: any; fetch: any; close: any; ready: boolean };
  const machine = { label: "abcd", name: "mbp", pub: "key" };

  beforeEach(() => {
    relayLink = {
      send: vi.fn(),
      fetch: vi.fn(async () => new Response(JSON.stringify({ id: "p1", host: "mbp" }), { status: 200 })),
      close: vi.fn(),
      ready: true,
    };
    (globalThis as any).RELAY = {
      open: vi.fn((_m: unknown, h: RelayHandlers) => {
        handlers = h;
        return relayLink;
      }),
      noticeOutcome: () => "unrelated",
    };
  });

  it("opens through the relay and asks /me inside it", async () => {
    link = createLink({ mode: "relay", machine });
    await flush();
    expect((globalThis as any).RELAY.open).toHaveBeenCalledWith(machine, expect.anything());
    handlers.onopen({});
    await flush();
    expect(relayLink.fetch).toHaveBeenCalledWith("/me", {});
    expect(link.getState()).toMatchObject({ connected: true, host: "mbp" });
    link.send({ type: "usage" });
    expect(relayLink.send).toHaveBeenCalledWith({ type: "usage" });
  });

  it("an offline computer is marked, and a closed link tries again", async () => {
    link = createLink({ mode: "relay", machine });
    await flush();
    handlers.onstatus!({ pc: false, since: 5_000 });
    expect(link.getState()).toMatchObject({ connected: false, pcOffline: 5 });
    handlers.onclose!({ kind: "gone", detail: "" });
    await vi.advanceTimersByTimeAsync(500);
    expect((globalThis as any).RELAY.open).toHaveBeenCalledTimes(2);
  });

  it("a link the relay refuses is not tried again", async () => {
    link = createLink({ mode: "relay", machine });
    const events: LinkEvent[] = [];
    link.subscribeEvents((e) => events.push(e));
    await flush();
    handlers.onclose!({ kind: "signed_out", detail: "" });
    await vi.advanceTimersByTimeAsync(10_000);
    expect(events).toContainEqual({ type: "closed", kind: "signed_out", detail: "" });
    expect((globalThis as any).RELAY.open).toHaveBeenCalledTimes(1);
  });

  it("a busy relay is a toast and a retry", async () => {
    link = createLink({ mode: "relay", machine });
    const events: LinkEvent[] = [];
    link.subscribeEvents((e) => events.push(e));
    await flush();
    handlers.onclose!({ kind: "limit", detail: "sockets" });
    expect(events).toContainEqual({ type: "toast", text: "The relay is busy (sockets). Trying again in a moment." });
    await vi.advanceTimersByTimeAsync(500);
    expect((globalThis as any).RELAY.open).toHaveBeenCalledTimes(2);
  });
});

describe("useLink", () => {
  it("follows the part of the state it selects", async () => {
    const { link: l, ws } = await linkedLocal();
    const wrapper = ({ children }: { children: ReactNode }) => createElement(LinkContext.Provider, { value: l }, children);
    const { result } = renderHook(() => useLink((s) => s.inbox), { wrapper });
    expect(result.current).toEqual([]);
    act(() => ws.say({ type: "inbox", host: "mbp", sessions: [{ id: "s1" }] }));
    expect(result.current).toEqual([{ id: "s1" }]);
  });
});

describe("retryNow", () => {
  it("drops the socket and starts over at once", async () => {
    const { link: l, ws } = await linkedLocal();
    l.retryNow();
    expect(ws.closed).toBe(true);
    expect(l.getState().connected).toBe(false);
    await flush();
    expect(FakeSocket.all.length).toBe(2);
  });
});
