-- When no computer takes an issue's team or project, Arakun asks in Linear
-- which project to use.
--
-- `linear_repositories` holds the GitHub repositories (`owner/name`) of the
-- projects on each computer that takes work from Linear, sent with its
-- routes. They are the choices: a repository on a computer.
--
-- `linear_picks` is a question asked in a session and not answered yet:
-- the choices offered, as JSON `[{ "label": …, "repository": … }]`, in the
-- order shown. The row goes when the person picks.
--
-- `linear_deliveries.repository` is the repository picked for a delegation,
-- so the computer starts in that project. Null when a route took it.
CREATE TABLE linear_repositories (
  label TEXT NOT NULL REFERENCES machines (label) ON DELETE CASCADE,
  account_id INTEGER NOT NULL,
  organization_id TEXT NOT NULL,
  repository TEXT NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (label, organization_id, repository)
);
CREATE INDEX linear_repositories_by_account ON linear_repositories (organization_id, account_id);

CREATE TABLE linear_picks (
  agent_session_id TEXT PRIMARY KEY,
  organization_id TEXT NOT NULL,
  issue_id TEXT NOT NULL,
  account_id INTEGER NOT NULL,
  team_id TEXT NOT NULL,
  choices TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

ALTER TABLE linear_deliveries ADD COLUMN repository TEXT;
