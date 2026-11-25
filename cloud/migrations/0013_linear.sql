-- Linear delegation: Linear hands an issue to Arakun, and the Worker passes it
-- on to the user's computer through the relay. Times are Unix seconds.
--
-- No text from Linear is stored: only ids, tokens (sealed with AES-GCM under
-- LINEAR_TOKEN_KEY), times and machine labels. The issue itself is fetched
-- from Linear again when it is needed.

-- One row per Linear workspace that installed the app. The tokens are the
-- app's own (actor=app), sealed with the organization id as additional data.
CREATE TABLE linear_workspaces (
  organization_id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  app_user_id TEXT NOT NULL,
  access_token TEXT NOT NULL,
  refresh_token TEXT NOT NULL,
  expires_at INTEGER NOT NULL,
  installed_by INTEGER NOT NULL REFERENCES users (github_id),
  installed_at INTEGER NOT NULL
);

-- Which GitHub account a Linear user is. A delegated issue goes to the
-- computers of the account its delegator is linked to.
CREATE TABLE linear_links (
  organization_id TEXT NOT NULL,
  linear_user_id TEXT NOT NULL,
  github_id INTEGER NOT NULL REFERENCES users (github_id) ON DELETE CASCADE,
  linked_at INTEGER NOT NULL,
  PRIMARY KEY (organization_id, linear_user_id)
);
CREATE INDEX linear_links_by_account ON linear_links (github_id);

-- Which computer takes the issues of a Linear team or project. The computer
-- reports it; the row goes with the machine.
CREATE TABLE linear_routes (
  label TEXT NOT NULL REFERENCES machines (label) ON DELETE CASCADE,
  github_id INTEGER NOT NULL,
  organization_id TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('team', 'project')),
  target_id TEXT NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (label, organization_id, kind, target_id)
);
CREATE INDEX linear_routes_by_target ON linear_routes (organization_id, github_id, target_id);

-- One row per webhook that became work for a computer. `webhook_id` makes a
-- redelivery by Linear a no-op. The three times say how far it got: sent to
-- the computer, accepted by it, or given up on.
CREATE TABLE linear_deliveries (
  id TEXT PRIMARY KEY,
  webhook_id TEXT NOT NULL UNIQUE,
  agent_session_id TEXT NOT NULL,
  organization_id TEXT NOT NULL,
  issue_id TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('created', 'prompt', 'stop')),
  activity_id TEXT,
  github_id INTEGER NOT NULL,
  label TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  announced_at INTEGER,
  accepted_at INTEGER,
  expired_at INTEGER
);
CREATE INDEX linear_deliveries_by_session ON linear_deliveries (agent_session_id, kind);
