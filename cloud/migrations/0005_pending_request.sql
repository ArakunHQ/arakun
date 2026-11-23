-- Which request an admin reviewed: the ID of the invite the user accepted, and
-- the login that invite was for. An approval names the request, so one made
-- after a reject and a new accept does not admit the new request. Requests made
-- before this column get a random ID; the admin's client reads it like any.
ALTER TABLE pending ADD COLUMN request_id TEXT NOT NULL DEFAULT '';
ALTER TABLE pending ADD COLUMN invited_login TEXT NOT NULL DEFAULT '';
UPDATE pending SET request_id = lower(hex(randomblob(16))) WHERE request_id = '';
