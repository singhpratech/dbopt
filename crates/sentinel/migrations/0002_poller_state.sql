-- Per-instance, per-key blob store for stateful pollers (wait-stats deltas,
-- index-usage deltas, last-deadlock-hash). Value is JSON. Keeping it generic
-- so future pollers don't need a schema change.

CREATE TABLE IF NOT EXISTS poller_state (
    instance_id   INTEGER NOT NULL REFERENCES instances(id),
    key           TEXT NOT NULL,
    value         TEXT NOT NULL,
    updated_at    INTEGER NOT NULL,
    PRIMARY KEY (instance_id, key)
);
