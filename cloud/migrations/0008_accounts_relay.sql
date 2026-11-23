-- Accounts own the relay: sessions get a public id, a name and a kind, users get
-- a plan, a block and an email, and machines, notices and their reads are new.
-- Times are Unix seconds.

-- SQLite cannot add a unique column, so the id is added, filled for the old
-- rows, and then made unique by an index.
ALTER TABLE sessions ADD COLUMN id TEXT;
UPDATE sessions SET id = lower(hex(randomblob(16))) WHERE id IS NULL;
CREATE UNIQUE INDEX sessions_by_id ON sessions (id);
ALTER TABLE sessions ADD COLUMN name TEXT NOT NULL DEFAULT '';
ALTER TABLE sessions ADD COLUMN kind TEXT NOT NULL DEFAULT 'app';
-- Old sessions are named after their user's login.
UPDATE sessions SET name = (SELECT login FROM users WHERE users.github_id = sessions.github_id);

ALTER TABLE users ADD COLUMN plan TEXT NOT NULL DEFAULT 'free';
ALTER TABLE users ADD COLUMN blocked_at INTEGER;
ALTER TABLE users ADD COLUMN blocked_reason TEXT;
-- The primary verified address GitHub gave at sign-in; null if it gave none.
ALTER TABLE users ADD COLUMN email TEXT;

-- A computer's label belongs to the account that claimed it first.
CREATE TABLE machines (
  label TEXT PRIMARY KEY,
  github_id INTEGER NOT NULL REFERENCES users (github_id) ON DELETE CASCADE,
  pub TEXT NOT NULL,
  name TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('app', 'server')),
  created_at INTEGER NOT NULL,
  last_seen_at INTEGER NOT NULL
);
CREATE INDEX machines_by_user ON machines (github_id);

-- github_id null: for everyone.
CREATE TABLE notices (
  id TEXT PRIMARY KEY,
  github_id INTEGER REFERENCES users (github_id) ON DELETE CASCADE,
  text TEXT NOT NULL,
  url TEXT,
  created_at INTEGER NOT NULL,
  expires_at INTEGER
);
CREATE INDEX notices_by_user ON notices (github_id);

CREATE TABLE notice_reads (
  github_id INTEGER NOT NULL REFERENCES users (github_id) ON DELETE CASCADE,
  notice_id TEXT NOT NULL REFERENCES notices (id) ON DELETE CASCADE,
  PRIMARY KEY (github_id, notice_id)
);
