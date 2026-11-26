// Which way in this is, and where the page goes from an address, as `start()`
// in the old app.js: the account host, an old relay host, a host that failed, or the
// computer's own server. The root route asks `whereTo` before every screen.

import type { QueryClient } from "@tanstack/react-query";
import { type Boot, dropMachine, dropPush, LAST_MACHINE, meQuery, pick, pickedMachine, setSaid } from "./account";
import { activeLink, setActiveLink } from "./api";
import { type Machine, RELAY } from "./globals";
import { createLink, type Link } from "./link";
import { pair } from "./pair";
import { loadToken } from "./token";

export type Way = "account" | "update" | "unreachable" | "local";

// The pairing code arrives in the QR link (?p=), with the label of the key
// the computer signs its handshake with (&k=). The code works once, so it is
// never kept, and the address bar drops both at once.
let pairCode: { code: string; key: string } | null = null;
export function takePairCode(): { code: string; key: string } {
  if (pairCode === null) {
    const q = new URLSearchParams(location.search);
    const key = q.get("k") || "";
    pairCode = { code: q.get("p") || "", key: /^[a-z2-7]{16}$/.test(key) ? key : "" };
    if (pairCode.code) history.replaceState(null, "", location.pathname + location.hash);
  }
  const taken = pairCode;
  pairCode = { code: "", key: "" };
  return taken;
}

// GitHub's sign-in comes back to `/?sign_in=failed` when it did not work.
// The address bar keeps only the page.
function takeSignInFailed(): boolean {
  const q = new URLSearchParams(location.search).get("sign_in");
  if (q) history.replaceState(null, "", location.pathname + location.hash);
  return q === "failed";
}

let way: Way | null = null;
let detecting: Promise<Way> | null = null;
let accountStarted = false;
// The pairing code from the address, traded once; "" or why it failed. Every
// ask waits on the same trade: dropping the code from the address makes the
// router ask again before the trade is done.
let localStart: Promise<string> | null = null;
let localStartSeen = false;
// A session to open once a computer is picked from the list.
let pendingOpen = "";
// The session a notification opened, which may be another computer's.
let noticeSession = "";
// Why the pairing screen shows: a code that did not work, or the computer let go of this phone.
let pairNote = "";
let fatalWait = 2000;

/** The way in, asked once. A host that failed is asked again by `retryStart`. */
export function detectWay(): Promise<Way> {
  if (!detecting) {
    RELAY.forgetOldPairing();
    detecting = RELAY.detect().then((found) => {
      way = found === "account" || found === "update" || found === "unreachable" ? found : "local";
      if (way !== "unreachable") fatalWait = 2000;
      // A subscription made on this address would only bring taps here.
      if (way === "update") dropPush();
      return way;
    });
  }
  return detecting;
}

/** With an account (sign in, a list of computers, the relay) or the computer's own server (a pairing code). */
export function mode(): "account" | "local" {
  return way === "account" ? "account" : "local";
}

/** Whether it is known which way in this is. */
export function started(): boolean {
  return way !== null && way !== "unreachable";
}

/** Forgets that the host failed, so the next screen asks again. */
export function retryStart(): void {
  if (way === "unreachable") {
    way = null;
    detecting = null;
  }
}

/** How long the Cannot connect screen waits before it asks again, a little longer each time. */
export function nextFatalWait(): number {
  const ms = fatalWait;
  fatalWait = Math.min(fatalWait * 2, 30_000);
  return ms;
}

export function pairMessage(): string {
  return pairNote;
}

export function setPairMessage(text: string): void {
  pairNote = text;
}

/** Opens a socket of the computer's own, unless one is open. */
export function startLocal(): void {
  if (activeLink()) return;
  setActiveLink(createLink({ mode: "local" }));
}

/** The session to open after a computer is picked from the list, once. */
export function takePendingOpen(): string {
  const id = pendingOpen;
  pendingOpen = "";
  return id;
}

export function setPendingOpen(id: string): void {
  pendingOpen = id;
}

/** Marks `id` as opened from a notification: with several computers it may be another's. */
export function noteNoticeOpen(id: string, computers: number): void {
  noticeSession = computers > 1 ? id : "";
}

/**
 * Opens session `id` on the link. The open goes first: it clears what the
 * link watches, so the notification check is armed after it.
 */
export function openSession(link: Link, id: string): void {
  link.send({ type: "open", session: id });
  if (noticeSession === id) link.watchNotice(id);
  noticeSession = "";
}

