// What the computer has told this phone, and how each message changes it.
// The screen's own state (what is typed, which sheet is open) lives in React;
// this is only what came over the link.

import type { SessionStatus } from "./format";

export type Entry = Record<string, unknown>;

/** A row of the inbox, as the computer sends it. */
export interface InboxSession {
  id: string;
  title?: string;
  agent?: string;
  status?: SessionStatus;
  project?: string;
  env?: string;
  /** Unix time of the last change. */
  updated?: number;
  added?: number;
  removed?: number;
  /** What a waiting session asks, so the inbox alone says it. */
  asks?: { kind: string; text: string };
  [k: string]: unknown;
}

/** Where a session can start. */
export interface Place {
  path: string;
  project: string;
  env?: string;
  branch?: string;
  agent?: string;
  [k: string]: unknown;
}

/** The session open on this phone. `entries` holds the ones from `from` on. */
export interface Session {
  id: string;
  title: string;
  agent: string;
  status: SessionStatus;
  count: number;
  from: number;
  entries: Entry[];
  permission: unknown;
  question: unknown;
  place: string;
  usage?: unknown;
  elapsed_ms?: number | null;
  /** When `elapsed_ms` was heard, so the clock can run on from it. */
  elapsedAt?: number;
  changes?: unknown;
  [k: string]: unknown;
}

/** This phone as the computer keeps it, from `/me`. */
export interface Me {
  id?: string;
  name?: string;
  host?: string;
  push_key?: string;
  notifying?: boolean;
  /** The two numbers and `muted` are missing from an older computer: use the defaults. */
  notices?: { done: boolean; input: boolean; error: boolean; away_after_secs?: number; min_turn_secs?: number };
  /** Ids of the sessions whose notices are off. */
  muted?: string[];
  [k: string]: unknown;
}

export type Setting = Record<string, unknown>;

export type ServerMessage =
  | { type: "pong" }
  | { type: "inbox"; host: string; sessions: InboxSession[]; places?: Place[] }
  | { type: "usage"; agents: unknown[] }
  | ({ type: "session"; id: string; from: number; count: number; entries: Entry[] } & Partial<Omit<Session, "id" | "from" | "count" | "entries">>)
  | { type: "entries"; id: string; from: number; count: number; entries: Entry[]; older?: boolean }
  | { type: "commands"; id: string; commands: unknown[] }
  | { type: "subagents"; id: string; subagents: unknown[] }
  | ({ type: "state"; id: string } & Partial<Omit<Session, "id" | "from" | "count" | "entries">>)
  | { type: "changes"; id: string; turns: unknown[] }
  | { type: "diff"; id: string; entry: number; index: number; [k: string]: unknown }
  | { type: "settings"; id: string; settings: Setting[] }
  | { type: "options"; env: string; agent: string; settings: Setting[] }
  | { type: "started"; id: string }
  | { type: "error"; message: string };

export type UsageMessage = Extract<ServerMessage, { type: "usage" }>;
export type DiffMessage = Extract<ServerMessage, { type: "diff" }>;
export type OptionsMessage = Extract<ServerMessage, { type: "options" }>;

/** What this page sends the computer. Only `type` is checked here. */
export type ClientMessage = { type: string; [k: string]: unknown };

export interface LinkState {
  connected: boolean;
  host: string;
  inbox: InboxSession[];
  places: Place[];
  session: Session | null;
  /** Both agents' plan limits. */
  usage: UsageMessage | null;
  /** The computer's VAPID public key, which this phone subscribes with. */
  pushKey: string;
  me: Me | null;
  /** Through the relay: since when (unix) the computer has been away, while it is. */
  pcOffline: number | null;
  // The rest belongs to the open session and goes when it closes.
  commands: unknown[];
  subagents: unknown[];
  /** The turns of the list of changes, once asked for. */
  changes: unknown[] | null;
  /** Diffs asked for, by `${entry}:${index}`. */
  diffs: Record<string, DiffMessage>;
  settings: Setting[] | null;
  /** The last answer about a New session form's choices; the form checks env and agent. */
  options: OptionsMessage | null;
}

/**
 * What the link tells the screen besides the state: things to show once,
 * or that the screen must act on.
 * - `toast`: a short line to show.
 * - `session-elsewhere`: the session a notification opened is not on this
 *   computer; the link has closed it, the screen goes back to the inbox.
 * - `started`: a session the New session form asked for began and is open.
 * - `error`: the computer refused something, such as a start.
 * - `linked`: the computer answered; the screen sends what it needs again.
 * - `unpaired`: no token, or the computer refused it; `message` says why ("" for none).
 * - `closed`: the relay ended the link for good (`signed_out`, `refused`,
 *   `untrusted`); `detail` is its reason.
 */
