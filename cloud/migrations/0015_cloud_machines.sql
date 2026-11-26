-- The machines we run for an account: one container each, with `arakun-server`
-- in it (src/cloud_machine.ts). The label is the machine's relay label; its key
-- is kept by the machine's own object, never here. The row is made before the
-- container first claims the label, so `machines` may not have it yet. Times
-- are Unix seconds.
CREATE TABLE cloud_machines (
  label TEXT PRIMARY KEY,
  github_id INTEGER NOT NULL REFERENCES users (github_id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE INDEX cloud_machines_by_user ON cloud_machines (github_id);
