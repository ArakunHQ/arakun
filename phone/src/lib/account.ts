// The account host (app.arakun.dev): who is signed in, their computers, the
// one picked, and the other devices signed in. The requests go through the
// functions relay.js has; Query keeps their answers under "me", "machines"
// and "sessions".

import { type QueryClient, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useSyncExternalStore } from "react";
import { activeLink, setActiveLink } from "./api";
import { type ActResult, type BootResult, type Machine, type MachinesResult, RELAY } from "./globals";
import { createLink } from "./link";
import { toast } from "./toast";

/** The computer opened last, by label: a tap on a notification opens it again. */
export const LAST_MACHINE = "arakun.machine";

/** What `/me` and the first list said. `me` and `machines` are there once signed in. */
export type Boot = BootResult & {
  me?: { login?: string; notices?: Notice[]; [k: string]: unknown };
  machines?: Machine[];
  machine?: Machine | null;
  reason?: string | null;
};

export interface Notice {
  id: string | number;
  text: string;
  url?: string;
}

export type SessionsResult = { list: unknown[] } | { signedOut: true };

export const meQuery = { queryKey: ["me"], queryFn: () => RELAY.boot() as Promise<Boot>, staleTime: Infinity } as const;

// ---- the sentence over the list of computers ----------------------------------
// Why the page shows the list: the relay sent it back, or a tap could not be
// followed to a computer.

let said = "";
const saidListeners = new Set<() => void>();

export function setSaid(text: string): void {
  said = text;
  for (const fn of saidListeners) fn();
}

export function useSaid(): string {
  return useSyncExternalStore(
    (fn) => {
      saidListeners.add(fn);
      return () => saidListeners.delete(fn);
    },
    () => said,
  );
}

// ---- the computer picked -------------------------------------------------------

let picked: Machine | null = null;

export function pickedMachine(): Machine | null {
  return picked;
}

/** Connects to one of the account's computers, offline ones too: the relay says when it comes back. */
export function pick(machine: Machine): void {
  dropMachine();
  picked = machine;
  setSaid("");
  localStorage.setItem(LAST_MACHINE, machine.label);
  setActiveLink(createLink({ mode: "relay", machine }));
}

/** Lets go of the computer picked, and of the link to it. */
export function dropMachine(): void {
  const link = activeLink();
  setActiveLink(null);
  link?.close();
  picked = null;
}

/** Back to the list of computers, with a sentence when the relay sent us there. */
export function leaveMachine(qc: QueryClient, text = ""): void {
  dropMachine();
  setSaid(text);
  void qc.invalidateQueries({ queryKey: ["machines"] });
}

/**
 * The account host said the session is gone: only sign-in is left, and
 * nothing of the account stays on this phone, however the sign-out came. The
 * computer drops this phone from its push list itself when it hears of it.
 */
export function signedOut(qc: QueryClient, text = ""): void {
  dropMachine();
  dropPush();
  RELAY.forgetAccountState(localStorage);
  qc.setQueryData(["me"], { screen: "signin" });
  qc.removeQueries({ queryKey: ["machines"] });
  qc.removeQueries({ queryKey: ["sessions"] });
  setSaid(text);
}

/**
 * Drops the browser's push subscription, made with a key of one of the
 * account's computers. `getRegistration` and not `ready`: where the service
 * worker could not be registered, `ready` never settles.
 */
export function dropPush(): void {
  if (!("serviceWorker" in navigator) || !isSecureContext) return;
  navigator.serviceWorker
    .getRegistration()
    .then((reg) => reg?.pushManager?.getSubscription())
    .then((sub) => sub?.unsubscribe())
    .catch(() => {});
}

// ---- the hooks -----------------------------------------------------------------

export function useMe(enabled = true) {
  return useQuery({ ...meQuery, enabled });
}

// Signs out here when an answer says the session is gone.
function useSignedOutBy(gone: boolean) {
  const qc = useQueryClient();
  useEffect(() => {
    if (gone) signedOut(qc);
  }, [gone, qc]);
}

/** The account's computers; asked again while on screen, for their online states. */
export function useMachines(enabled = true) {
  const q = useQuery({
    queryKey: ["machines"],
    enabled,
    queryFn: () => RELAY.machines() as Promise<MachinesResult>,
    refetchInterval: 15_000,
  });
  useSignedOutBy(!!q.data && "signedOut" in q.data);
  return q;
}

/** Removes one of the account's computers, then asks for the list again. Ask the person first. */
export function useRemoveMachine() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async (m: Machine): Promise<ActResult> => {
      try {
        return await RELAY.forgetMachine(m.label);
      } catch {
        return { ok: false };
      }
    },
    onSuccess: (done, m) => {
      if ("signedOut" in done) return signedOut(qc);
      if (!done.ok) toast(`Could not remove ${m.name}`);
      else if (localStorage.getItem(LAST_MACHINE) === m.label) localStorage.removeItem(LAST_MACHINE);
      void qc.invalidateQueries({ queryKey: ["machines"] });
    },
  });
}

/** Dismisses an operator's notice: gone from the screen at once, then told to the account host. */
export function useMarkNoticeRead() {
  const qc = useQueryClient();
  return useMutation({
    onMutate: (id: Notice["id"]) => {
      qc.setQueryData<Boot>(["me"], (b) =>
        b?.me ? { ...b, me: { ...b.me, notices: (b.me.notices || []).filter((n) => String(n.id) !== String(id)) } } : b,
      );
    },
    mutationFn: async (id: Notice["id"]) => {
      await fetch(`/notices/${encodeURIComponent(id)}/read`, { method: "POST", credentials: "same-origin" }).catch(() => {});
    },
  });
}

/** The account's signed-in devices, asked once each time the screen opens. */
export function useSignedInSessions() {
  const q = useQuery({
    queryKey: ["sessions"],
    queryFn: () => RELAY.sessions() as Promise<SessionsResult>,
    staleTime: 0,
    refetchOnMount: "always",
  });
  useSignedOutBy(!!q.data && "signedOut" in q.data);
  return q;
}

/**
 * Signs this phone out of the account. The session cookie is HttpOnly, so only
 * the account host can end it: sign-in shows once the host said the session
 * is gone, and not before. Otherwise a reload would be signed in again behind
 * a screen that said it was not. Ask the person first.
 */
export function useSignOut() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async () => {
      try {
        return (await fetch("/auth/logout", { method: "POST", cache: "no-store", credentials: "same-origin" })).status;
      } catch {
        // No answer: nothing ended.
        return 0;
      }
    },
    onSuccess: (status) => {
      // Still signed in: the computer, the drafts and the notifications stay as they are.
      if (!RELAY.loggedOut(status)) return alert(RELAY.signOutFailed());
      // The relay may have closed the link for the sign-out already.
      if ((qc.getQueryData<Boot>(["me"]))?.screen !== "signin") signedOut(qc);
    },
  });
}
