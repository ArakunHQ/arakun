-- What an invitee did with a team's invites, so a team cannot invite the same
-- person again and again. Keyed by the invitee's GitHub ID, as invites are.
--
-- `declined_at` is their last decline: the team may not invite them again for
-- a week after it. `muted` is 1 when they asked not to see the team's invites
-- at all; the team may not invite them until they undo it, which drops the row.
CREATE TABLE invite_refusals (
  team_id TEXT NOT NULL REFERENCES teams (team_id) ON DELETE CASCADE,
  github_id INTEGER NOT NULL,
  declined_at INTEGER NOT NULL,
  muted INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (team_id, github_id)
);

CREATE INDEX invite_refusals_by_user ON invite_refusals (github_id);
