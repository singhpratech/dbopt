-- Deep live-vitals time-series, captured via read-only DMVs so the sentinel
-- can answer "is the server under pressure RIGHT NOW" the way an experienced
-- DBA would — not just query-level regressions. Each surface gets its own
-- table so one bad poller can't corrupt the others, mirroring 0001_init.
--
-- All tables key off (instance_id, captured_at) with captured_at as unix epoch
-- millis, identical to every other time-series table here.

-- CPU / scheduler pressure: runnable_tasks (workers ready but waiting for a
-- CPU) and work_queue (pending tasks with no worker) summed over the VISIBLE
-- ONLINE schedulers. Sustained non-zero runnable_tasks is the classic
-- "CPU PRESSURE" signal.
CREATE TABLE IF NOT EXISTS cpu_pressure_snapshot (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id          INTEGER NOT NULL REFERENCES instances(id),
    captured_at          INTEGER NOT NULL,
    online_schedulers    INTEGER NOT NULL,
    runnable_tasks       INTEGER NOT NULL,
    work_queue           INTEGER NOT NULL,
    current_workers      INTEGER NOT NULL,
    active_workers       INTEGER NOT NULL,
    pending_disk_io      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_cpu_pressure_inst_time
    ON cpu_pressure_snapshot(instance_id, captured_at);

-- Memory headroom: Page Life Expectancy (seconds buffer pages survive without
-- reference — low + falling = buffer pool churn) plus pending memory grants
-- (queries waiting for a workspace grant = RESOURCE_SEMAPHORE pressure).
CREATE TABLE IF NOT EXISTS memory_headroom_snapshot (
    id                       INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id              INTEGER NOT NULL REFERENCES instances(id),
    captured_at              INTEGER NOT NULL,
    page_life_expectancy     INTEGER NOT NULL,
    pending_memory_grants    INTEGER NOT NULL,
    granted_memory_kb        INTEGER NOT NULL,
    target_server_memory_kb  INTEGER NOT NULL,
    total_server_memory_kb   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_mem_headroom_inst_time
    ON memory_headroom_snapshot(instance_id, captured_at);

-- File IO latency: per-tick DELTAS of sys.dm_io_virtual_file_stats (the DMV is
-- cumulative since restart). We persist the avg ms-per-read / ms-per-write for
-- the window so a spike in storage latency is visible after the fact. One row
-- per (database_name, file_logical_name) that saw IO in the window.
CREATE TABLE IF NOT EXISTS io_latency_delta (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id         INTEGER NOT NULL REFERENCES instances(id),
    captured_at         INTEGER NOT NULL,
    database_name       TEXT NOT NULL,
    file_logical_name   TEXT NOT NULL,
    file_type           TEXT NOT NULL,
    reads_delta         INTEGER NOT NULL,
    writes_delta        INTEGER NOT NULL,
    read_stall_ms_delta INTEGER NOT NULL,
    write_stall_ms_delta INTEGER NOT NULL,
    avg_read_latency_ms  REAL NOT NULL,
    avg_write_latency_ms REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_io_latency_inst_time
    ON io_latency_delta(instance_id, captured_at);

-- tempdb contention: live PAGELATCH_* waits on tempdb allocation pages
-- (PFS / GAM / SGAM — page ids 1:1, 1:2, 1:3 and their multiples). A non-zero
-- count signals classic tempdb allocation contention.
CREATE TABLE IF NOT EXISTS tempdb_contention_snapshot (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id         INTEGER NOT NULL REFERENCES instances(id),
    captured_at         INTEGER NOT NULL,
    pagelatch_waiters   INTEGER NOT NULL,
    pfs_waiters         INTEGER NOT NULL,
    gam_waiters         INTEGER NOT NULL,
    sgam_waiters        INTEGER NOT NULL,
    total_wait_ms       INTEGER NOT NULL,
    tempdb_data_files   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_tempdb_inst_time
    ON tempdb_contention_snapshot(instance_id, captured_at);

-- Plan-cache health: count + size of single-use ad-hoc compiled plans. A cache
-- dominated by single-use ad-hoc plans wastes memory and signals missing
-- parameterization.
CREATE TABLE IF NOT EXISTS plan_cache_snapshot (
    id                     INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id            INTEGER NOT NULL REFERENCES instances(id),
    captured_at            INTEGER NOT NULL,
    single_use_plan_count  INTEGER NOT NULL,
    single_use_size_kb     INTEGER NOT NULL,
    total_plan_count       INTEGER NOT NULL,
    total_size_kb          INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_plan_cache_inst_time
    ON plan_cache_snapshot(instance_id, captured_at);
