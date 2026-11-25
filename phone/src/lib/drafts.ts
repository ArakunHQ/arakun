// What the page keeps on this phone: drafts, the last place a session started,
// and the shortcuts. On the account host each computer keeps its own, since a
// draft or a shortcut of one computer means nothing on another.

import { pickedMachine } from "./account";
import { RELAY } from "./globals";
import { mode } from "./start";
import { toast } from "./toast";

// The keys the old app.js used, so what was kept before the update is still found.
// The colon matters: RELAY.scoped puts the computer before it.
export const DRAFT_KEY = "arakun.draft:";
/** Where the last session started, offered first for the next. */
export const PLACE_KEY = "arakun.place";
export const CUTS_KEY = "arakun.shortcuts";

/** `key` for the computer in use; as it is when served by the computer itself. */
export function kept(key: string): string {
  return RELAY.scoped(key, mode() === "account" ? (pickedMachine()?.label ?? null) : null);
}

// Between computers on the account host there is no key to keep anything under.
function nowhereToKeep(): boolean {
  return mode() === "account" && !pickedMachine();
}

/** What is being written survives a switch of session, a reload, or the phone closing the app. */
export function keepDraft(id: string, text: string): void {
  if (nowhereToKeep()) return;
  try {
    if (text) localStorage.setItem(kept(DRAFT_KEY) + id, text);
    else localStorage.removeItem(kept(DRAFT_KEY) + id);
  } catch {
    // Storage full or off: the draft lives only in memory.
  }
}

export function loadDraft(id: string): string {
  try {
    return localStorage.getItem(kept(DRAFT_KEY) + id) || "";
  } catch {
    return "";
  }
}

/** Where the last session started on this computer, if it was kept. */
export function lastPlacePath(): string | null {
  try {
    return localStorage.getItem(kept(PLACE_KEY));
  } catch {
    return null;
  }
}

export function keepPlace(path: string): void {
  if (nowhereToKeep()) return;
  try {
    localStorage.setItem(kept(PLACE_KEY), path);
  } catch {
    // Storage full or off: the next form starts on the first place.
  }
}

// ---- shortcuts: phrases kept on this phone, to send without typing again ----

export interface Cut {
  id: string;
  name?: string;
  text: string;
  /** Sends on a tap, rather than filling the box. */
  send?: boolean;
  /** How often it was used. */
  n?: number;
}

/** Where a number goes each time the shortcut is used. */
export const BLANK = "{number}";
/** A phrase this short sends on a tap by default. */
export const CUT_SHORT = 24;

export function loadCuts(): Cut[] {
  try {
    const v = JSON.parse(localStorage.getItem(kept(CUTS_KEY)) || "[]");
    return Array.isArray(v) ? v.filter((c) => c && c.id && typeof c.text === "string" && c.text) : [];
  } catch {
    return [];
  }
}

export function storeCuts(cuts: Cut[]): boolean {
  if (nowhereToKeep()) return false;
  try {
    localStorage.setItem(kept(CUTS_KEY), JSON.stringify(cuts));
    return true;
  } catch {
    toast("Could not save the shortcut");
    return false;
  }
}

/** A blank has to be filled first, so that one never sends on a tap. */
export function cutSends(c: Cut): boolean {
  return !!c.send && !c.text.includes(BLANK);
}

export function cutName(text: string): string {
  const t = text.replace(BLANK, "…").replace(/\s+/g, " ").trim();
  return t.length > 28 ? `${t.slice(0, 27).trimEnd()}…` : t;
}