export type LinkEvent =
  | { type: "toast"; text: string }
  | { type: "session-elsewhere"; id: string }
  | { type: "started"; id: string }
  | { type: "error"; message: string }
  | { type: "linked" }
  | { type: "unpaired"; message: string }
  | { type: "closed"; kind: string; detail: string };

export function emptyLink(): LinkState {
  return {
    connected: false,
    host: "",
    inbox: [],
    places: [],
    session: null,
    usage: null,
    pushKey: "",
    me: null,
    pcOffline: null,
    options: null,
    ...sessionParts(),
  };
}

function sessionParts() {
  return { commands: [], subagents: [], changes: null, diffs: {}, settings: null };
}

/** The session `id` opened, filled from its inbox row until the computer answers. */
export function openSession(s: LinkState, id: string): LinkState {
  const row = s.inbox.find((r) => r.id === id);
  const session: Session = {
    id,
    title: row?.title ?? "",
    agent: row?.agent ?? "",
    status: row?.status ?? "idle",
    count: 0,
    from: 0,
    entries: [],
    permission: null,
    question: null,
    place: row?.project ?? "",
  };
  return { ...s, ...sessionParts(), session };
}

export function closeSession(s: LinkState): LinkState {
  return { ...s, ...sessionParts(), session: null };
}

/** What `/me` says about this phone, kept. */
export function tookMe(s: LinkState, me: Me): LinkState {
  return { ...s, host: me.host || s.host, pushKey: me.push_key || "", me };
}

/** Whether `msg` is about the session open here. */
function forOpen(s: LinkState, msg: { id: string }): s is LinkState & { session: Session } {
  return !!s.session && s.session.id === msg.id;
}

/** The state after `msg`; `s` itself is left as it was. `now` stamps `elapsedAt`. */
export function reduce(s: LinkState, msg: ServerMessage, now: number = Date.now()): LinkState {
  switch (msg.type) {
    case "inbox":
      return { ...s, host: msg.host, inbox: msg.sessions, places: msg.places || [] };
    case "usage":
      return { ...s, usage: msg };
    case "session": {
      if (!forOpen(s, msg)) return s;
      const { type: _, ...fields } = msg;
      const before = s.session.entries;
      const from = s.session.from;
      // Opened again after a reconnect: the computer sends its last entries,
      // which overlap what is loaded here (older ones included).
      const overlaps = before.length > 0 && msg.from >= from && msg.from <= from + before.length;
      const session: Session = { ...s.session, ...fields, elapsedAt: now };
      if (overlaps) {
        session.from = from;
        session.entries = before.slice(0, msg.from - from).concat(msg.entries);
      }
      return { ...s, session };
    }
    case "entries": {
      if (!forOpen(s, msg)) return s;
      const old = s.session;
      let from = old.from;
      let entries: Entry[];
      if (msg.older) {
        // Fills [msg.from, old.from) before what is loaded.
        entries = msg.entries.slice(0, Math.max(0, old.from - msg.from)).concat(old.entries);
        from = msg.from;
      } else if (msg.from <= old.from) {
        from = msg.from;
        entries = msg.entries;
      } else {
        entries = old.entries.slice(0, msg.from - old.from).concat(msg.entries);
      }
      return { ...s, session: { ...old, count: msg.count, from, entries } };
    }
    case "commands":
      return forOpen(s, msg) ? { ...s, commands: msg.commands } : s;
    case "subagents":
      return forOpen(s, msg) ? { ...s, subagents: msg.subagents } : s;
    case "state": {
      if (!forOpen(s, msg)) return s;
      const { type: _, ...fields } = msg;
      return { ...s, session: { ...s.session, ...fields, elapsedAt: now } };
    }
    case "changes":
      return forOpen(s, msg) ? { ...s, changes: msg.turns } : s;
    case "diff":
      return forOpen(s, msg) ? { ...s, diffs: { ...s.diffs, [`${msg.entry}:${msg.index}`]: msg } } : s;
    case "settings":
      return forOpen(s, msg) ? { ...s, settings: msg.settings } : s;
    case "options":
      return { ...s, options: msg };
    case "started":
      return openSession(s, msg.id);
    case "error":
    case "pong":
      return s;
  }
  return s;
}
