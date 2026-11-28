// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see cloud/LICENSE)
// Every read and change of the panel. A change invalidates the reads it
// touches, always the log, and never edits the cache by hand: the server's
// answer after the change is the truth.

import { queryOptions, useMutation, useQueryClient, type QueryClient, type QueryKey } from "@tanstack/react-query";
import { api } from "./api";
import type {
  BetaPlatform,
  BetaSignup,
  ClosedAnswer,
  CloudRequest,
  FlagsAnswer,
  GitHubAccount,
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

export type SignupStatus = BetaSignup["status"];

export const signupsQuery = ({ status, platform }: { status: SignupStatus; platform?: BetaPlatform }) =>
  queryOptions({
    queryKey: ["signups", { status, platform }],
    queryFn: () => api<BetaSignup[]>("GET", `beta/signups${search({ platform, status })}`),
  });

export const usersQuery = ({ q, after }: { q?: string; after?: string }) =>
  queryOptions({
    queryKey: ["users", { q, after }],
    queryFn: () => api<UserPage>("GET", `users${search({ q, after })}`),
  });

export const userQuery = (accountId: number) =>
  queryOptions({
    queryKey: ["user", accountId],
    queryFn: () => api<UserDetail>("GET", `users/${accountId}`),
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

export const logQuery = ({ accountId, before }: { accountId?: number; before?: number }) =>
  queryOptions({
    queryKey: ["log", { accountId, before }],
    queryFn: () => api<LogPage>("GET", `log${search({ account_id: accountId, before })}`),
  });

// The prefixes that invalidate every page or filter of a list.
const REQUESTS: QueryKey = ["requests"];
const SIGNUPS: QueryKey = ["signups"];
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
  accountId: (variables: V) => number,
  lists: QueryKey[],
) {
  const client = useQueryClient();
  return useMutation({
    mutationFn,
    onSuccess: (_data, variables) => invalidate(client, [...lists, userQuery(accountId(variables)).queryKey]),
  });
}

/// Grants a cloud access request and the `cloud` flag with it.
export function useGrant() {
  return useAccountChange(
    (accountId: number) => api<CloudRequest>("PUT", `cloud/requests/${accountId}/grant`),
    (accountId) => accountId,
    [REQUESTS, USERS],
  );
}

export function useDecline() {
  return useAccountChange(
    (accountId: number) => api<CloudRequest>("PUT", `cloud/requests/${accountId}/decline`),
    (accountId) => accountId,
    [REQUESTS],
  );
}

type SignupDecision = { accountId: number; platform: BetaPlatform; decision: "admit" | "decline" };

/// Admits or declines a beta sign-up. Admitting invites no one: the operator
/// adds the email in the store first.
export function useDecideSignup() {
  return useAccountChange(
    ({ accountId, platform, decision }: SignupDecision) => api<BetaSignup>("PUT", `beta/signups/${accountId}/${platform}/${decision}`),
    (v) => v.accountId,
    [SIGNUPS],
  );
}

type FlagChange = { accountId: number; flag: string };

export function useAddFlag() {
  return useAccountChange(
    ({ accountId, flag }: FlagChange) => api<FlagsAnswer>("PUT", `users/${accountId}/flags/${encodeURIComponent(flag)}`),
    (v) => v.accountId,
    // A request shows whether the account has the `cloud` flag.
    [USERS, REQUESTS],
  );
}

export function useRemoveFlag() {
  return useAccountChange(
    ({ accountId, flag }: FlagChange) => api<FlagsAnswer>("DELETE", `users/${accountId}/flags/${encodeURIComponent(flag)}`),
    (v) => v.accountId,
    [USERS, REQUESTS],
  );
}

export function useSetPlan() {
  return useAccountChange(
    ({ accountId, plan }: { accountId: number; plan: string }) => api<void>("PUT", `users/${accountId}/plan`, { plan }),
    (v) => v.accountId,
    [USERS],
  );
}

export function useBlock() {
  return useAccountChange(
    ({ accountId, reason }: { accountId: number; reason?: string }) => api<void>("PUT", `users/${accountId}/block`, { reason }),
    (v) => v.accountId,
    // A request and a sign-up show whether the account is blocked.
    [USERS, REQUESTS, SIGNUPS],
  );
}

export function useUnblock() {
  return useAccountChange(
    (accountId: number) => api<void>("DELETE", `users/${accountId}/block`),
    (accountId) => accountId,
    [USERS, REQUESTS, SIGNUPS],
  );
}

/// Ends every signed-in device of the account.
export function useCloseSessions() {
  return useAccountChange(
    (accountId: number) => api<ClosedAnswer>("DELETE", `users/${accountId}/sessions`),
    (accountId) => accountId,
    [],
  );
}

/// The account of a GitHub user, made if it has none, so the operator can set
/// it up before the person first signs in.
export function useGitHubAccount() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (githubId: number) => api<GitHubAccount>("PUT", `github/${githubId}`),
    onSuccess: () => invalidate(client, [USERS]),
  });
}

export function useCreateNotice() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (notice: NewNotice) => api<NoticeCreated>("POST", "notices", notice),
    onSuccess: (_data, notice) =>
      invalidate(client, notice.account_id === undefined ? [NOTICES] : [NOTICES, userQuery(notice.account_id).queryKey]),
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
