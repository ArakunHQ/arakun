-- Devices, teams, members, invites, envelopes and channels: see PROTOCOL.md.
-- Messages are not here: each channel keeps its own in a Durable Object.
-- Binary values are base64url text, as the clients send them. Times are Unix
-- seconds.

-- device_id is derived from sig_pub, so one key is one device everywhere.
-- endorsed_by is null only for a user's first device.
CREATE TABLE devices (
  device_id TEXT PRIMARY KEY,
  github_id INTEGER NOT NULL REFERENCES users (github_id) ON DELETE CASCADE,
  sig_pub TEXT NOT NULL UNIQUE,
  enc_pub TEXT NOT NULL,
  name TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  endorsed_by TEXT REFERENCES devices (device_id)
);

CREATE INDEX devices_by_user ON devices (github_id);

-- keyed_epoch is the newest epoch with envelopes for every member's device.
-- It is behind epoch while a rotation is pending; 0 until the first key.
CREATE TABLE teams (
  team_id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  epoch INTEGER NOT NULL,
  keyed_epoch INTEGER NOT NULL,
  created_by INTEGER NOT NULL REFERENCES users (github_id),
  created_at INTEGER NOT NULL
);

-- Only members who were approved. Pending ones live in their own table, so a
-- check for membership cannot let one through by forgetting a role.
CREATE TABLE members (
  team_id TEXT NOT NULL REFERENCES teams (team_id) ON DELETE CASCADE,
  github_id INTEGER NOT NULL REFERENCES users (github_id) ON DELETE CASCADE,
  role TEXT NOT NULL CHECK (role IN ('owner', 'admin', 'member')),
  joined_at INTEGER NOT NULL,
  PRIMARY KEY (team_id, github_id)
);

CREATE INDEX members_by_user ON members (github_id);

CREATE TABLE pending (
  team_id TEXT NOT NULL REFERENCES teams (team_id) ON DELETE CASCADE,
  github_id INTEGER NOT NULL REFERENCES users (github_id) ON DELETE CASCADE,
  invited_by INTEGER NOT NULL,
  requested_at INTEGER NOT NULL,
  PRIMARY KEY (team_id, github_id)
);

-- login is lower case: GitHub logins ignore case. One open invite per login
-- and team; inviting again replaces it.
CREATE TABLE invites (
  invite_id TEXT PRIMARY KEY,
  team_id TEXT NOT NULL REFERENCES teams (team_id) ON DELETE CASCADE,
  login TEXT NOT NULL,
  invited_by INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  UNIQUE (team_id, login)
);

CREATE INDEX invites_by_login ON invites (login);

-- One team key for one device and epoch. body is the envelope as JSON.
CREATE TABLE envelopes (
  team_id TEXT NOT NULL REFERENCES teams (team_id) ON DELETE CASCADE,
  device_id TEXT NOT NULL REFERENCES devices (device_id) ON DELETE CASCADE,
  epoch INTEGER NOT NULL,
  body TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (team_id, device_id, epoch)
);

-- name_ct is the Sealed name as JSON; the server cannot read it.
CREATE TABLE channels (
  team_id TEXT NOT NULL REFERENCES teams (team_id) ON DELETE CASCADE,
  channel_id TEXT NOT NULL,
  name_ct TEXT NOT NULL,
  created_by INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (team_id, channel_id)
);
