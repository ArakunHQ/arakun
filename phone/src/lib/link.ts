// The one connection to the computer: a socket of its own on the computer's
// server (`local`), or a link through the relay to a computer of the account
// (`relay`). Either way it sends, fetches, keeps the state the computer
// reported, and tries again while the computer is not there.

import { createContext, useContext, useSyncExternalStore } from "react";
import { type Machine, RELAY, type RelayLink } from "./globals";
import {
  type ClientMessage,
  closeSession,
  emptyLink,
  type LinkEvent,
  type LinkState,
  type Me,
  openSession,
  reduce,
  type ServerMessage,
  tookMe,
} from "./model";
import { forgetToken, loadToken } from "./token";

export type { LinkEvent, LinkState } from "./model";

// A socket of the computer's own is pinged this often and given up on after
// this long without a word, or when it does not open in time. A dead network
// can leave it looking open for minutes, and closing it can take as long, so
// the page lets go of it and tries again at once.
export const LOCAL_PING_MS = 15_000;
export const LOCAL_SILENT_MS = 40_000;
export const LOCAL_OPEN_MS = 15_000;
const RETRY_FIRST_MS = 500;
const RETRY_MOST_MS = 8000;

// Relay endings that another try would not change.
const FINAL = new Set(["signed_out", "refused", "untrusted"]);

export interface LinkOptions {
  mode: "local" | "relay";
  /** The computer to reach through the relay; needed for `relay`. */
  machine?: Machine;
  /** Tests pass fakes; the page uses the browser's own. */
  WebSocketImpl?: typeof WebSocket;
  fetchImpl?: typeof fetch;
}

export interface Link {
  subscribe(fn: () => void): () => void;
  subscribeEvents(fn: (e: LinkEvent) => void): () => void;
  getState(): LinkState;
  /** Sends when the link is up, drops it otherwise. `open` and `close` also open and close the session in the state. */
  send(msg: ClientMessage): void;
  /** What would be an HTTP request to the computer. */
  fetch(path: string, init?: RequestInit): Promise<Response>;
  /** Back on screen or back online: an attempt that was waiting goes now. */
  reconnectNow(): void;
  /** The Retry button, and after pairing: start over now, also mid-attempt. */
  retryNow(): void;
  /** The session `id` was opened from a notification, which may be another computer's. */
  watchNotice(id: string): void;
  close(): void;
}

interface Channel {
  send(obj: unknown): void;
  close(): void;
}

