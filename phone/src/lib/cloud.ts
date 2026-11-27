// Cloud environments, on the account host: `arakun-server` in a container
// Arakun runs, which keeps working with the computer off. They run on CPU we
// pay for, so an account asks for access first (GET and POST /cloud/access)
// and the operator grants the `cloud` flag by hand. With it, the account makes
// one (/cloud/machines) and spends a monthly allowance of awake hours
// (GET /cloud/usage). Once it runs, the environment is in the Computers list
// like any server, and opening it there wakes it.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

/** The account flag the operator grants. cloud/src/cloud_access.ts has the same. */
export const CLOUD_FLAG = "cloud";

export interface Access {
  status: "none" | "pending" | "granted" | "declined";
  requested_at: number | null;
  decided_at: number | null;
  /** For "declined": when the account may ask again, in Unix seconds. */
  retry_at: number | null;
}

/** Why an environment sleeps and does not wake by itself. */
export type Limit = "stopped_all" | "held" | "allowance_used" | "task_cap";

export interface CloudMachine {
  label: string;
  name: string;
  running: boolean;
  snapshot: boolean;
  last_active: number | null;
  task_ms: number;
  stopped: Limit | null;
}

export interface Usage {
  month: string;
  used_ms: number;
  allowance_ms: number;
  left_ms: number;
  task_cap_ms: number;
  stopped: Exclude<Limit, "task_cap"> | null;
  message: string | null;
}

/** The longest reason the service takes. */
export const MAX_REASON = 500;

/** A length of time as the service says it: whole minutes under an hour, else hours to a tenth. */
export function hours(ms: number): string {
  if (ms < 3_600_000) {
    const m = Math.round(ms / 60_000);
    return m === 1 ? "1 minute" : `${m} minutes`;
  }
  const h = Math.round((ms / 3_600_000) * 10) / 10;
  return h === 1 ? "1 hour" : `${h} hours`;
}

/** What is left of this month's hours. */
export function usageLine(u: Usage): string {
  return `${hours(u.left_ms)} of ${hours(u.allowance_ms)} left this month`;
}

/** Whether the account may ask now: never did, or was declined long enough ago. */
export function canAsk(a: Access, now = Date.now() / 1000): boolean {
  if (a.status === "none") return true;
  return a.status === "declined" && (a.retry_at == null || a.retry_at <= now);
}

/** Where the account's request stands. */
export function accessLine(a: Access, now = Date.now() / 1000): string {
  switch (a.status) {
    case "pending":
      return "You asked for access. We look at each request by hand and send a notice when it is decided.";
    case "granted":
      return "You have access.";
    case "declined":
      if (a.retry_at && a.retry_at > now) {
        const days = Math.ceil((a.retry_at - now) / 86_400);
        return `Not this time. You can ask again in ${days === 1 ? "1 day" : `${days} days`}.`;
      }
      return "Not last time, but you can ask again.";
    default:
      return "A cloud environment keeps your sessions working with your computer off. We give access by hand.";
  }
}

/** What an environment's row says: awake, asleep, or why it stopped. */
export function machineLine(m: CloudMachine, u: Usage | null): string {
  if (m.running) return "Awake";
  switch (m.stopped) {
    case null:
      return m.snapshot ? "Asleep. Opening it wakes it." : "Starting. The first start takes a few minutes.";
    case "task_cap":
      return `Stopped after ${u ? hours(u.task_cap_ms) : "the longest one task may run"}, the most one task may run. Wake it to go on.`;
    case "allowance_used":
    case "stopped_all":
    case "held":
      if (u?.message) return u.message;
      return m.stopped === "allowance_used" ? "Stopped: this month's hours are used." : "Stopped by Arakun for now.";
  }
}

/** The service's reason when it gives one. */
async function why(res: Response, fallback: string): Promise<string> {
  try {
    const body = (await res.json()) as { error?: string };
    if (body.error) return body.error;
  } catch {
    // No body to read.
  }
  return fallback;
}

async function get<T>(path: string): Promise<T | null> {
  const res = await fetch(path, { cache: "no-store", credentials: "same-origin" });
  // Signed out, or no access: nothing to show here.
  if (res.status === 401 || res.status === 403) return null;
  // Cloud environments are not set up on this host.
  if (res.status === 503) return null;
  if (!res.ok) throw new Error(String(res.status));
  return (await res.json()) as T;
}

async function send(path: string, method: string, fallback: string, body?: unknown): Promise<Response> {
  const res = await fetch(path, {
    method,
    cache: "no-store",
    credentials: "same-origin",
    headers: body === undefined ? undefined : { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!res.ok) throw new Error(await why(res, fallback));
  return res;
}

/** Where the account stands with access. Only on the account host. */
export function useCloudAccess(enabled: boolean) {
  return useQuery({ queryKey: ["cloud", "access"], queryFn: () => get<Access>("/cloud/access"), enabled, staleTime: 60_000, retry: false });
}

/** The account's environments; null when the host has none set up. */
export function useCloudMachines(enabled: boolean) {
  return useQuery({
    queryKey: ["cloud", "machines"],
    queryFn: () => get<CloudMachine[]>("/cloud/machines"),
    enabled,
    retry: false,
    // A new environment starts in the background: ask until it runs.
    refetchInterval: (q) => (q.state.data?.some((m) => !m.running && !m.stopped && !m.snapshot) ? 5_000 : 30_000),
  });
}

/** This month's hours. */
export function useCloudUsage(enabled: boolean) {
  return useQuery({ queryKey: ["cloud", "usage"], queryFn: () => get<Usage>("/cloud/usage"), enabled, retry: false, refetchInterval: 60_000 });
}

/** Asks for access, with an optional reason for the reviewer. */
export function useAskAccess() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async (reason: string): Promise<Access> => {
      const res = await send("/cloud/access", "POST", "Could not ask. Try again.", reason ? { reason } : {});
      return (await res.json()) as Access;
    },
    onSuccess: (a) => qc.setQueryData(["cloud", "access"], a),
  });
}

/** Makes the account's environment and starts it; one each for now. */
export function useMakeMachine() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async (): Promise<CloudMachine> => {
      const res = await send("/cloud/machines", "POST", "Could not make the cloud environment. Try again.", {});
      return (await res.json()) as CloudMachine;
    },
    onSuccess: (m) => {
      qc.setQueryData<CloudMachine[] | null>(["cloud", "machines"], (list) => [...(list ?? []), m]);
      void qc.invalidateQueries({ queryKey: ["machines"] });
    },
  });
}

/** Wakes, stops or removes one environment. */
export function useMachineAction() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async ({ m, action }: { m: CloudMachine; action: "wake" | "sleep" | "remove" }): Promise<void> => {
      const path = `/cloud/machines/${encodeURIComponent(m.label)}`;
      if (action === "remove") await send(path, "DELETE", "Could not remove it. Try again.");
      else await send(`${path}/${action}`, "POST", action === "wake" ? "Could not wake it. Try again." : "Could not stop it. Try again.");
    },
    onSettled: (_d, _e, { action }) => {
      void qc.invalidateQueries({ queryKey: ["cloud"] });
      if (action === "remove") void qc.invalidateQueries({ queryKey: ["machines"] });
    },
  });
}
