// The JSON the panel reads, field for field as the Workers write it:
// /api/me from worker/index.ts, the rest from the cloud Worker's
// src/admin_lists.ts, src/admin_log.ts, src/cloud_access.ts, src/beta.ts and
// src/admin.ts.
// Times are Unix seconds.

/// GET /api/me
export interface Me {
  /// The operator's GitHub user ID: operators sign in to the panel with GitHub
  /// and need no account.
  github_id: number;
  login: string;
}

/// A row of GET /admin/users. `login` is empty for an account the operator
/// set up before it first signed in.
export interface UserRow {
  account_id: number;
  /// The GitHub user linked to it, for the avatar; null without one.
  github_id: number | null;
  login: string;
  plan: string;
  blocked_at: number | null;
  blocked_reason: string | null;
  created_at: number;
  last_seen_at: number;
  /// How many machines it has registered.
  machines: number;
  flags: string[];
}

/// GET /admin/users?q=&after=&limit=
export interface UserPage {
  users: UserRow[];
  /// The `after` cursor of the next page, or null on the last.
  next: string | null;
}

/// A signed-in device of one account, in GET /admin/users/<id>.
export interface SessionRow {
  id: string;
  name: string;
  kind: string;
  created_at: number;
  last_used_at: number;
  expires_at: number;
  /// Set only for a cloud machine's own session.
  machine_label: string | null;
}

/// A row of GET /admin/machines.
export interface MachineRow {
  label: string;
  account_id: number;
  github_id: number | null;
  login: string;
  name: string;
  kind: "app" | "server";
  created_at: number;
  last_seen_at: number;
}

/// GET /admin/users/<id>. A 404 means the database has no such account.
export interface UserDetail {
  user: UserRow & { email: string | null };
  sessions: SessionRow[];
  machines: Omit<MachineRow, "account_id" | "github_id" | "login">[];
  cloud_request: CloudRequest | null;
  notices: NoticeRow[];
}

/// A row of GET /admin/cloud/requests, and the answer of a grant or a decline:
/// `requestJson` in cloud/src/cloud_access.ts.
export interface CloudRequest {
  account_id: number;
  github_id: number | null;
  login: string;
  status: "pending" | "granted" | "declined";
  reason: string | null;
  requested_at: number;
  decided_at: number | null;
  decided_by: string | null;
  /// When a declined account may ask again; null otherwise.
  retry_at: number | null;
  has_flag: boolean;
  blocked: boolean;
  /// What GitHub said about the account when it asked. Null where it did not say.
  github: {
    login: string;
    created_at: number | null;
    age_days: number | null;
    public_repos: number | null;
    followers: number | null;
    following: number | null;
  };
}

export type BetaPlatform = "ios" | "android";

/// A row of GET /admin/beta/signups, and the answer of an admit or a decline:
/// `signupJson` in cloud/src/beta.ts.
export interface BetaSignup {
  account_id: number;
  github_id: number | null;
  login: string;
  platform: BetaPlatform;
  /// The address to add as a tester: an Apple ID or a Google account.
  email: string;
  status: "pending" | "admitted" | "declined";
  requested_at: number;
  decided_at: number | null;
  decided_by: string | null;
  /// When a declined account may sign up again; null otherwise.
  retry_at: number | null;
  blocked: boolean;
  /// What GitHub said about the account when it signed up. Null where it did not say.
  github: {
    login: string;
    created_at: number | null;
    age_days: number | null;
    public_repos: number | null;
    followers: number | null;
    following: number | null;
  };
}

/// A row of GET /admin/notices (an array) and of a user's notices.
export interface NoticeRow {
  id: string;
  /// Null for a notice to everyone.
  account_id: number | null;
  login: string | null;
  text: string;
  url: string | null;
  created_at: number;
  expires_at: number | null;
  /// How many accounts marked it read.
  reads: number;
}

export type LogAction =
  | "account.create"
  | "flag.grant"
  | "flag.revoke"
  | "sessions.close"
  | "block"
  | "unblock"
  | "plan.set"
  | "notice.create"
  | "notice.delete"
  | "cloud.grant"
  | "cloud.decline"
  | "cloud.machine.sleep"
  | "cloud.stop_all"
  | "cloud.resume_all"
  | "cloud.hold"
  | "cloud.release"
  | "beta.admit"
  | "beta.decline"
  | "operator.totp.add"
  | "operator.totp.replace"
  | "operator.totp.reset";

/// A row of GET /admin/log.
export interface LogEntry {
  id: number;
  at: number;
  /// Both null for a change made with the secret alone (curl).
  operator_id: number | null;
  operator_login: string | null;
  action: LogAction;
  account_id: number | null;
  login: string | null;
  detail: Record<string, string | number | null> | null;
}

/// GET /admin/log?account_id=&before=&limit=
export interface LogPage {
  entries: LogEntry[];
  /// The `before` of the next page, or null on the last.
  next: number | null;
}

/// GET /admin/plans: `free` first.
export interface Plans {
  plans: string[];
}

/// PUT and DELETE /admin/users/<id>/flags/<flag>: the flags after the change.
export interface FlagsAnswer {
  account_id: number;
  flags: string[];
}

/// DELETE /admin/users/<id>/sessions
export interface ClosedAnswer {
  account_id: number;
  closed: number;
}

/// PUT /admin/github/<github_id>: the account of a GitHub user, made if it had none.
export interface GitHubAccount {
  account_id: number;
  github_id: number;
  created: boolean;
}

/// POST /admin/notices
export interface NoticeCreated {
  id: string;
}

export interface NewNotice {
  account_id?: number;
  text: string;
  url?: string;
  expires_at?: number;
}
