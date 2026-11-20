-- The invitee's numeric GitHub ID, looked up with GitHub when the invite is
-- made. Invites are matched by it, not by login: a login can pass to another
-- account, and the login we hold for a user only changes when they sign in
-- again. Invites made before this column only name a login, so they are
-- dropped; they expire within 7 days anyway, and an admin can invite again.
ALTER TABLE invites ADD COLUMN github_id INTEGER;
DELETE FROM invites WHERE github_id IS NULL;
DROP INDEX invites_by_login;
CREATE INDEX invites_by_user ON invites (github_id);
