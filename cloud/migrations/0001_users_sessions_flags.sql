-- Times are Unix seconds.

-- Keyed by the numeric GitHub ID, which stays when a user renames their
-- account. Teams and members will point here. A row with an empty login and
-- last_seen_at 0 was given a flag before its first sign-in.
CREATE TABLE users (
  github_id INTEGER PRIMARY KEY,
  login TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  last_seen_at INTEGER NOT NULL
);

-- Only the SHA-256 of a session token: a copy of the database cannot sign in.
CREATE TABLE sessions (
  token_hash TEXT PRIMARY KEY,
  github_id INTEGER NOT NULL REFERENCES users (github_id) ON DELETE CASCADE,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  last_used_at INTEGER NOT NULL
);

-- Sign-in drops the user's expired sessions.
CREATE INDEX sessions_by_user ON sessions (github_id);

-- The primary key also serves the lookup of one user's flags.
CREATE TABLE user_flags (
  github_id INTEGER NOT NULL REFERENCES users (github_id) ON DELETE CASCADE,
  flag TEXT NOT NULL,
  granted_at INTEGER NOT NULL,
  PRIMARY KEY (github_id, flag)
);
