-- Missing-index DMV snapshots. sys.dm_db_missing_index_* is wiped on every
-- instance restart, failover, or DDL against the table, so live advice built
-- on it evaporates. A daily snapshot lets the advisor say "this suggestion was
-- seen on N of the last M monitored days" instead of presenting one reading
-- as a verdict. Rows are stored as-is (no delta): the DMV is already a rollup.
CREATE TABLE IF NOT EXISTS missing_index_snapshot (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id          INTEGER NOT NULL REFERENCES instances(id),
    captured_at          INTEGER NOT NULL,
    db_name              TEXT NOT NULL,
    schema_name          TEXT NOT NULL,
    table_name           TEXT NOT NULL,
    equality_columns     TEXT NOT NULL,
    inequality_columns   TEXT NOT NULL,
    included_columns     TEXT NOT NULL,
    user_seeks           INTEGER NOT NULL,
    avg_user_impact      REAL NOT NULL,
    avg_total_user_cost  REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_missing_index_inst_time
    ON missing_index_snapshot(instance_id, captured_at);