export function createLink(opts: LinkOptions): Link {
  const WS = opts.WebSocketImpl ?? WebSocket;
  const doFetch = opts.fetchImpl ?? ((input: RequestInfo | URL, init?: RequestInit) => fetch(input, init));
  let state = emptyLink();
  const listeners = new Set<() => void>();
  const eventListeners = new Set<(e: LinkEvent) => void>();
  let channel: Channel | null = null;
  let retry = RETRY_FIRST_MS;
  let retryTimer: ReturnType<typeof setTimeout> | undefined;
  // The newest local attempt. One that Retry overtook while it waited on
  // `/me` must not open a second socket or call the current one lost.
  let attempt = 0;
  let attemptAbort: AbortController | null = null;
  // The session a notification tap opened, until the computer answers for it.
  let noticeOpen = "";
  let ended = false;

  const set = (next: LinkState) => {
    if (next === state) return;
    state = next;
    for (const fn of listeners) fn();
  };
  const emit = (e: LinkEvent) => {
    for (const fn of eventListeners) fn(e);
  };
  const raw = (msg: unknown) => channel?.send(msg);

  function handle(msg: ServerMessage) {
    if (noticeOpen) {
      const tapped = RELAY.noticeOutcome(noticeOpen, msg as { type?: string; id?: string });
      if (tapped !== "unrelated") {
        const id = noticeOpen;
        noticeOpen = "";
        if (tapped === "elsewhere" && state.session?.id === id) {
          // This computer does not have the session: the notification came from another.
          set(closeSession(state));
          emit({ type: "session-elsewhere", id });
          emit({ type: "toast", text: RELAY.noticeElsewhere() });
          return;
        }
      }
    }
    set(reduce(state, msg));
    if (msg.type === "started") {
      noticeOpen = "";
      raw({ type: "open", session: msg.id });
      emit({ type: "started", id: msg.id });
    } else if (msg.type === "error") {
      emit({ type: "error", message: msg.message });
      emit({ type: "toast", text: msg.message });
    }
  }

  // The computer is there and knows us: pick up where it was.
  function linked() {
    set({ ...state, connected: true, pcOffline: null });
    retry = RETRY_FIRST_MS;
    if (state.session) raw({ type: "open", session: state.session.id });
    // The socket stays up in the background; the computer must know, or it
    // would skip the push for the session shown here.
    if (document.visibilityState !== "visible") raw({ type: "visible", visible: false });
    emit({ type: "linked" });
  }

  // The computer is not there: try again, waiting a little longer each time.
  function lost() {
    if (ended) return;
    set({ ...state, connected: false });
    clearTimeout(retryTimer);
    retryTimer = setTimeout(connect, retry);
    retry = Math.min(retry * 2, RETRY_MOST_MS);
  }

  function openLocal(token: string): Channel {
    const proto = location.protocol === "https:" ? "wss" : "ws";
    const ws = new WS(`${proto}://${location.host}/ws?t=${encodeURIComponent(token)}`);
    // Wall-clock time: a phone that slept counts the time it slept.
    let heard = Date.now();
    let pinger: ReturnType<typeof setInterval> | undefined;
    let done = false;
    const end = (lose: boolean) => {
      if (done) return;
      done = true;
      clearInterval(pinger);
      clearTimeout(opening);
      ws.close();
      if (lose && channel === self) lost();
    };
    const opening = setTimeout(() => end(true), LOCAL_OPEN_MS);
    const self: Channel = {
      send(obj) {
        if (ws.readyState === WS.OPEN) ws.send(JSON.stringify(obj));
      },
      close: () => end(false),
    };
    ws.addEventListener("open", () => {
      clearTimeout(opening);
      pinger = setInterval(() => {
        if (Date.now() - heard > LOCAL_SILENT_MS) return end(true);
        self.send({ type: "ping" });
      }, LOCAL_PING_MS);
      if (channel === self) linked();
    });
    ws.addEventListener("message", (ev) => {
      heard = Date.now();
      if (channel === self) handle(JSON.parse((ev as MessageEvent).data));
    });
    ws.addEventListener("close", () => end(true));
    // Its own socket: one closed by Retry can still report an error later.
    ws.addEventListener("error", () => ws.close());
    return self;
  }

  // Through the relay the session cookie lets the socket in, and the channel
  // is the computer's client once the handshake is done; `/me` follows.
  function openRelay(): RelayLink {
    if (!opts.machine) throw new Error("a relay link needs a machine");
    const rl: RelayLink = RELAY.open(opts.machine, {
      onopen: async () => {
        try {
          const res = await rl.fetch("/me", {});
          if (res.ok) set(tookMe(state, (await res.json()) as Me));
        } catch {
          // The link closed meanwhile; onclose takes it from here.
          return;
        }
        if (channel === rl) linked();
      },
      onmessage: (msg: ServerMessage) => {
        if (channel === rl) handle(msg);
      },
      onstatus: ({ pc, since }: { pc: boolean; since?: number | null }) => {
        const away = since ? Math.floor(since / 1000) : Math.floor(Date.now() / 1000);
        set({ ...state, connected: false, pcOffline: pc ? null : away });
      },
      onclose: ({ kind, detail }: { kind: string; detail: string }) => {
        if (channel !== rl) return;
        channel = null;
        set({ ...state, connected: false, pcOffline: null });
        if (FINAL.has(kind)) return emit({ type: "closed", kind, detail });
        if (kind === "limit") emit({ type: "toast", text: `The relay is busy (${detail}). Trying again in a moment.` });
        lost();
      },
    });
    return rl;
  }

  function api(path: string, init: RequestInit = {}): Promise<Response> {
    if (opts.mode === "relay") {
      const rl = channel as RelayLink | null;
      if (!rl || !rl.ready) return Promise.reject(new Error("not connected"));
      return rl.fetch(path, { method: init.method, body: init.body ?? undefined });
    }
    const headers: Record<string, string> = { ...(init.headers as Record<string, string> | undefined) };
    const token = loadToken();
    if (token) headers.Authorization = `Bearer ${token}`;
    return doFetch(path, { ...init, headers });
  }

  // Asks the computer whether the token still stands before opening a
  // socket: a socket refused looks the same as a computer that is off, and
  // this tells the two apart.
  async function connect() {
    clearTimeout(retryTimer);
    retryTimer = undefined;
    if (ended) return;
    if (opts.mode === "relay") {
      channel = openRelay();
      return;
    }
    const token = loadToken();
    if (!token) return emit({ type: "unpaired", message: "" });
    const mine = ++attempt;
    attemptAbort?.abort();
    const abort = new AbortController();
    attemptAbort = abort;
    // A computer that went away without a word would leave this hanging.
    const timer = setTimeout(() => abort.abort(), LOCAL_OPEN_MS);
    let ok = false;
    try {
      const res = await api("/me", { signal: abort.signal });
      if (mine !== attempt || ended) return;
      if (res.status === 403) {
        forgetToken();
        return emit({ type: "unpaired", message: "This phone is no longer paired. Pair it again from Arakun." });
      }
      ok = res.ok;
      if (ok) {
        const me = (await res.json()) as Me;
        if (mine !== attempt || ended) return;
        set(tookMe(state, me));
      }
    } catch {
      ok = false;
    } finally {
      clearTimeout(timer);
    }
    if (mine !== attempt || ended) return;
    attemptAbort = null;
    if (!ok) return lost();
    channel = openLocal(token);
  }

  function reconnectNow() {
    if (!retryTimer) return;
    retry = RETRY_FIRST_MS;
    void connect();
  }

  const onVisibility = () => {
    const visible = document.visibilityState === "visible";
    raw({ type: "visible", visible });
    if (visible) reconnectNow();
  };
  document.addEventListener("visibilitychange", onVisibility);
  window.addEventListener("online", reconnectNow);
  window.addEventListener("pageshow", reconnectNow);

  // After the caller had its turn to subscribe: a missing token is an event.
  queueMicrotask(() => void connect());

  return {
    subscribe(fn) {
      listeners.add(fn);
      return () => listeners.delete(fn);
    },
    subscribeEvents(fn) {
      eventListeners.add(fn);
      return () => eventListeners.delete(fn);
    },
    getState: () => state,
    send(msg) {
      if (msg.type === "open" && typeof msg.session === "string") {
        // A session opened by hand is not the one a notification tap waits on.
        noticeOpen = "";
        set(openSession(state, msg.session));
      } else if (msg.type === "close") {
        noticeOpen = "";
        set(closeSession(state));
      }
      raw(msg);
    },
    fetch: api,
    reconnectNow,
    retryNow() {
      channel?.close();
      channel = null;
      set({ ...state, connected: false });
      retry = RETRY_FIRST_MS;
      void connect();
    },
    watchNotice(id) {
      noticeOpen = id;
    },
    close() {
      ended = true;
      clearTimeout(retryTimer);
      attemptAbort?.abort();
      channel?.close();
      channel = null;
      document.removeEventListener("visibilitychange", onVisibility);
      window.removeEventListener("online", reconnectNow);
      window.removeEventListener("pageshow", reconnectNow);
    },
  };
}

/** The link the screens below use. */
export const LinkContext = createContext<Link | null>(null);

/** A part of the link's state; `select` must return a part, not a new object. */
export function useLink<T>(select: (s: LinkState) => T): T {
  const link = useContext(LinkContext);
  if (!link) throw new Error("useLink needs a LinkContext above it");
  return useSyncExternalStore(link.subscribe, () => select(link.getState()));
}
