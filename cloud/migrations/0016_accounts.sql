-- Accounts get an ID of their own. Until now every table keyed an account by
-- its numeric GitHub ID; GitHub is now one identity linked to an account, in
-- `identities`, and every table points at `accounts (id)`.
--
-- An account that exists keeps its number as its ID. That number is also in
-- places SQL cannot reach: device signatures (`device|<id>|...`), the relay's
-- and channels' Durable Objects, and the clients' own stores. Keeping it means
-- every account, session and device goes on working. New accounts get a
-- random number of their own, above any GitHub ID (see src/auth.ts).
--
-- Everything is renamed in place, not rebuilt: a rebuilt `users` would have to
-- be dropped, and dropping it would cascade-delete every row that points at
-- it. SQLite rewrites the foreign keys of other tables on each rename.

ALTER TABLE users RENAME TO accounts;
ALTER TABLE accounts RENAME COLUMN github_id TO id;

-- `subject` is the provider's own ID for the person, as text so a provider
-- with string IDs fits too. For GitHub it is the numeric user ID, which stays
-- when they rename their login. One identity per provider and account.
CREATE TABLE identities (
  provider TEXT NOT NULL CHECK (provider IN ('github')),
  subject TEXT NOT NULL,
  account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
  linked_at INTEGER NOT NULL,
  PRIMARY KEY (provider, subject),
  UNIQUE (account_id, provider)
);

-- Every account so far is a GitHub account, its ID that of its GitHub user.
INSERT INTO identities (provider, subject, account_id, linked_at)
  SELECT 'github', CAST(id AS TEXT), id, created_at FROM accounts;

ALTER TABLE sessions RENAME COLUMN github_id TO account_id;
ALTER TABLE user_flags RENAME COLUMN github_id TO account_id;
ALTER TABLE devices RENAME COLUMN github_id TO account_id;
ALTER TABLE members RENAME COLUMN github_id TO account_id;
ALTER TABLE pending RENAME COLUMN github_id TO account_id;
ALTER TABLE machines RENAME COLUMN github_id TO account_id;
ALTER TABLE notices RENAME COLUMN github_id TO account_id;
ALTER TABLE notice_reads RENAME COLUMN github_id TO account_id;
ALTER TABLE cloud_access_requests RENAME COLUMN github_id TO account_id;
ALTER TABLE admin_log RENAME COLUMN github_id TO account_id;
ALTER TABLE beta_signups RENAME COLUMN github_id TO account_id;
ALTER TABLE linear_links RENAME COLUMN github_id TO account_id;
ALTER TABLE linear_routes RENAME COLUMN github_id TO account_id;
ALTER TABLE linear_deliveries RENAME COLUMN github_id TO account_id;
ALTER TABLE cloud_machines RENAME COLUMN github_id TO account_id;

-- Left as they are, because they hold GitHub IDs on purpose:
-- `invites.github_id` names the GitHub user invited, who may have no account
-- yet. `operator_totp.github_id` and `admin_log.operator_id` name admin panel
-- operators, who sign in to the panel with GitHub and need no account.
-- `teams.created_by`, `pending.invited_by`, `invites.invited_by`,
-- `channels.created_by` and `linear_workspaces.installed_by` keep their
-- values, which are account IDs now.
