-- A random ID of the upload that made the newest key, set in the same update
-- as keyed_epoch. The upload's envelopes are written only if it is theirs, so
-- an upload that did not make the key writes nothing.
ALTER TABLE teams ADD COLUMN key_upload TEXT;
