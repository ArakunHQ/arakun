-- What the operator changed, and who did it. Append-only: the Worker has no
-- route that edits or deletes a row. Times are Unix seconds.
CREATE TABLE admin_log (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  at INTEGER NOT NULL,
  -- Null for a call made with the secret alone (curl).
  operator_id INTEGER,
  operator_login TEXT,
  action TEXT NOT NULL,
  -- The account the change is about. No foreign key: the record outlives the account.
  github_id INTEGER,
  detail TEXT
);

-- One account's history, newest first.
CREATE INDEX admin_log_by_account ON admin_log (github_id, id);
