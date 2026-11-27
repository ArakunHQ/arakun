import { describe, expect, it } from "vitest";
import { closeSession, emptyLink, type Entry, type LinkState, openSession, reduce, type ServerMessage, tookMe } from "./model";

const NOW = 1_700_000_000_000;

const s1 = { id: "s1", title: "Fix the build", agent: "claude", status: "working" as const, project: "app" };
const place = { path: "/w/app", project: "app", env: "app", agent: "claude" };

// A session opened by hand with `count` entries loaded from `from`.
function opened(from: number, entries: Entry[]): LinkState {
  const s = openSession({ ...emptyLink(), inbox: [s1] }, "s1");
  return { ...s, session: { ...s.session!, from, count: from + entries.length, entries } };
}

function run(s: LinkState, msg: ServerMessage): LinkState {
  const before = JSON.stringify(s);
  const next = reduce(s, msg, NOW);
  // Pure: the old state is never written to.
  expect(JSON.stringify(s)).toBe(before);
  return next;
}

describe("reduce", () => {
  it("inbox sets the host, the sessions and the places", () => {
    const s = run(emptyLink(), { type: "inbox", host: "mbp", sessions: [s1], places: [place] });
    expect(s.host).toBe("mbp");
    expect(s.inbox).toEqual([s1]);
    expect(s.places).toEqual([place]);
  });

  it("inbox without places leaves none", () => {
    const s = run({ ...emptyLink(), places: [place] }, { type: "inbox", host: "mbp", sessions: [] });
    expect(s.places).toEqual([]);
  });

  it("usage keeps the message", () => {
    const msg = { type: "usage" as const, agents: [{ agent: "claude", name: "Claude" }] };
    expect(run(emptyLink(), msg).usage).toEqual(msg);
  });

  it("session is ignored when another one is open", () => {
    const s = opened(0, []);
    const msg = { type: "session" as const, id: "other", from: 0, count: 1, entries: [{ n: 1 }] };
    expect(run(s, msg)).toBe(s);
    expect(run(emptyLink(), msg).session).toBeNull();
  });

  it("session replaces what is loaded when it does not overlap", () => {
    const s = run(opened(0, []), {
      type: "session",
      id: "s1",
      title: "T",
      status: "idle",
      from: 10,
      count: 12,
      entries: [{ n: 10 }, { n: 11 }],
    });
    expect(s.session).toMatchObject({ id: "s1", title: "T", status: "idle", from: 10, count: 12, elapsedAt: NOW });
    expect(s.session!.entries).toEqual([{ n: 10 }, { n: 11 }]);
    expect(s.session).not.toHaveProperty("type");
  });

  it("session after a reconnect keeps the older entries it overlaps", () => {
    const s = run(opened(5, [{ n: 5 }, { n: 6 }, { n: 7 }]), {
      type: "session",
      id: "s1",
      from: 6,
      count: 9,
      entries: [{ n: 6 }, { n: 7 }, { n: 8 }],
    });
    expect(s.session!.from).toBe(5);
    expect(s.session!.entries).toEqual([{ n: 5 }, { n: 6 }, { n: 7 }, { n: 8 }]);
  });

  it("entries from further on replace the tail", () => {
    const s = run(opened(0, [{ n: 0 }, { n: 1 }, { n: 2 }]), {
      type: "entries",
      id: "s1",
      from: 2,
      count: 4,
      entries: [{ n: "2b" }, { n: 3 }],
    });
    expect(s.session!.count).toBe(4);
    expect(s.session!.entries).toEqual([{ n: 0 }, { n: 1 }, { n: "2b" }, { n: 3 }]);
  });

  it("entries from before what is loaded replace it all", () => {
    const s = run(opened(5, [{ n: 5 }]), { type: "entries", id: "s1", from: 3, count: 6, entries: [{ n: 3 }, { n: 4 }] });
    expect(s.session!.from).toBe(3);
    expect(s.session!.entries).toEqual([{ n: 3 }, { n: 4 }]);
  });

  it("older entries fill in before what is loaded", () => {
    const s = run(opened(5, [{ n: 5 }]), {
      type: "entries",
      id: "s1",
      from: 2,
      count: 6,
      older: true,
      entries: [{ n: 2 }, { n: 3 }, { n: 4 }, { n: 5 }],
    });
    expect(s.session!.from).toBe(2);
    expect(s.session!.entries).toEqual([{ n: 2 }, { n: 3 }, { n: 4 }, { n: 5 }]);
  });

  it("an append keeps the first words and adds the rest", () => {
    const s = run(opened(4, [{ type: "user", text: "q" }, { type: "agent", text: "Hello wor" }]), {
      type: "append",
      id: "s1",
      n: 5,
      at: 6,
      text: "world!",
    });
    expect(s.session!.entries).toEqual([{ type: "user", text: "q" }, { type: "agent", text: "Hello world!" }]);
  });

  it("an append for an entry not loaded here changes nothing", () => {
    const s = opened(4, [{ type: "agent", text: "Hi" }]);
    expect(run(s, { type: "append", id: "s1", n: 9, at: 0, text: "x" })).toBe(s);
    expect(run(s, { type: "append", id: "s1", n: 4, at: 5, text: "x" })).toBe(s);
  });

  it("entries for another session are ignored", () => {
    const s = opened(0, []);
    expect(run(s, { type: "entries", id: "x", from: 0, count: 1, entries: [{}] })).toBe(s);
  });

  it("commands and subagents are kept for the open session", () => {
    let s = opened(0, []);
    s = run(s, { type: "commands", id: "s1", commands: [{ name: "/review" }] });
    s = run(s, { type: "subagents", id: "s1", subagents: [{ id: "a" }] });
    expect(s.commands).toEqual([{ name: "/review" }]);
    expect(s.subagents).toEqual([{ id: "a" }]);
    expect(run(s, { type: "commands", id: "x", commands: [] })).toBe(s);
  });

  it("state merges into the open session", () => {
    const s = run(opened(0, [{ n: 0 }]), {
      type: "state",
      id: "s1",
      status: "needs_input",
      permission: { tool: "Bash" },
      elapsed_ms: 4000,
    });
    expect(s.session).toMatchObject({ status: "needs_input", permission: { tool: "Bash" }, elapsed_ms: 4000, elapsedAt: NOW });
    expect(s.session!.entries).toEqual([{ n: 0 }]);
    expect(s.session).not.toHaveProperty("type");
  });

  it("changes, diff and settings are kept for the open session", () => {
    let s = opened(0, []);
    s = run(s, { type: "changes", id: "s1", turns: [{ files: [] }] });
    s = run(s, { type: "diff", id: "s1", entry: 3, index: 0, name: "a.rs", lines: [] });
    s = run(s, { type: "settings", id: "s1", settings: [{ id: "model" }] });
    expect(s.changes).toEqual([{ files: [] }]);
    expect(s.diffs["3:0"]).toMatchObject({ name: "a.rs" });
    expect(s.settings).toEqual([{ id: "model" }]);
    expect(run(s, { type: "settings", id: "x", settings: [] })).toBe(s);
  });

  it("options keeps the latest answer for the New session form", () => {
    const msg = { type: "options" as const, env: "/w/app", agent: "claude", settings: [{ id: "model" }] };
    expect(run(emptyLink(), msg).options).toEqual(msg);
  });

  it("started opens the new session", () => {
    const s = run({ ...emptyLink(), inbox: [s1] }, { type: "started", id: "s1" });
    expect(s.session).toMatchObject({ id: "s1", title: "Fix the build", entries: [], from: 0, count: 0, place: "app" });
  });

  it("error and pong change nothing", () => {
    const s = opened(0, []);
    expect(run(s, { type: "error", message: "no" })).toBe(s);
    expect(run(s, { type: "pong" })).toBe(s);
  });
});

describe("openSession and closeSession", () => {
  it("opens from the inbox row, or blank when it is not there", () => {
    const s = openSession({ ...emptyLink(), inbox: [s1] }, "s2");
    expect(s.session).toMatchObject({ id: "s2", title: "", agent: "", status: "idle", place: "" });
  });

  it("drops what belonged to the session before", () => {
    let s = run(opened(0, []), { type: "settings", id: "s1", settings: [{}] });
    s = closeSession(s);
    expect(s.session).toBeNull();
    expect(s.settings).toBeNull();
  });
});

describe("tookMe", () => {
  it("keeps the host when /me has none", () => {
    const s = tookMe({ ...emptyLink(), host: "mbp" }, { id: "p1", push_key: "k" });
    expect(s.host).toBe("mbp");
    expect(s.pushKey).toBe("k");
    expect(s.me).toEqual({ id: "p1", push_key: "k" });
  });
});
