-- Each admin panel operator's authenticator key, for the second step of the
-- panel's sign-in (admin/worker/totp.ts). Times are Unix seconds.
--
-- `sealed` is the key encrypted by the admin Worker (AES-GCM, with a key only
-- that Worker can derive), so this table alone cannot make a code.
-- `last_step` is the 30-second step of the last code that was taken, so no
-- code is taken twice and none older than it at all.
--
-- No foreign key to users: an operator need not have signed in to the app.
CREATE TABLE operator_totp (
  github_id INTEGER PRIMARY KEY,
  sealed TEXT NOT NULL CHECK (length(sealed) <= 200),
  last_step INTEGER NOT NULL,
  created_at INTEGER NOT NULL
);
