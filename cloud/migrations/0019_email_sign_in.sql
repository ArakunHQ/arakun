-- Sign-in with a six-digit code mailed to the address (src/email_auth.ts).
--
-- `identities` takes a second provider, 'email', whose subject is the address
-- in lower case. SQLite cannot change a CHECK in place, so the table is made
-- again. Nothing points at it, so dropping the old one cascades nothing.
CREATE TABLE identities_new (
  provider TEXT NOT NULL CHECK (provider IN ('github', 'email')),
  subject TEXT NOT NULL,
  account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
  linked_at INTEGER NOT NULL,
  PRIMARY KEY (provider, subject),
  UNIQUE (account_id, provider)
);
INSERT INTO identities_new (provider, subject, account_id, linked_at)
  SELECT provider, subject, account_id, linked_at FROM identities;
DROP TABLE identities;
ALTER TABLE identities_new RENAME TO identities;

-- An email sign-in looks for the one account whose verified GitHub address
-- it is, so that person signs in to the account they have.
CREATE INDEX accounts_email ON accounts (email COLLATE NOCASE);

-- One row per address that asked for a code. Only the code's SHA-256 is kept,
-- and NULL once it was used. `sends` counts the codes mailed since
-- `window_start`, which limits the mail to one address and with it the
-- guesses at its codes. A row goes once its code and its window are both over.
CREATE TABLE email_codes (
  address TEXT PRIMARY KEY,
  code_hash TEXT,
  expires_at INTEGER NOT NULL,
  tries INTEGER NOT NULL DEFAULT 0,
  sends INTEGER NOT NULL,
  window_start INTEGER NOT NULL
);
CREATE INDEX email_codes_window ON email_codes (window_start);
