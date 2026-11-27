-- Linear can end a session without its stop button: Arakun is unassigned,
-- the issue is closed, or Arakun loses the team or the workspace. The Worker
-- then sends the computer a stop of its own, and `reason` says why, so the
-- computer can tell Linear. It is null for a stop a person pressed.
--
-- `team_id` is the issue's team when it was delegated, so a team Arakun can
-- no longer see still finds its sessions. Null on rows from before it.
ALTER TABLE linear_deliveries ADD COLUMN reason TEXT
  CHECK (reason IS NULL OR reason IN ('unassigned', 'canceled', 'completed', 'team_access', 'revoked'));
ALTER TABLE linear_deliveries ADD COLUMN team_id TEXT;
CREATE INDEX linear_deliveries_by_issue ON linear_deliveries (organization_id, issue_id, kind);
