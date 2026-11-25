-- Requests for cloud environments. Times are Unix seconds.
--
-- One row per GitHub account, so one account holds one request and one grant.
-- The `cloud` flag decides access; this row is the record of what was asked
-- and decided. A new request after a decline, or after a revoke, writes over
-- the row.
--
-- The gh_* columns are what GitHub's public profile said about the account
-- when it asked, for the reviewer. They are null when GitHub did not answer:
-- the request is kept anyway. Whether the account has 2FA is not among them:
-- GitHub tells only the account itself, and sign-in revokes its token.
CREATE TABLE cloud_access_requests (
  github_id INTEGER PRIMARY KEY REFERENCES users (github_id) ON DELETE CASCADE,
  status TEXT NOT NULL CHECK (status IN ('pending', 'granted', 'declined')),
  requested_at INTEGER NOT NULL,
  decided_at INTEGER,
  -- Who decided, as the operator wrote it: the admin door has no accounts.
  decided_by TEXT CHECK (length(decided_by) <= 100),
  -- Why the person asks, in their words.
  reason TEXT CHECK (length(reason) <= 500),
  gh_login TEXT NOT NULL,
  gh_created_at INTEGER,
  gh_public_repos INTEGER,
  gh_followers INTEGER,
  gh_following INTEGER,
  -- A pending request has no decision yet; a decided one has its time.
  CHECK ((status = 'pending') = (decided_at IS NULL))
);

-- The operator's list: one status, oldest first.
CREATE INDEX cloud_access_requests_by_status ON cloud_access_requests (status, requested_at);
