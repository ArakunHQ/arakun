-- When a pending user last reminded the team's admins of their request, so
-- they can do it at most once an hour. Null until the first reminder.
ALTER TABLE pending ADD COLUMN reminded_at INTEGER;
