-- The community widget index (docs/superpowers/specs/2026-11-27-community-widgets-design.md).
--
-- `widgets` is a listing: `id` is `<author>/<name>`. `installs` counts the
-- installs the app reported. `pulled_reason` set: the whole widget is pulled.
--
-- `widget_versions` is one version of a listing, pinned to a commit and the
-- hash of its files. `files` is JSON with the text of `widget.toml`,
-- `main.rhai` and `sample.json`, so the Marketplace shows the needs and the
-- preview without fetching the repository. `signature` is Arakun's minisign
-- signature of the version (Verified); null is Not verified.
-- `pulled_reason` set: that version is pulled.
--
-- `widget_reports` is a Report from the app, for a person at Arakun to
-- decide. `closed_at` is set once they did.
CREATE TABLE widgets (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  about TEXT NOT NULL,
  license TEXT NOT NULL,
  repository TEXT NOT NULL,
  installs INTEGER NOT NULL DEFAULT 0,
  pulled_reason TEXT,
  updated_at INTEGER NOT NULL
);

CREATE TABLE widget_versions (
  widget_id TEXT NOT NULL REFERENCES widgets (id) ON DELETE CASCADE,
  version TEXT NOT NULL,
  commit_sha TEXT NOT NULL,
  published_at INTEGER NOT NULL,
  hash TEXT NOT NULL,
  files TEXT NOT NULL,
  signature TEXT,
  pulled_reason TEXT,
  PRIMARY KEY (widget_id, version)
);

CREATE TABLE widget_reports (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  widget_id TEXT NOT NULL,
  version TEXT NOT NULL,
  reason TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  closed_at INTEGER,
  closed_by TEXT
);
CREATE INDEX widget_reports_open ON widget_reports (closed_at, created_at);
