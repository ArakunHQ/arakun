-- A Linear refresh token lasts 30 days from its last use, and the Worker
-- used it only when a workspace was busy. A daily job now refreshes the
-- tokens of a quiet workspace (src/linear_keep.ts).
--
-- `refreshed_at` is when the pair was last issued, the install or a refresh.
-- Rows from before it take their install time: a job refreshes them first.
-- `refused_at` is when Linear refused the refresh token, as it does once the
-- app is removed or the token ran out. Only installing again mends that, and
-- it clears the column. The Linear tab then shows the workspace as not
-- installed.
ALTER TABLE linear_workspaces ADD COLUMN refreshed_at INTEGER NOT NULL DEFAULT 0;
UPDATE linear_workspaces SET refreshed_at = installed_at;
ALTER TABLE linear_workspaces ADD COLUMN refused_at INTEGER;
