-- Linking and unlinking sign-in methods on an account (src/sign_in_methods.ts).
--
-- A code mailed to add an address to an account works only for that account,
-- and a sign-in code never adds an address: NULL is a sign-in code. A code of
-- either kind replaces the address's code before it, and both count against
-- the same sends.
ALTER TABLE email_codes ADD COLUMN for_account INTEGER;

-- Every sign-in method added to or removed from an account, and by which
-- session. Append-only. No foreign key: the record outlives the account and
-- the session.
CREATE TABLE sign_in_changes (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  account_id INTEGER NOT NULL,
  at INTEGER NOT NULL,
  action TEXT NOT NULL CHECK (action IN ('link', 'unlink')),
  provider TEXT NOT NULL,
  subject TEXT NOT NULL,
  session_id TEXT
);

-- One account's changes, newest first.
CREATE INDEX sign_in_changes_by_account ON sign_in_changes (account_id, id);