/** Everything back as before the first screen; for tests. */
export function resetStart(): void {
  dropMachine();
  activeLink()?.close();
  setActiveLink(null);
  way = null;
  detecting = null;
  accountStarted = false;
  localStart = null;
  localStartSeen = false;
  pairCode = null;
  pendingOpen = "";
  noticeSession = "";
  pairNote = "";
  fatalWait = 2000;
  setSaid("");
}

// The screens that need a computer.
function needsComputer(path: string): boolean {
  return ["/inbox", "/sessions", "/new", "/usage", "/settings", "/offline"].includes(path) || /^\/s\/[^/]+$/.test(path);
}

function sessionOf(path: string): string {
  const m = /^\/s\/([^/]+)$/.exec(path);
  return m ? decodeURIComponent(m[1]) : "";
}

/** Where the page goes instead of `path`, or null to stay. */
export async function whereTo(path: string, qc: QueryClient): Promise<string | null> {
  const found = await detectWay();
  const go = (to: string) => (to === path ? null : to);
  if (found === "unreachable") return go("/fatal");
  if (found === "update") return go("/moved");
  if (found === "local") return localWhere(path, go);
  return accountWhere(path, qc, go);
}

async function localWhere(path: string, go: (to: string) => string | null): Promise<string | null> {
  if (!localStart) {
    const { code, key } = takePairCode();
    localStart = code ? pair(code, key).then((done) => (done.ok ? "" : done.error)) : Promise.resolve("");
  }
  const failed = await localStart;
  // A code that did not work sends to pairing once, with why.
  if (failed && !localStartSeen) {
    localStartSeen = true;
    pairNote = failed;
    return go("/pair");
  }
  if (path === "/pair") return null;
  if (!loadToken()) return go("/pair");
  startLocal();
  return needsComputer(path) ? null : go("/inbox");
}

async function accountWhere(path: string, qc: QueryClient, go: (to: string) => string | null): Promise<string | null> {
  const first = !accountStarted;
  accountStarted = true;
  if (first && takeSignInFailed()) setSaid("Sign-in with GitHub did not work. Try again.");
  let boot: Boot;
  try {
    boot = await qc.ensureQueryData(meQuery);
  } catch {
    // The list says so, with Try again; nobody is known yet.
    return go("/computers");
  }
  if (boot.screen === "signin") return go("/signin");
  if (boot.screen === "blocked") return go("/computers");
  if (pickedMachine()) return needsComputer(path) || path === "/computers" ? null : go("/inbox");
  const machines = boot.machines || [];
  if (first && boot.machine) {
    pick(boot.machine);
    return needsComputer(path) ? null : go("/inbox");
  }
  const id = first ? sessionOf(path) : "";
  if (id) {
    // A tap on a notification names a session of the computer opened last.
    const last = machines.find((m: Machine) => m.label === localStorage.getItem(LAST_MACHINE));
    if (last) {
      pick(last);
      noteNoticeOpen(id, machines.length);
      return null;
    }
    if (machines.length) {
      // No computer to take the tap to: the person picks, and the session opens there.
      pendingOpen = id;
      setSaid(RELAY.tapSentence());
    }
  }
  return go("/computers");
}

// What the page is showing, in the words `RELAY.tapRoute` knows.
function screenOf(path: string): string {
  return { "/moved": "update", "/signin": "signin", "/fatal": "fatal", "/computers": "machines" }[path] || "inbox";
}

/**
 * A tap on a notification while the page is open. On the account host the
 * page can rest with no computer picked (the list, sign-in), and there is no
 * session to show there. `go` navigates.
 */
export function tappedNotice(id: string, path: string, qc: QueryClient, go: (to: string) => void): void {
  const to = `/s/${encodeURIComponent(id)}`;
  // Not known yet which way in this is: the start takes the tap, as from the address.
  if (!started()) return go(to);
  let account: { me: unknown; blocked: boolean; machines: Machine[] } | null = null;
  if (mode() === "account") {
    const boot = qc.getQueryData<Boot>(["me"]);
    const listed = qc.getQueryData<{ list?: Machine[]; blocked?: boolean }>(["machines"]);
    account = {
      me: boot?.me ?? null,
      blocked: boot?.screen === "blocked" || !!listed?.blocked,
      machines: listed?.list ?? boot?.machines ?? [],
    };
  }
  const route = RELAY.tapRoute(screenOf(path), account, !!pickedMachine(), localStorage.getItem(LAST_MACHINE));
  if (route.go === "stay") return;
  const computers = account ? account.machines.length : 1;
  if (route.go === "list") {
    pendingOpen = id;
    setSaid(RELAY.tapSentence());
    return go("/computers");
  }
  if (route.go === "pick") pick(route.machine);
  noteNoticeOpen(id, computers);
  go(to);
}
