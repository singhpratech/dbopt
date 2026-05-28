-- sentinel initial schema
-- All time-series tables key off `instance_id` and `captured_at` (unix epoch
-- millis stored as INTEGER). Each surface has its own table so a single bad
-- poller can't corrupt the others.

CREATE TABLE IF NOT EXISTS instances (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL UNIQUE,
    server      TEXT NOT NULL,
    db          TEXT,
    auth_mode   TEXT NOT NULL,
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_at  INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS query_store_snapshot (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id       INTEGER NOT NULL REFERENCES instances(id),
    captured_at       INTEGER NOT NULL,
    query_id          INTEGER NOT NULL,
    plan_id           INTEGER NOT NULL,
    total_duration_ms INTEGER NOT NULL,
    cpu_ms            INTEGER NOT NULL,
    logical_reads     INTEGER NOT NULL,
    executions        INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_qss_inst_time
    ON query_store_snapshot(instance_id, captured_at);

CREATE TABLE IF NOT EXISTS live_request_snapshot (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id         INTEGER NOT NULL REFERENCES instances(id),
    captured_at         INTEGER NOT NULL,
    session_id          INTEGER NOT NULL,
    request_id          INTEGER NOT NULL,
    duration_ms         INTEGER NOT NULL,
    blocking_session_id INTEGER,
    wait_type           TEXT,
    sql_text_hash       TEXT,
    sql_text_preview    TEXT
);
CREATE INDEX IF NOT EXISTS idx_lrs_inst_time
    ON live_request_snapshot(instance_id, captured_at);

CREATE TABLE IF NOT EXISTS wait_stats_delta (
    id                          INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id                 INTEGER NOT NULL REFERENCES instances(id),
    captured_at                 INTEGER NOT NULL,
    wait_type                   TEXT NOT NULL,
    waiting_tasks_count_delta   INTEGER NOT NULL,
    wait_time_ms_delta          INTEGER NOT NULL,
    signal_wait_ms_delta        INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_wsd_inst_time
    ON wait_stats_delta(instance_id, captured_at);

CREATE TABLE IF NOT EXISTS deadlock_capture (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id        INTEGER NOT NULL REFERENCES instances(id),
    captured_at        INTEGER NOT NULL,
    xml_blob           TEXT NOT NULL,
    victim_session_id  INTEGER,
    victim_resource    TEXT
);
CREATE INDEX IF NOT EXISTS idx_dc_inst_time
    ON deadlock_capture(instance_id, captured_at);

CREATE TABLE IF NOT EXISTS index_usage_delta (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id   INTEGER NOT NULL REFERENCES instances(id),
    captured_at   INTEGER NOT NULL,
    db_name       TEXT NOT NULL,
    schema_name   TEXT NOT NULL,
    table_name    TEXT NOT NULL,
    index_name    TEXT NOT NULL,
    seeks_delta   INTEGER NOT NULL,
    scans_delta   INTEGER NOT NULL,
    lookups_delta INTEGER NOT NULL,
    updates_delta INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_iud_inst_time
    ON index_usage_delta(instance_id, captured_at);

CREATE TABLE IF NOT EXISTS size_snapshot (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id  INTEGER NOT NULL REFERENCES instances(id),
    captured_at  INTEGER NOT NULL,
    schema_name  TEXT NOT NULL,
    table_name   TEXT NOT NULL,
    index_name   TEXT,
    reserved_kb  INTEGER NOT NULL,
    used_kb      INTEGER NOT NULL,
    data_kb      INTEGER NOT NULL,
    row_count    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ss_inst_time
    ON size_snapshot(instance_id, captured_at);

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
