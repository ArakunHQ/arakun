// Every read and change of the panel. A change invalidates the reads it
// touches, always the log, and never edits the cache by hand: the server's
// answer after the change is the truth.

import { queryOptions, useMutation, useQueryClient, type QueryClient, type QueryKey } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { ApiError, api } from "./api";
import type {
  ClosedAnswer,
  CloudRequest,
  FlagsAnswer,
  LogPage,
  MachineRow,
  Me,
  NewNotice,
  NoticeCreated,
  NoticeRow,
  Plans,
  UserDetail,
  UserPage,
} from "./types";

export type RequestStatus = CloudRequest["status"];

/// A query string from the values that are set, with its "?".
function search(params: Record<string, string | number | undefined>): string {
  const query = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) if (value !== undefined) query.set(key, String(value));
  const text = query.toString();
  return text ? `?${text}` : "";
}

export const meQuery = queryOptions({
  queryKey: ["me"],
  queryFn: () => api<Me>("GET", "me"),
});

export const requestsQuery = (status?: RequestStatus) =>
  queryOptions({
    queryKey: ["requests", status ?? "all"],
    queryFn: () => api<CloudRequest[]>("GET", `cloud/requests${search({ status })}`),
  });

export const usersQuery = ({ q, after }: { q?: string; after?: string }) =>
  queryOptions({
    queryKey: ["users", { q, after }],
    queryFn: () => api<UserPage>("GET", `users${search({ q, after })}`),
  });

export const userQuery = (githubId: number) =>
  queryOptions({
    queryKey: ["user", githubId],
    queryFn: () => api<UserDetail>("GET", `users/${githubId}`),
  });

export const machinesQuery = queryOptions({
  queryKey: ["machines"],
  queryFn: () => api<MachineRow[]>("GET", "machines"),
});

export const noticesQuery = queryOptions({
  queryKey: ["notices"],
  queryFn: () => api<NoticeRow[]>("GET", "notices"),
});

export const plansQuery = queryOptions({
  queryKey: ["plans"],
  queryFn: () => api<Plans>("GET", "plans"),
});

export const logQuery = ({ githubId, before }: { githubId?: number; before?: number }) =>
  queryOptions({
    queryKey: ["log", { githubId, before }],
    queryFn: () => api<LogPage>("GET", `log${search({ github_id: githubId, before })}`),
  });

// The prefixes that invalidate every page or filter of a list.
const REQUESTS: QueryKey = ["requests"];
const USERS: QueryKey = ["users"];
const ANY_USER: QueryKey = ["user"];
const NOTICES: QueryKey = ["notices"];
const LOG: QueryKey = ["log"];

/// Awaited, so the button stays busy until the lists show the change.
function invalidate(client: QueryClient, keys: QueryKey[]): Promise<unknown> {
  return Promise.all([...keys, LOG].map((queryKey) => client.invalidateQueries({ queryKey })));
}

/// A change to one account: the lists it shows in and its own screen.
function useAccountChange<V, T>(
  mutationFn: (variables: V) => Promise<T>,
  githubId: (variables: V) => number,
  lists: QueryKey[],
) {
  const client = useQueryClient();
  return useMutation({
    mutationFn,
    onSuccess: (_data, variables) => invalidate(client, [...lists, userQuery(githubId(variables)).queryKey]),
  });
}

/// Grants a cloud access request and the `cloud` flag with it.
export function useGrant() {
  return useAccountChange(
    (githubId: number) => api<CloudRequest>("PUT", `cloud/requests/${githubId}/grant`),
    (githubId) => githubId,
    [REQUESTS, USERS],
  );
}

export function useDecline() {
  return useAccountChange(
    (githubId: number) => api<CloudRequest>("PUT", `cloud/requests/${githubId}/decline`),
    (githubId) => githubId,
    [REQUESTS],
  );
}

type FlagChange = { githubId: number; flag: string };

export function useAddFlag() {
  return useAccountChange(
    ({ githubId, flag }: FlagChange) => api<FlagsAnswer>("PUT", `users/${githubId}/flags/${encodeURIComponent(flag)}`),
    (v) => v.githubId,
    // A request shows whether the account has the `cloud` flag.
    [USERS, REQUESTS],
  );
}

export function useRemoveFlag() {
  return useAccountChange(
    ({ githubId, flag }: FlagChange) => api<FlagsAnswer>("DELETE", `users/${githubId}/flags/${encodeURIComponent(flag)}`),
    (v) => v.githubId,
    [USERS, REQUESTS],
  );
}

export function useSetPlan() {
  return useAccountChange(
    ({ githubId, plan }: { githubId: number; plan: string }) => api<void>("PUT", `users/${githubId}/plan`, { plan }),
    (v) => v.githubId,
    [USERS],
  );
}

export function useBlock() {
  return useAccountChange(
    ({ githubId, reason }: { githubId: number; reason?: string }) => api<void>("PUT", `users/${githubId}/block`, { reason }),
    (v) => v.githubId,
    // A request shows whether the account is blocked.
    [USERS, REQUESTS],
  );
}

export function useUnblock() {
  return useAccountChange(
    (githubId: number) => api<void>("DELETE", `users/${githubId}/block`),
    (githubId) => githubId,
    [USERS, REQUESTS],
  );
}

/// Ends every signed-in device of the account.
export function useCloseSessions() {
  return useAccountChange(
    (githubId: number) => api<ClosedAnswer>("DELETE", `users/${githubId}/sessions`),
    (githubId) => githubId,
    [],
  );
}

export function useCreateNotice() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (notice: NewNotice) => api<NoticeCreated>("POST", "notices", notice),
    onSuccess: (_data, notice) =>
      invalidate(client, notice.github_id === undefined ? [NOTICES] : [NOTICES, userQuery(notice.github_id).queryKey]),
  });
}

export function useDeleteNotice() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api<void>("DELETE", `notices/${encodeURIComponent(id)}`),
    // The ID does not say whose notice it was, so every account's screen.
    onSuccess: () => invalidate(client, [NOTICES, ANY_USER]),
  });
}

/// Signs the operator out of the panel and forgets what it read.
export function useLogout() {
  const client = useQueryClient();
  const navigate = useNavigate();
  return useMutation({
    mutationFn: async () => {
      // Not under /api: the Worker itself clears the cookie.
      const res = await fetch("/auth/logout", { method: "POST", credentials: "same-origin" });
      if (!res.ok) throw new ApiError(res.status, `the server answered ${res.status}`);
    },
    onSuccess: async () => {
      client.clear();
      await navigate({ to: "/sign-in" });
    },
  });
}
