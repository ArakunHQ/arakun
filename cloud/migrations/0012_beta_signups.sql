-- Sign-ups for the phone app betas (TestFlight for iOS, Play testing for
-- Android). Times are Unix seconds.
--
-- One row per GitHub account and platform. The operator admits by hand: they
-- add `email` as a tester in App Store Connect or Play Console, then mark the
-- row admitted. A new sign-up after a decline writes over the row.
--
-- `email` is the address to invite: an Apple ID for TestFlight, a Google
-- account for Play. The gh_* columns are what GitHub's public profile said
-- about the account when it signed up, for the reviewer; null when GitHub did
-- not answer.
CREATE TABLE beta_signups (
  github_id INTEGER NOT NULL REFERENCES users (github_id) ON DELETE CASCADE,
  platform TEXT NOT NULL CHECK (platform IN ('ios', 'android')),
  email TEXT NOT NULL CHECK (length(email) <= 254),
  status TEXT NOT NULL CHECK (status IN ('pending', 'admitted', 'declined')),
  requested_at INTEGER NOT NULL,
  decided_at INTEGER,
  -- Who decided, as the operator wrote it: the admin door has no accounts.
  decided_by TEXT CHECK (length(decided_by) <= 100),
  gh_login TEXT NOT NULL,
  gh_created_at INTEGER,
  gh_public_repos INTEGER,
  gh_followers INTEGER,
  gh_following INTEGER,
  -- A pending sign-up has no decision yet; a decided one has its time.
  CHECK ((status = 'pending') = (decided_at IS NULL)),
  PRIMARY KEY (github_id, platform)
);

-- The operator's list: one status, oldest first.
CREATE INDEX beta_signups_by_status ON beta_signups (status, requested_at);
