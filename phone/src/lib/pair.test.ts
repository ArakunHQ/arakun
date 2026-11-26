import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { pair } from "./pair";
import type { SealedHandlers, SealedOptions } from "./sealed";
import { LABEL_KEY, TOKEN_KEY } from "./token";

// The sealed socket, faked: what pairing asked for, and the handlers to answer it.
const opened: { opts?: SealedOptions; handlers?: SealedHandlers; closed: boolean } = { closed: false };
vi.mock("./sealed", () => ({
  openSealed: (opts: SealedOptions, handlers: SealedHandlers) => {
    opened.opts = opts;
    opened.handlers = handlers;
    return { close: () => (opened.closed = true), send() {}, fetch: () => Promise.reject(), ready: false };
  },
}));

beforeEach(() => {
  opened.opts = undefined;
  opened.closed = false;
});
afterEach(() => localStorage.clear());

describe("pairing", () => {
  it("brings the code inside the sealed socket, pinned to the link's key, and keeps the token and key", async () => {
    const done = pair("123456", "bbbbbbbbbbbbbbbb");
    expect(opened.opts!.who).toMatchObject({ code: "123456" });
    expect(opened.opts!.label).toBe("bbbbbbbbbbbbbbbb");
    opened.handlers!.onopen({ label: "bbbbbbbbbbbbbbbb", paired: { token: "tok", host: "mbp", id: "d1" } });
    expect(await done).toEqual({ ok: true, host: "mbp" });
    expect(opened.closed).toBe(true);
    expect(localStorage.getItem(TOKEN_KEY)).toBe("tok");
    expect(localStorage.getItem(LABEL_KEY)).toBe("bbbbbbbbbbbbbbbb");
  });

  it("a typed code takes the key the first handshake shows", async () => {
    const done = pair("123456");
    expect(opened.opts!.label).toBe("");
    opened.handlers!.onopen({ label: "cccccccccccccccc", paired: { token: "tok", host: "", id: "d1" } });
    await done;
    expect(localStorage.getItem(LABEL_KEY)).toBe("cccccccccccccccc");
  });

  it("says why it did not work, and keeps nothing", async () => {
    const wrong = pair("000000");
    opened.handlers!.onclose({ kind: "refused", detail: "bad_code" });
    expect(await wrong).toEqual({ ok: false, error: "That code is wrong or has expired. Make a new one in Arakun." });
    const other = pair("123456", "bbbbbbbbbbbbbbbb");
    opened.handlers!.onclose({ kind: "untrusted", detail: "bad hello: label" });
    expect(await other).toMatchObject({ ok: false, error: expect.stringContaining("not the one that made the code") });
    const away = pair("123456");
    opened.handlers!.onclose({ kind: "gone", detail: "closed" });
    expect(await away).toMatchObject({ ok: false, error: expect.stringContaining("Could not reach Arakun") });
    expect(localStorage.getItem(TOKEN_KEY)).toBeNull();
  });
});
