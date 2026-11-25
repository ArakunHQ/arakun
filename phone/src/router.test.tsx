import { QueryClient } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createAppRouter, onLinkEvent } from "./router";
import { useSaid } from "./lib/account";
import { noteNoticeOpen, openSession, pairMessage, resetStart } from "./lib/start";
import { renderHook } from "@testing-library/react";
import type { Link } from "./lib/link";
import { TOKEN_KEY } from "./lib/token";

// The page's scripts, faked: which way in this is, and what the account says.
function fakeRelay(over: Record<string, unknown> = {}) {
  (globalThis as any).RELAY = {
    forgetOldPairing() {},
    detect: async () => null,
    boot: async () => ({ screen: "signin" }),
    machines: async () => ({ list: [] }),
    open: vi.fn(() => ({ send() {}, close() {}, fetch: () => new Promise(() => {}), ready: false })),
    unreachable: () => "Could not reach h.",
    tapSentence: () => "Pick the computer that notification came from to open its session.",
    forgetAccountState() {},
    ...over,
  };
}

/** Where the page lands, starting at `hash`. */
async function land(hash: string): Promise<string> {
  history.replaceState(null, "", `/${hash}`);
  const router = createAppRouter(new QueryClient({ defaultOptions: { queries: { retry: false } } }));
  await router.load();
  return router.state.location.pathname;
}

const machine = (label: string) => ({ label, pub: "AAAA", name: `pc-${label}` });

beforeEach(() => {
  localStorage.clear();
  // A computer that never answers: the link waits, nothing fails.
  vi.stubGlobal("fetch", () => new Promise(() => {}));
});

afterEach(() => {
  resetStart();
  vi.unstubAllGlobals();
  delete (globalThis as any).RELAY;
});

describe("where the page lands", () => {
  it("takes the old session link #s=abc to /s/abc", async () => {
    fakeRelay();
    localStorage.setItem(TOKEN_KEY, "tok");
    expect(await land("#s=abc")).toBe("/s/abc");
  });

  it("asks for a pairing code on the computer's own server without a token", async () => {
    fakeRelay();
    expect(await land("#/inbox")).toBe("/pair");
  });

  it("opens the inbox on the computer's own server with a token", async () => {
    fakeRelay();
    localStorage.setItem(TOKEN_KEY, "tok");
    expect(await land("")).toBe("/inbox");
  });

  it("asks to sign in on the account host when signed out", async () => {
    fakeRelay({ detect: async () => "account" });
    expect(await land("#/inbox")).toBe("/signin");
  });

  it("says the address moved on an old relay host", async () => {
    fakeRelay({ detect: async () => "update" });
    expect(await land("")).toBe("/moved");
  });

  it("says it cannot connect when the account host failed", async () => {
    fakeRelay({ detect: async () => "unreachable" });
    expect(await land("#/inbox")).toBe("/fatal");
  });

  it("connects to the only computer of the account", async () => {
    const pc = machine("aaaaaaaaaaaaaaaa");
    const open = vi.fn(() => ({ send() {}, close() {}, fetch: () => new Promise(() => {}), ready: false }));
    fakeRelay({ detect: async () => "account", boot: async () => ({ screen: "connect", me: { login: "rik" }, machines: [pc], machine: pc }), open });
    expect(await land("")).toBe("/inbox");
    expect(open).toHaveBeenCalledWith(pc, expect.anything());
  });

  it("lists the computers when there are several", async () => {
    const list = [machine("aaaaaaaaaaaaaaaa"), machine("bbbbbbbbbbbbbbbb")];
    fakeRelay({ detect: async () => "account", boot: async () => ({ screen: "list", me: { login: "rik" }, machines: list, machine: null }) });
    expect(await land("#/inbox")).toBe("/computers");
  });

  it("takes a session link to the computer opened last", async () => {
    const list = [machine("aaaaaaaaaaaaaaaa"), machine("bbbbbbbbbbbbbbbb")];
    localStorage.setItem("arakun.machine", "bbbbbbbbbbbbbbbb");
    const open = vi.fn(() => ({ send() {}, close() {}, fetch: () => new Promise(() => {}), ready: false }));
    fakeRelay({ detect: async () => "account", boot: async () => ({ screen: "list", me: { login: "rik" }, machines: list, machine: null }), open });
    expect(await land("#s=abc")).toBe("/s/abc");
    expect(open).toHaveBeenCalledWith(list[1], expect.anything());
  });

  it("lists the computers for a session link with no computer to take it to", async () => {
    const list = [machine("aaaaaaaaaaaaaaaa"), machine("bbbbbbbbbbbbbbbb")];
    fakeRelay({ detect: async () => "account", boot: async () => ({ screen: "list", me: { login: "rik" }, machines: list, machine: null }) });
    expect(await land("#/s/abc")).toBe("/computers");
  });
});

describe("what the link says", () => {
  it("goes back to the list with the relay's sentence on a refusal", () => {
    fakeRelay();
    const go = vi.fn();
    onLinkEvent({ type: "closed", kind: "refused", detail: "That computer removed this phone." }, new QueryClient(), go);
    expect(go).toHaveBeenCalledWith("/computers");
    expect(renderHook(() => useSaid()).result.current).toBe("That computer removed this phone.");
  });

  it("shows sign-in when the relay says the session is gone", () => {
    fakeRelay();
    const qc = new QueryClient();
    const go = vi.fn();
    onLinkEvent({ type: "closed", kind: "signed_out", detail: "" }, qc, go);
    expect(go).toHaveBeenCalledWith("/signin");
    expect(qc.getQueryData(["me"])).toEqual({ screen: "signin" });
  });

  it("asks for a code again when the computer let go of this phone", () => {
    fakeRelay();
    const go = vi.fn();
    onLinkEvent({ type: "unpaired", message: "This phone is no longer paired." }, new QueryClient(), go);
    expect(go).toHaveBeenCalledWith("/pair");
    expect(pairMessage()).toBe("This phone is no longer paired.");
  });

  it("opens a session the New session form started, in place of the form", () => {
    const go = vi.fn();
    onLinkEvent({ type: "started", id: "a b" }, new QueryClient(), go);
    expect(go).toHaveBeenCalledWith("/s/a%20b", { replace: true });
  });
});

describe("a session opened from a notification", () => {
  const fakeLink = () => {
    const calls: string[] = [];
    const link = { send: (m: { type: string }) => calls.push(m.type), watchNotice: () => calls.push("watch") } as unknown as Link;
    return { link, calls };
  };

  it("is watched after it is opened, with several computers", () => {
    const { link, calls } = fakeLink();
    noteNoticeOpen("abc", 2);
    openSession(link, "abc");
    expect(calls).toEqual(["open", "watch"]);
  });

  it("is not watched with one computer", () => {
    const { link, calls } = fakeLink();
    noteNoticeOpen("abc", 1);
    openSession(link, "abc");
    expect(calls).toEqual(["open"]);
  });
});
