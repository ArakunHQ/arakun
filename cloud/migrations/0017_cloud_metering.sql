-- Awake time of an account's cloud machines (src/cloud_meter.ts): one row per
-- account and UTC month, such as '2026-11', in milliseconds. The clock runs
-- while a container is up and stops while it sleeps on a snapshot. A month
-- with no row has used nothing.
CREATE TABLE cloud_usage (
  account_id INTEGER NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
  month TEXT NOT NULL,
  awake_ms INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (account_id, month)
);

-- The operator's switch for one account: its cloud machines stop now and do
-- not start again until the row is gone. The `cloud` flag stays, so lifting
-- it needs no new request. Times are Unix seconds.
CREATE TABLE cloud_holds (
  account_id INTEGER PRIMARY KEY REFERENCES accounts (id) ON DELETE CASCADE,
  reason TEXT,
  held_at INTEGER NOT NULL
);
