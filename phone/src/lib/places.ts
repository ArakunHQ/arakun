// Where a new session can start, and the form that starts one: the quick
// sheet over the tab screens and the full New session screen share it.

import { useCallback, useContext, useEffect, useState } from "react";
import { keepPlace, lastPlacePath } from "./drafts";
import { LinkContext, useLink } from "./link";
import type { ClientMessage, InboxSession, Place } from "./model";
import { sendShot, type Shot } from "./shots";
import { toast } from "./toast";

export const AGENTS: [string, string][] = [
  ["claude", "Claude Code"],
  ["droid", "Droid"],
  ["opencode", "OpenCode"],
  ["codex", "Codex"],
  ["devin", "Devin"],
  ["cursor", "Cursor"],
  ["antigravity", "Antigravity"],
];

export const agentName = (key: string): string => (AGENTS.find(([k]) => k === key) || [key, key])[1];

// A place that does not say its agent starts with the first one.
const agentOf = (p: Place): string => p.agent || AGENTS[0][0];

/** A place's branch; the environment's name stands in where the computer does not say. */
export const branchOf = (p: Place): string => p.branch || p.env || p.project;

/** As the full form's list shows a place. */
export const placeLabel = (p: Place): string => (p.env && p.env !== p.project ? `${p.project} · ${p.env}` : p.project);

export interface Project {
  name: string;
  places: Place[];
  /** The newest session's time there, 0 for none. */
  updated: number;
  working: number;
}

/** Projects with their places, freshest session first. */
export function projectList(places: Place[], inbox: InboxSession[]): Project[] {
  const by = new Map<string, Project>();
  for (const p of places) {
    if (!by.has(p.project)) by.set(p.project, { name: p.project, places: [], updated: 0, working: 0 });
    by.get(p.project)!.places.push(p);
  }
  for (const s of inbox) {
    const g = by.get(s.project as string);
    if (!g) continue;
    g.updated = Math.max(g.updated, s.updated || 0);
    if (s.status === "working") g.working++;
  }
  return [...by.values()].sort((a, b) => b.updated - a.updated);
}

/** The place used last, else the first. */
export function lastPlace(places: Place[]): Place | undefined {
  const last = lastPlacePath();
  return places.find((p) => p.path === last) || places[0];
}

/** The place a project starts in: the branch used last there, else its checkout. */
export function placeFor(places: Place[], project: string): Place | undefined {
  const mine = places.filter((p) => p.project === project);
  const last = lastPlacePath();
  return mine.find((p) => p.path === last) || mine.find((p) => p.env === p.project) || mine[0];
}

export interface SettingSet {
  key: string;
  label: string;
  current?: string;
  options: { id: string; name: string; detail?: string; disabled?: boolean }[];
}

/** What the form holds; it goes along from the quick sheet to the full form. */
export interface StartForm {
  path: string;
  agent: string;
  text: string;
  /** Settings picked here, by key; the rest stay as the agent would start. */
  picked: Record<string, string>;
}

export function formAt(place: Place, text = ""): StartForm {
  return { path: place.path, agent: agentOf(place), text, picked: {} };
}

/** The name of what setting `key` is on now: the pick here, else what the agent would start with. */
export function currentSetting(options: SettingSet[] | null, picked: Record<string, string>, key: string): string | null {
  const set = options?.find((x) => x.key === key);
  if (!set) return null;
  const id = picked[key] ?? set.current;
  return set.options.find((x) => x.id === id)?.name ?? null;
}

/** The form's state and what it does, from `init`. */
export function useStartForm(init: StartForm, { needLink = false } = {}) {
  const link = useContext(LinkContext);
  if (!link) throw new Error("useStartForm needs a LinkContext above it");
  const places = useLink((s) => s.places);
  const connected = useLink((s) => s.connected);
  const answer = useLink((s) => s.options);
  const [form, setForm] = useState(init);
  const [sent, setSent] = useState(false);
  const [shots, setShots] = useState<Shot[]>([]);

  // The model and reasoning a session there would start with, and their
  // choices. Asked again once the link is back, as an ask while away is lost.
  useEffect(() => {
    if (connected) link.send({ type: "options", env: form.path, agent: form.agent });
  }, [link, connected, form.path, form.agent]);
  const options = answer && answer.env === form.path && answer.agent === form.agent ? (answer.settings as unknown as SettingSet[]) : null;

  // The computer refused the start: the form can send again.
  useEffect(() => link.subscribeEvents((e) => void (e.type === "error" && setSent(false))), [link]);

  const setPlace = useCallback(
    (path: string) => {
      const p = places.find((x) => x.path === path);
      setForm((f) => ({ ...f, path, agent: p ? agentOf(p) : f.agent, picked: {} }));
    },
    [places],
  );
  const setAgent = (agent: string) => setForm((f) => (f.agent === agent ? f : { ...f, agent, picked: {} }));
  const setText = (text: string) => setForm((f) => ({ ...f, text }));
  const pick = (key: string, id: string) =>
    setForm((f) => {
      const picked = { ...f.picked, [key]: id };
      // The reasoning on offer depends on the model: a new model drops the old pick.
      if (key === "modelId") delete picked.reasoningEffort;
      return { ...f, picked };
    });

  const canSend = !sent && (!!form.text.trim() || shots.length > 0);
  const start = () => {
    if (!canSend) return false;
    // The quick sheet stays open while away, so it says why nothing happened.
    if (needLink && !link.getState().connected) {
      toast("Not connected. Try again once it reconnects.");
      return false;
    }
    // Photos go up in pieces before the start that names them, over the socket now.
    if (shots.length && !link.getState().connected) {
      toast("Photos wait for a connection");
      return false;
    }
    setSent(true);
    keepPlace(form.path);
    // The model goes first: the reasoning on offer depends on it.
    const settings = ["modelId", "reasoningEffort"].filter((k) => form.picked[k]).map((k) => [k, form.picked[k]]);
    const msg: ClientMessage = { type: "start", env: form.path, agent: form.agent, text: form.text, settings };
    if (shots.length) {
      // No session yet: the computer keeps an upload by its id alone.
      const send = (m: ClientMessage) => link.send(m);
      msg.uploads = shots.map((sh, i) => {
        const up = `${Date.now()}-${i}-${Math.random().toString(36).slice(2, 7)}`;
        sendShot(sh, up, "", send);
        return up;
      });
      if (!form.text.trim()) msg.text = shots.length === 1 ? "(photo)" : `(${shots.length} photos)`;
      setShots([]);
    }
    link.send(msg);
    return true;
  };

  return { form, options, sent, canSend, shots, setShots, setPlace, setAgent, setText, pick, start };
}

declare module "@tanstack/history" {
  interface HistoryState {
    /** What the quick sheet held when it opened the full form. */
    startForm?: StartForm;
    /** Set on the step the project tiles or the quick sheet put in the history, so the phone's back closes them. */
    newStep?: boolean;
  }
}
