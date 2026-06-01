//! rusqlite wrapper used by the sentinel daemon.
//!
//! Schema lives in `migrations/*.sql` and is embedded into the binary with
//! `include_str!` so deployments only need to ship one file. We hold the
//! `Connection` behind a `Mutex` because SQLite is single-writer; readers
//! that need real concurrency can open their own read-only handle.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use anyhow::Context;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::ConnectionInfo;

/// All migration scripts, in the order they must run. Embedded at compile
/// time so the binary is self-contained.
const MIGRATIONS: &[(&str, &str)] = &[
    ("0001_init",          include_str!("../migrations/0001_init.sql")),
    ("0002_poller_state",  include_str!("../migrations/0002_poller_state.sql")),
    ("0003_logs",          include_str!("../migrations/0003_logs.sql")),
    ("0004_query_text",    include_str!("../migrations/0004_query_text.sql")),
    ("0005_deadlock_graph_hash", include_str!("../migrations/0005_deadlock_graph_hash.sql")),
    ("0006_query_baseline", include_str!("../migrations/0006_query_baseline.sql")),
    ("0007_vitals",         include_str!("../migrations/0007_vitals.sql")),
];

const SCHEMA_VERSION_KEY: &str = "schema_version";

/// Half-open `[from, to)` time window. Stored as UTC.
#[derive(Debug, Clone, Copy)]
pub struct TimeRange {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

impl TimeRange {
    pub fn last_hours(h: i64) -> Self {
        let to = Utc::now();
        let from = to - chrono::Duration::hours(h);
        Self { from, to }
    }
    pub fn last_days(d: i64) -> Self {
        let to = Utc::now();
        let from = to - chrono::Duration::days(d);
        Self { from, to }
    }
    fn from_ms(&self) -> i64 { self.from.timestamp_millis() }
    fn to_ms(&self) -> i64 { self.to.timestamp_millis() }
}

/// Convert a stored `captured_at` (unix epoch millis) back to a UTC timestamp,
/// the inverse of the `timestamp_millis()` we persist with. An out-of-range
/// value (never produced by our own writers) falls back to `now` rather than
/// panicking — mirrors the same defensive conversion in `health::enrichment`.
fn ms_to_dt(ms: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp_millis(ms).unwrap_or_else(Utc::now)
}

/// One row of "top queries by duration" — what the report uses.
#[derive(Debug, Clone, Serialize)]
pub struct TopQueryRow {
    pub query_id: i64,
    pub plan_id: i64,
    pub total_duration_ms: i64,
    pub executions: i64,
    pub query_sql_text: Option<String>,
    /// Most recent execution time (unix ms) seen for this query in the window —
    /// powers the "by last run" sort. Null for pre-0004 rows without it.
    pub last_execution_ms: Option<i64>,
}

/// One row of "regression detected" — query got slower across the window.
/// `baseline_duration_ms` holds the rolling-mean per-execution baseline the
/// z-score detector compared the latest sample against.
#[derive(Debug, Clone, Serialize)]
pub struct RegressionRow {
    pub query_id: i64,
    pub baseline_duration_ms: i64,
    pub current_duration_ms: i64,
    pub delta_pct: f64,
}

/// Z-score threshold (in standard deviations) above the rolling mean that the
/// latest per-execution duration must clear to count as a regression. ~3σ is a
/// conventional outlier cutoff: it keeps false positives low while still
/// catching genuine slowdowns.
pub const REGRESSION_Z_SCORE_K: f64 = 3.0;

/// Minimum number of snapshot samples (including the current one) required
/// before the z-score is trusted. Below this we have too little history to
/// estimate a stable stddev, so the query is skipped (graceful fallback).
pub const REGRESSION_MIN_SAMPLES: usize = 6;

/// Minimum total executions across the sampled history. Guards against flagging
/// rarely-run queries whose averages swing wildly on a single slow run.
pub const REGRESSION_MIN_EXECUTIONS: i64 = 10;

/// Minimum absolute increase (current − baseline mean, in ms-per-execution)
/// before a statistical outlier is worth reporting. Filters out z-score spikes
/// on sub-millisecond queries where the relative jump is noise.
pub const REGRESSION_MIN_ABS_DELTA_MS: f64 = 5.0;

/// Apply the rolling-mean + standard-deviation (z-score) regression test to one
/// query's per-snapshot duration-per-execution series (oldest→newest).
///
/// Returns a [`RegressionRow`] only when the latest sample is a genuine outlier:
/// enough samples and executions exist, the prior history has non-zero spread,
/// the latest value exceeds `mean + REGRESSION_Z_SCORE_K * stddev`, and the
/// absolute jump clears [`REGRESSION_MIN_ABS_DELTA_MS`]. Tiny or flat samples
/// return `None` rather than being force-judged.
pub fn detect_regression(query_id: i64, points: &[(f64, i64)]) -> Option<RegressionRow> {
    if points.len() < REGRESSION_MIN_SAMPLES {
        return None;
    }
    let total_exec: i64 = points.iter().map(|(_, e)| *e).sum();
    if total_exec < REGRESSION_MIN_EXECUTIONS {
        return None;
    }
    // Compare the most-recent sample against the rolling mean/stddev of the
    // *prior* samples so the current outlier doesn't inflate its own baseline.
    let (current, _) = *points.last().unwrap();
    let prior = &points[..points.len() - 1];
    let n = prior.len() as f64;
    if n < 2.0 {
        return None;
    }
    let mean = prior.iter().map(|(d, _)| *d).sum::<f64>() / n;
    // Sample variance (Bessel's correction, n-1) for an unbiased stddev.
    let variance = prior.iter().map(|(d, _)| (d - mean).powi(2)).sum::<f64>() / (n - 1.0);
    let stddev = variance.sqrt();
    // Flat history (no variation): no statistical basis to call an outlier.
    if !(stddev > 0.0) || !mean.is_finite() {
        return None;
    }
    let z = (current - mean) / stddev;
    if z < REGRESSION_Z_SCORE_K {
        return None;
    }
    if (current - mean) < REGRESSION_MIN_ABS_DELTA_MS {
        return None;
    }
    let delta_pct = if mean > 0.0 {
        (current / mean - 1.0) * 100.0
    } else {
        0.0
    };
    Some(RegressionRow {
        query_id,
        baseline_duration_ms: mean.round() as i64,
        current_duration_ms: current.round() as i64,
        delta_pct,
    })
}

// ---------- Durable rolling baseline ---------------------------------------

/// One query's persisted rolling baseline, accumulated with Welford's online
/// algorithm so updating it never requires re-reading the whole history.
///
/// `count` samples have been folded in; `mean` is the running mean of the
/// duration-per-execution (ms) series; `m2` is the running sum of squared
/// deviations from the mean (Welford's M2). Sample variance is `m2 / (count -
/// 1)` and stddev its square root. This is what survives across polling
/// windows and process restarts — the durable answer to "what is normal for
/// this query".
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct QueryBaseline {
    pub query_id: i64,
    pub count: i64,
    pub mean: f64,
    pub m2: f64,
    /// Most recently observed duration-per-execution sample (ms).
    pub last_value_ms: f64,
}

impl QueryBaseline {
    /// Sample standard deviation (Bessel's correction). `None` until at least
    /// two samples have been folded in (otherwise variance is undefined).
    pub fn stddev(&self) -> Option<f64> {
        if self.count < 2 {
            return None;
        }
        let var = self.m2 / (self.count as f64 - 1.0);
        if var.is_finite() && var >= 0.0 {
            Some(var.sqrt())
        } else {
            None
        }
    }

    /// Fold one new duration-per-execution sample into the running stats using
    /// Welford's incremental update. Returns the updated baseline; the caller
    /// persists it. Pure + side-effect-free so it is trivially unit-testable.
    pub fn fold(mut self, sample: f64) -> Self {
        if !sample.is_finite() {
            return self;
        }
        self.count += 1;
        let delta = sample - self.mean;
        self.mean += delta / self.count as f64;
        let delta2 = sample - self.mean;
        self.m2 += delta * delta2;
        self.last_value_ms = sample;
        self
    }
}

/// Minimum number of persisted samples a durable baseline must hold before we
/// trust it to judge a new sample. Below this we don't have a stable stddev, so
/// the durable path returns `None` and the caller falls back to the in-window
/// z-score (graceful degradation, mirrors [`REGRESSION_MIN_SAMPLES`]).
pub const DURABLE_BASELINE_MIN_COUNT: i64 = 6;

/// Judge a single new `sample` (duration-per-execution, ms) against a query's
/// PERSISTED rolling baseline. This is the durable analogue of
/// [`detect_regression`]: instead of a per-window series it uses the running
/// mean/stddev accumulated across every prior poll.
///
/// Returns a [`RegressionRow`] only when the baseline holds enough history
/// ([`DURABLE_BASELINE_MIN_COUNT`]), has non-zero spread, and the new sample
/// clears `mean + REGRESSION_Z_SCORE_K * stddev` and the absolute-delta floor —
/// the same outlier gates as the in-window detector, just sourced from durable
/// state. `None` when the baseline is too thin (the caller then falls back to
/// the in-window z-score).
pub fn detect_regression_durable(baseline: &QueryBaseline, sample: f64) -> Option<RegressionRow> {
    if baseline.count < DURABLE_BASELINE_MIN_COUNT {
        return None;
    }
    let mean = baseline.mean;
    let stddev = baseline.stddev()?;
    if !(stddev > 0.0) || !mean.is_finite() {
        return None;
    }
    let z = (sample - mean) / stddev;
    if z < REGRESSION_Z_SCORE_K {
        return None;
    }
    if (sample - mean) < REGRESSION_MIN_ABS_DELTA_MS {
        return None;
    }
    let delta_pct = if mean > 0.0 {
        (sample / mean - 1.0) * 100.0
    } else {
        0.0
    };
    Some(RegressionRow {
        query_id: baseline.query_id,
        baseline_duration_ms: mean.round() as i64,
        current_duration_ms: sample.round() as i64,
        delta_pct,
    })
}

/// Aggregate pain summary for the report header.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PainSummary {
    pub top_wait_type: Option<String>,
    pub top_wait_time_ms: i64,
    pub deadlock_count: i64,
    pub blocking_incidents: i64,
}

/// Benign/idle/system wait types — noise, not user-facing pain (Paul Randal /
/// the community real-time script ignorable list). Excluded by the wait poller AND the top-wait
/// pick so the Reliability grade isn't dinged by background scheduler waits.
pub const IGNORABLE_WAITS: &[&str] = &[
    "CLR_AUTO_EVENT","CLR_MANUAL_EVENT","CLR_SEMAPHORE",
    "SLEEP_TASK","SLEEP_SYSTEMTASK","SLEEP_BPOOL_FLUSH","SLEEP_DBSTARTUP",
    "SLEEP_DCOMSTARTUP","SLEEP_MASTERDBREADY","SLEEP_MASTERMDREADY",
    "SLEEP_MASTERUPGRADED","SLEEP_MSDBSTARTUP","SLEEP_TEMPDBSTARTUP",
    "LAZYWRITER_SLEEP","BROKER_TASK_STOP","BROKER_TO_FLUSH",
    "BROKER_RECEIVE_WAITFOR","BROKER_EVENTHANDLER","BROKER_TRANSMITTER",
    "SQLTRACE_BUFFER_FLUSH","SQLTRACE_INCREMENTAL_FLUSH_SLEEP","SQLTRACE_WAIT_ENTRIES",
    "CHECKPOINT_QUEUE","REQUEST_FOR_DEADLOCK_SEARCH","LOGMGR_QUEUE",
    "XE_TIMER_EVENT","XE_DISPATCHER_WAIT","XE_DISPATCHER_JOIN","XE_LIVE_TARGET_TVF",
    "TRACEWRITE","FT_IFTS_SCHEDULER_IDLE_WAIT","FT_IFTSHC_MUTEX",
    "DISPATCHER_QUEUE_SEMAPHORE","WAITFOR","ONDEMAND_TASK_QUEUE",
    "HADR_FILESTREAM_IOMGR_IOCOMPLETION","HADR_WORK_QUEUE","HADR_TIMER_TASK",
    "HADR_CLUSAPI_CALL","HADR_LOGCAPTURE_WAIT","HADR_NOTIFICATION_DEQUEUE",
    "PREEMPTIVE_OS_WAITFOROBJECT","PREEMPTIVE_XE_GETTARGETSTATE",
    "PREEMPTIVE_XE_DISPATCHER","PREEMPTIVE_XE_TARGETINIT","PREEMPTIVE_XE_SESSIONCOMMIT",
    "PREEMPTIVE_OS_FLUSHFILEBUFFERS","PREEMPTIVE_OS_AUTHENTICATIONOPS",
    "PREEMPTIVE_OS_GETPROCADDRESS","PREEMPTIVE_OS_LIBRARYOPS",
    "DIRTY_PAGE_POLL","SP_SERVER_DIAGNOSTICS_SLEEP","SOS_WORK_DISPATCHER",
    "QDS_PERSIST_TASK_MAIN_LOOP_SLEEP","QDS_ASYNC_QUEUE","QDS_SHUTDOWN_QUEUE",
    "QDS_CLEANUP_STALE_QUERIES_TASK_MAIN_LOOP_SLEEP",
    "WAIT_XTP_OFFLINE_CKPT_NEW_LOG","WAIT_XTP_CKPT_CLOSE","WAIT_XTP_HOST_WAIT",
    "WAIT_XTP_RECOVERY","STARTUP_DEPENDENCY_MANAGER","CXCONSUMER",
    "PARALLEL_REDO_DRAIN_WORKER","PARALLEL_REDO_WORKER_WAIT_WORK",
    "PWAIT_DIRECTLOGCONSUMER_GETNEXT","PWAIT_EXTENSIBILITY_CLEANUP_TASK",
    "VDI_CLIENT_OTHER","DBMIRROR_DBM_EVENT","DBMIRROR_EVENTS_QUEUE",
    "DBMIRRORING_CMD","DBMIRROR_WORKER_QUEUE","SNI_HTTP_ACCEPT",
    "SERVER_IDLE_CHECK","RESOURCE_QUEUE","KSOURCE_WAKEUP","SLEEP_RETRY_VIRTUALALLOC",
];

/// `'A','B',...` for embedding in a SQL `NOT IN (...)` clause.
pub fn ignorable_waits_in_clause() -> String {
    IGNORABLE_WAITS
        .iter()
        .map(|w| format!("'{w}'"))
        .collect::<Vec<_>>()
        .join(",")
}

/// Index that has accumulated writes but no reads in the window.
#[derive(Debug, Clone, Serialize)]
pub struct UnusedIndexRow {
    pub db_name: String,
    pub schema_name: String,
    pub table_name: String,
    pub index_name: String,
    pub updates_in_window: i64,
}

pub struct Storage {
    conn: Mutex<Connection>,
}

impl Storage {
    /// Open (or create) the database at `path` and run pending migrations.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("opening sentinel db at {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "foreign_keys", "ON").ok();
        let storage = Self { conn: Mutex::new(conn) };
        storage.migrate()?;
        Ok(storage)
    }

    /// Open an in-memory store. Used by tests and ad-hoc CLI runs.
    pub fn open_in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.pragma_update(None, "foreign_keys", "ON").ok();
        let storage = Self { conn: Mutex::new(conn) };
        storage.migrate()?;
        Ok(storage)
    }

    /// Run every embedded migration whose id is greater than the recorded
    /// `schema_version` in the `meta` table.
    pub fn migrate(&self) -> anyhow::Result<()> {
        let mut conn = self.conn.lock().expect("sentinel storage mutex poisoned");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        let current: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [SCHEMA_VERSION_KEY],
                |r| r.get(0),
            )
            .optional()?;
        let current_id = current.as_deref().unwrap_or("");

        for (id, sql) in MIGRATIONS {
            if *id <= current_id { continue; }
            let tx = conn.transaction()?;
            tx.execute_batch(sql)
                .with_context(|| format!("running migration {id}"))?;
            tx.execute(
                "INSERT INTO meta(key, value) VALUES(?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [SCHEMA_VERSION_KEY, id],
            )?;
            tx.commit()?;
            tracing::info!(target: "sentinel::storage", "applied migration {id}");
        }
        Ok(())
    }

    // ---------- Retention --------------------------------------------------

    /// Delete captured time-series rows older than `cutoff` across every
    /// time-series table, bounding the store's on-disk growth. Returns the
    /// number of rows removed. After a non-trivial prune we truncate the WAL so
    /// freed pages are actually reclaimed on disk.
    ///
    /// `instances` / `poller_state` / `meta` / the AI + analysis logs are NOT
    /// touched — only the high-volume telemetry tables age out.
    pub fn prune_before(&self, cutoff: DateTime<Utc>) -> anyhow::Result<usize> {
        const TABLES: &[&str] = &[
            "query_store_snapshot",
            "live_request_snapshot",
            "wait_stats_delta",
            "deadlock_capture",
            "index_usage_delta",
            "size_snapshot",
            "cpu_pressure_snapshot",
            "memory_headroom_snapshot",
            "io_latency_delta",
            "tempdb_contention_snapshot",
            "plan_cache_snapshot",
        ];
        let cutoff_ms = cutoff.timestamp_millis();
        let conn = self.conn.lock().expect("sentinel storage mutex poisoned");
        let mut removed = 0usize;
        for t in TABLES {
            // Table names are compile-time constants, never user input.
            removed += conn.execute(
                &format!("DELETE FROM {t} WHERE captured_at < ?1"),
                [cutoff_ms],
            )?;
        }
        // The durable baseline keys on last_updated_ms (it has no captured_at).
        // Age out baselines for queries that stopped running before the cutoff
        // so the table can't grow without bound either.
        removed += conn.execute(
            "DELETE FROM query_baseline WHERE last_updated_ms < ?1",
            [cutoff_ms],
        )?;
        if removed > 0 {
            // Best-effort: reclaim WAL space after a large delete.
            let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        }
        Ok(removed)
    }

    /// How many seconds of captured telemetry we hold, i.e. `now - oldest
    /// captured_at` across every time-series table. `None` when nothing has been
    /// captured yet. Used by the Health front-door to tell "monitored long
    /// enough to trust the all-clear" from "just started / counters just reset".
    pub fn monitoring_age_secs(&self) -> Option<i64> {
        let conn = self.conn.lock().expect("sentinel storage mutex poisoned");
        let oldest_ms: Option<i64> = conn
            .query_row(
                "SELECT MIN(m) FROM (
                    SELECT MIN(captured_at) AS m FROM query_store_snapshot
                    UNION ALL SELECT MIN(captured_at) FROM live_request_snapshot
                    UNION ALL SELECT MIN(captured_at) FROM wait_stats_delta
                    UNION ALL SELECT MIN(captured_at) FROM deadlock_capture
                    UNION ALL SELECT MIN(captured_at) FROM index_usage_delta
                    UNION ALL SELECT MIN(captured_at) FROM size_snapshot
                    UNION ALL SELECT MIN(captured_at) FROM cpu_pressure_snapshot
                    UNION ALL SELECT MIN(captured_at) FROM memory_headroom_snapshot
                    UNION ALL SELECT MIN(captured_at) FROM io_latency_delta
                    UNION ALL SELECT MIN(captured_at) FROM tempdb_contention_snapshot
                    UNION ALL SELECT MIN(captured_at) FROM plan_cache_snapshot
                 )",
                [],
                |r| r.get::<_, Option<i64>>(0),
            )
            .ok()
            .flatten();
        oldest_ms.map(|ms| (Utc::now().timestamp_millis() - ms).max(0) / 1000)
    }

    // ---------- Instance registry ----------------------------------------

    /// Find-or-create an `instances` row for the given name + connection.
    pub fn ensure_instance(&self, name: &str, conn: &ConnectionInfo) -> anyhow::Result<i64> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        if let Some(id) = lock
            .query_row("SELECT id FROM instances WHERE name = ?1", [name], |r| r.get::<_, i64>(0))
            .optional()?
        {
            return Ok(id);
        }
        let auth_mode = match (conn.user.as_deref(), conn.password.as_deref()) {
            (Some(u), Some(_)) if !u.is_empty() => "sql",
            _ => "integrated",
        };
        lock.execute(
            "INSERT INTO instances(name, server, db, auth_mode, enabled, created_at)
                 VALUES(?1, ?2, ?3, ?4, 1, ?5)",
            params![
                name,
                conn.server,
                conn.database.clone().unwrap_or_default(),
                auth_mode,
                Utc::now().timestamp_millis(),
            ],
        )?;
        Ok(lock.last_insert_rowid())
    }

    // ---------- Insert helpers --------------------------------------------

    pub fn insert_query_store_row(&self, instance_id: i64, row: &QueryStoreRow) -> anyhow::Result<()> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        lock.execute(
            "INSERT INTO query_store_snapshot(instance_id, captured_at, query_id, plan_id,
                 total_duration_ms, cpu_ms, logical_reads, executions,
                 query_sql_text, last_execution_ms)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                instance_id,
                row.captured_at.timestamp_millis(),
                row.query_id,
                row.plan_id,
                row.total_duration_ms,
                row.cpu_ms,
                row.logical_reads,
                row.executions,
                row.query_sql_text,
                row.last_execution_ms,
            ],
        )?;
        Ok(())
    }

    pub fn insert_live_request(&self, instance_id: i64, row: &LiveRequestRow) -> anyhow::Result<()> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        lock.execute(
            "INSERT INTO live_request_snapshot(instance_id, captured_at, session_id, request_id,
                 duration_ms, blocking_session_id, wait_type, sql_text_hash, sql_text_preview)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                instance_id,
                row.captured_at.timestamp_millis(),
                row.session_id,
                row.request_id,
                row.duration_ms,
                row.blocking_session_id,
                row.wait_type,
                row.sql_text_hash,
                row.sql_text_preview,
            ],
        )?;
        Ok(())
    }

    pub fn insert_wait_delta(&self, instance_id: i64, row: &WaitDeltaRow) -> anyhow::Result<()> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        lock.execute(
            "INSERT INTO wait_stats_delta(instance_id, captured_at, wait_type,
                 waiting_tasks_count_delta, wait_time_ms_delta, signal_wait_ms_delta)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                instance_id,
                row.captured_at.timestamp_millis(),
                row.wait_type,
                row.waiting_tasks_count_delta,
                row.wait_time_ms_delta,
                row.signal_wait_ms_delta,
            ],
        )?;
        Ok(())
    }

    pub fn insert_deadlock(&self, instance_id: i64, row: &DeadlockRow) -> anyhow::Result<()> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        lock.execute(
            "INSERT INTO deadlock_capture(instance_id, captured_at, xml_blob,
                 victim_session_id, victim_resource, graph_hash)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                instance_id,
                row.captured_at.timestamp_millis(),
                row.xml_blob,
                row.victim_session_id,
                row.victim_resource,
                row.graph_hash,
            ],
        )?;
        Ok(())
    }

    /// True if a deadlock graph with this hash is already stored — so we count
    /// each real deadlock once instead of re-counting it every poll.
    pub fn deadlock_graph_exists(&self, instance_id: i64, hash: &str) -> bool {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        lock.query_row(
            "SELECT 1 FROM deadlock_capture WHERE instance_id = ?1 AND graph_hash = ?2 LIMIT 1",
            params![instance_id, hash],
            |_| Ok(()),
        )
        .optional()
        .ok()
        .flatten()
        .is_some()
    }

    pub fn insert_index_usage_delta(&self, instance_id: i64, row: &IndexUsageDeltaRow) -> anyhow::Result<()> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        lock.execute(
            "INSERT INTO index_usage_delta(instance_id, captured_at, db_name, schema_name,
                 table_name, index_name, seeks_delta, scans_delta, lookups_delta, updates_delta)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                instance_id,
                row.captured_at.timestamp_millis(),
                row.db_name,
                row.schema_name,
                row.table_name,
                row.index_name,
                row.seeks_delta,
                row.scans_delta,
                row.lookups_delta,
                row.updates_delta,
            ],
        )?;
        Ok(())
    }

    pub fn insert_size_snapshot(&self, instance_id: i64, row: &SizeSnapshotRow) -> anyhow::Result<()> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        lock.execute(
            "INSERT INTO size_snapshot(instance_id, captured_at, schema_name, table_name,
                 index_name, reserved_kb, used_kb, data_kb, row_count)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                instance_id,
                row.captured_at.timestamp_millis(),
                row.schema_name,
                row.table_name,
                row.index_name,
                row.reserved_kb,
                row.used_kb,
                row.data_kb,
                row.row_count,
            ],
        )?;
        Ok(())
    }

    // ---------- Deep vitals (CPU/memory/IO/tempdb/plan-cache) -------------

    pub fn insert_cpu_pressure(&self, instance_id: i64, row: &CpuPressureRow) -> anyhow::Result<()> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        lock.execute(
            "INSERT INTO cpu_pressure_snapshot(instance_id, captured_at, online_schedulers,
                 runnable_tasks, work_queue, current_workers, active_workers, pending_disk_io)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                instance_id,
                row.captured_at.timestamp_millis(),
                row.online_schedulers,
                row.runnable_tasks,
                row.work_queue,
                row.current_workers,
                row.active_workers,
                row.pending_disk_io,
            ],
        )?;
        Ok(())
    }

    pub fn insert_memory_headroom(&self, instance_id: i64, row: &MemoryHeadroomRow) -> anyhow::Result<()> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        lock.execute(
            "INSERT INTO memory_headroom_snapshot(instance_id, captured_at, page_life_expectancy,
                 pending_memory_grants, granted_memory_kb, target_server_memory_kb, total_server_memory_kb)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                instance_id,
                row.captured_at.timestamp_millis(),
                row.page_life_expectancy,
                row.pending_memory_grants,
                row.granted_memory_kb,
                row.target_server_memory_kb,
                row.total_server_memory_kb,
            ],
        )?;
        Ok(())
    }

    pub fn insert_io_latency(&self, instance_id: i64, row: &IoLatencyRow) -> anyhow::Result<()> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        lock.execute(
            "INSERT INTO io_latency_delta(instance_id, captured_at, database_name, file_logical_name,
                 file_type, reads_delta, writes_delta, read_stall_ms_delta, write_stall_ms_delta,
                 avg_read_latency_ms, avg_write_latency_ms)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                instance_id,
                row.captured_at.timestamp_millis(),
                row.database_name,
                row.file_logical_name,
                row.file_type,
                row.reads_delta,
                row.writes_delta,
                row.read_stall_ms_delta,
                row.write_stall_ms_delta,
                row.avg_read_latency_ms,
                row.avg_write_latency_ms,
            ],
        )?;
        Ok(())
    }

    pub fn insert_tempdb_contention(&self, instance_id: i64, row: &TempdbContentionRow) -> anyhow::Result<()> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        lock.execute(
            "INSERT INTO tempdb_contention_snapshot(instance_id, captured_at, pagelatch_waiters,
                 pfs_waiters, gam_waiters, sgam_waiters, total_wait_ms, tempdb_data_files)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                instance_id,
                row.captured_at.timestamp_millis(),
                row.pagelatch_waiters,
                row.pfs_waiters,
                row.gam_waiters,
                row.sgam_waiters,
                row.total_wait_ms,
                row.tempdb_data_files,
            ],
        )?;
        Ok(())
    }

    pub fn insert_plan_cache(&self, instance_id: i64, row: &PlanCacheRow) -> anyhow::Result<()> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        lock.execute(
            "INSERT INTO plan_cache_snapshot(instance_id, captured_at, single_use_plan_count,
                 single_use_size_kb, total_plan_count, total_size_kb)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                instance_id,
                row.captured_at.timestamp_millis(),
                row.single_use_plan_count,
                row.single_use_size_kb,
                row.total_plan_count,
                row.total_size_kb,
            ],
        )?;
        Ok(())
    }

    // ---------- Deep vitals read-back -------------------------------------
    // The live "DEEP VITALS" surface reads the most-recent persisted sample of
    // each surface for one instance. Each returns `None`/empty when nothing has
    // been captured yet (honest empty state, never an error).

    /// Read-only instance lookup by SERVER name. Unlike `ensure_instance` this
    /// NEVER creates a row — a read of vitals for an unmonitored server returns
    /// `None` so the caller can answer "no data yet" instead of conjuring an
    /// empty instance. We match on `server` (not the unique `name`) because the
    /// live UI knows the server it is connected to, not the daemon's label.
    pub fn get_instance_id(&self, server: &str) -> Option<i64> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        lock.query_row(
            "SELECT id FROM instances WHERE server = ?1 ORDER BY id DESC LIMIT 1",
            [server],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    /// Most-recent CPU/scheduler-pressure sample for this instance.
    pub fn latest_cpu_pressure(&self, instance_id: i64) -> Option<CpuPressureRow> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        lock.query_row(
            "SELECT captured_at, online_schedulers, runnable_tasks, work_queue,
                    current_workers, active_workers, pending_disk_io
             FROM cpu_pressure_snapshot
             WHERE instance_id = ?1
             ORDER BY captured_at DESC LIMIT 1",
            [instance_id],
            |r| {
                Ok(CpuPressureRow {
                    captured_at: ms_to_dt(r.get(0)?),
                    online_schedulers: r.get(1)?,
                    runnable_tasks: r.get(2)?,
                    work_queue: r.get(3)?,
                    current_workers: r.get(4)?,
                    active_workers: r.get(5)?,
                    pending_disk_io: r.get(6)?,
                })
            },
        )
        .optional()
        .ok()
        .flatten()
    }

    /// Most-recent memory-headroom sample for this instance.
    pub fn latest_memory_headroom(&self, instance_id: i64) -> Option<MemoryHeadroomRow> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        lock.query_row(
            "SELECT captured_at, page_life_expectancy, pending_memory_grants,
                    granted_memory_kb, target_server_memory_kb, total_server_memory_kb
             FROM memory_headroom_snapshot
             WHERE instance_id = ?1
             ORDER BY captured_at DESC LIMIT 1",
            [instance_id],
            |r| {
                Ok(MemoryHeadroomRow {
                    captured_at: ms_to_dt(r.get(0)?),
                    page_life_expectancy: r.get(1)?,
                    pending_memory_grants: r.get(2)?,
                    granted_memory_kb: r.get(3)?,
                    target_server_memory_kb: r.get(4)?,
                    total_server_memory_kb: r.get(5)?,
                })
            },
        )
        .optional()
        .ok()
        .flatten()
    }

    /// Every per-file IO-latency row at the MOST-RECENT captured_at for this
    /// instance (one tick captures one row per active file). Empty when none.
    pub fn latest_io_latency(&self, instance_id: i64) -> Vec<IoLatencyRow> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        // Find the newest capture instant, then return every file row at it.
        let latest_ms: Option<i64> = lock
            .query_row(
                "SELECT MAX(captured_at) FROM io_latency_delta WHERE instance_id = ?1",
                [instance_id],
                |r| r.get::<_, Option<i64>>(0),
            )
            .optional()
            .ok()
            .flatten()
            .flatten();
        let Some(at) = latest_ms else { return Vec::new() };
        let mut stmt = match lock.prepare(
            "SELECT captured_at, database_name, file_logical_name, file_type,
                    reads_delta, writes_delta, read_stall_ms_delta, write_stall_ms_delta,
                    avg_read_latency_ms, avg_write_latency_ms
             FROM io_latency_delta
             WHERE instance_id = ?1 AND captured_at = ?2
             ORDER BY (avg_read_latency_ms + avg_write_latency_ms) DESC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![instance_id, at], |r| {
            Ok(IoLatencyRow {
                captured_at: ms_to_dt(r.get(0)?),
                database_name: r.get(1)?,
                file_logical_name: r.get(2)?,
                file_type: r.get(3)?,
                reads_delta: r.get(4)?,
                writes_delta: r.get(5)?,
                read_stall_ms_delta: r.get(6)?,
                write_stall_ms_delta: r.get(7)?,
                avg_read_latency_ms: r.get(8)?,
                avg_write_latency_ms: r.get(9)?,
            })
        });
        match rows {
            Ok(it) => it.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Most-recent tempdb allocation-page contention sample for this instance.
    pub fn latest_tempdb_contention(&self, instance_id: i64) -> Option<TempdbContentionRow> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        lock.query_row(
            "SELECT captured_at, pagelatch_waiters, pfs_waiters, gam_waiters,
                    sgam_waiters, total_wait_ms, tempdb_data_files
             FROM tempdb_contention_snapshot
             WHERE instance_id = ?1
             ORDER BY captured_at DESC LIMIT 1",
            [instance_id],
            |r| {
                Ok(TempdbContentionRow {
                    captured_at: ms_to_dt(r.get(0)?),
                    pagelatch_waiters: r.get(1)?,
                    pfs_waiters: r.get(2)?,
                    gam_waiters: r.get(3)?,
                    sgam_waiters: r.get(4)?,
                    total_wait_ms: r.get(5)?,
                    tempdb_data_files: r.get(6)?,
                })
            },
        )
        .optional()
        .ok()
        .flatten()
    }

    /// Most-recent plan-cache-health sample for this instance.
    pub fn latest_plan_cache(&self, instance_id: i64) -> Option<PlanCacheRow> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        lock.query_row(
            "SELECT captured_at, single_use_plan_count, single_use_size_kb,
                    total_plan_count, total_size_kb
             FROM plan_cache_snapshot
             WHERE instance_id = ?1
             ORDER BY captured_at DESC LIMIT 1",
            [instance_id],
            |r| {
                Ok(PlanCacheRow {
                    captured_at: ms_to_dt(r.get(0)?),
                    single_use_plan_count: r.get(1)?,
                    single_use_size_kb: r.get(2)?,
                    total_plan_count: r.get(3)?,
                    total_size_kb: r.get(4)?,
                })
            },
        )
        .optional()
        .ok()
        .flatten()
    }

    /// Most-recent per-file cumulative IO snapshot for this instance, keyed by
    /// (database_name, file_logical_name) → (num_of_reads, num_of_writes,
    /// io_stall_read_ms, io_stall_write_ms). The IO-latency poller diffs the
    /// current cumulative reading against this to derive per-window deltas.
    /// `None` on the very first observation (nothing to diff against yet).
    pub fn previous_io_file_snapshot(
        &self,
        instance_id: i64,
    ) -> Option<HashMap<(String, String), (i64, i64, i64, i64)>> {
        let raw = self.get_state(instance_id, "io_file_snapshot").ok().flatten()?;
        let v: Vec<((String, String), (i64, i64, i64, i64))> = serde_json::from_str(&raw).ok()?;
        Some(v.into_iter().collect())
    }

    pub fn update_io_file_snapshot(
        &self,
        instance_id: i64,
        snapshot: &HashMap<(String, String), (i64, i64, i64, i64)>,
    ) -> anyhow::Result<()> {
        let v: Vec<(&(String, String), &(i64, i64, i64, i64))> = snapshot.iter().collect();
        let raw = serde_json::to_string(&v)?;
        self.set_state(instance_id, "io_file_snapshot", &raw)
    }

    // ---------- Poller state (generic key/value JSON store) ---------------

    fn get_state(&self, instance_id: i64, key: &str) -> anyhow::Result<Option<String>> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        Ok(lock
            .query_row(
                "SELECT value FROM poller_state WHERE instance_id = ?1 AND key = ?2",
                params![instance_id, key],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
    }

    fn set_state(&self, instance_id: i64, key: &str, value: &str) -> anyhow::Result<()> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        lock.execute(
            "INSERT INTO poller_state(instance_id, key, value, updated_at)
             VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(instance_id, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![instance_id, key, value, Utc::now().timestamp_millis()],
        )?;
        Ok(())
    }

    // ---------- Wait-stats snapshot --------------------------------------

    pub fn previous_wait_snapshot(&self, instance_id: i64) -> Option<HashMap<String, (i64, i64, i64)>> {
        let raw = self.get_state(instance_id, "wait_snapshot").ok().flatten()?;
        serde_json::from_str(&raw).ok()
    }

    pub fn update_wait_snapshot(
        &self,
        instance_id: i64,
        snapshot: &HashMap<String, (i64, i64, i64)>,
    ) -> anyhow::Result<()> {
        let raw = serde_json::to_string(snapshot)?;
        self.set_state(instance_id, "wait_snapshot", &raw)
    }

    // ---------- Index-usage snapshot --------------------------------------

    /// `key = (db, schema, table, index)`, `value = (seeks, scans, lookups, updates)`.
    pub fn previous_index_snapshot(
        &self,
        instance_id: i64,
    ) -> Option<HashMap<(String, String, String, String), (i64, i64, i64, i64)>> {
        let raw = self.get_state(instance_id, "index_snapshot").ok().flatten()?;
        let v: Vec<((String, String, String, String), (i64, i64, i64, i64))> =
            serde_json::from_str(&raw).ok()?;
        Some(v.into_iter().collect())
    }

    pub fn update_index_snapshot(
        &self,
        instance_id: i64,
        snapshot: &HashMap<(String, String, String, String), (i64, i64, i64, i64)>,
    ) -> anyhow::Result<()> {
        let v: Vec<_> = snapshot.iter().map(|(k, v)| (k.clone(), *v)).collect();
        let raw = serde_json::to_string(&v)?;
        self.set_state(instance_id, "index_snapshot", &raw)
    }

    // ---------- Deadlock dedup --------------------------------------------

    pub fn last_deadlock_hash(&self, instance_id: i64) -> Option<String> {
        self.get_state(instance_id, "last_deadlock_hash").ok().flatten()
    }

    pub fn set_last_deadlock_hash(&self, instance_id: i64, hash: &str) -> anyhow::Result<()> {
        self.set_state(instance_id, "last_deadlock_hash", hash)
    }

    // ---------- Query helpers ---------------------------------------------

    pub fn top_n_by_duration(&self, window: TimeRange, n: usize) -> anyhow::Result<Vec<TopQueryRow>> {
        self.top_n_queries(window, n, "total_duration_ms DESC")
    }

    /// Top queries by most-recent execution (the "by last run" view). Queries
    /// missing a last_execution_ms (pre-0004 rows) sort last.
    pub fn top_n_by_recency(&self, window: TimeRange, n: usize) -> anyhow::Result<Vec<TopQueryRow>> {
        self.top_n_queries(window, n, "last_execution_ms IS NULL, last_execution_ms DESC")
    }

    /// Shared aggregation for the top-queries views; `order_by` is a fixed,
    /// caller-supplied clause (never user input).
    fn top_n_queries(&self, window: TimeRange, n: usize, order_by: &str) -> anyhow::Result<Vec<TopQueryRow>> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        let sql = format!(
            "SELECT query_id, plan_id,
                    SUM(total_duration_ms) AS total_duration_ms,
                    SUM(executions)        AS executions,
                    MAX(query_sql_text)    AS query_sql_text,
                    MAX(last_execution_ms) AS last_execution_ms
             FROM query_store_snapshot
             WHERE captured_at >= ?1 AND captured_at < ?2
             GROUP BY query_id, plan_id
             ORDER BY {order_by}
             LIMIT ?3"
        );
        let mut stmt = lock.prepare(&sql)?;
        let rows = stmt
            .query_map(params![window.from_ms(), window.to_ms(), n as i64], |r| {
                Ok(TopQueryRow {
                    query_id: r.get(0)?,
                    plan_id: r.get(1)?,
                    total_duration_ms: r.get(2)?,
                    executions: r.get(3)?,
                    query_sql_text: r.get(4)?,
                    last_execution_ms: r.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Regressions: queries whose most-recent per-execution duration is a
    /// statistical outlier versus that query's own recent history.
    ///
    /// For each query we read the snapshot series in `window`, derive the
    /// per-snapshot mean duration-per-execution (`total_duration_ms /
    /// executions`), then compute the rolling mean and (sample) standard
    /// deviation of the *prior* snapshots. The latest snapshot is flagged when
    /// it lands above `mean + Z_SCORE_K * stddev` (a z-score above the
    /// configured threshold), the absolute jump clears
    /// [`REGRESSION_MIN_ABS_DELTA_MS`], and there are enough samples and
    /// executions to be meaningful. Tiny samples fall back gracefully: a query
    /// with fewer than [`REGRESSION_MIN_SAMPLES`] snapshots is skipped rather
    /// than judged against a noisy stddev. `baseline_duration_ms` carries the
    /// rolling-mean baseline and `delta_pct` the percent change versus it.
    pub fn regressions_since(&self, window: TimeRange) -> anyhow::Result<Vec<RegressionRow>> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        // Pull the per-snapshot duration-per-execution series per query,
        // oldest→newest, so the last element of each group is "current".
        let mut stmt = lock.prepare(
            "SELECT query_id, captured_at,
                    CAST(total_duration_ms AS REAL) / executions AS dur_per_exec,
                    executions
             FROM query_store_snapshot
             WHERE captured_at >= ?1 AND captured_at < ?2
               AND executions > 0
             ORDER BY query_id, captured_at",
        )?;
        // Each row: (query_id, dur_per_exec, executions).
        let mut series: std::collections::BTreeMap<i64, Vec<(f64, i64)>> =
            std::collections::BTreeMap::new();
        let raw = stmt
            .query_map(params![window.from_ms(), window.to_ms()], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(2)?, r.get::<_, i64>(3)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);
        drop(lock);
        for (qid, dur, exec) in raw {
            series.entry(qid).or_default().push((dur, exec));
        }

        let mut out: Vec<RegressionRow> = Vec::new();
        for (qid, points) in series {
            if let Some(row) = detect_regression(qid, &points) {
                out.push(row);
            }
        }
        // Largest absolute slowdown first; cap the report list.
        out.sort_by(|a, b| {
            (b.current_duration_ms - b.baseline_duration_ms)
                .cmp(&(a.current_duration_ms - a.baseline_duration_ms))
        });
        out.truncate(50);
        Ok(out)
    }

    // ---------- Durable rolling baseline ----------------------------------

    /// Read the persisted rolling baseline for one query, if any has been
    /// accumulated yet.
    pub fn get_query_baseline(
        &self,
        instance_id: i64,
        query_id: i64,
    ) -> anyhow::Result<Option<QueryBaseline>> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        Ok(lock
            .query_row(
                "SELECT query_id, count, mean, m2, last_value_ms
                 FROM query_baseline WHERE instance_id = ?1 AND query_id = ?2",
                params![instance_id, query_id],
                |r| {
                    Ok(QueryBaseline {
                        query_id: r.get(0)?,
                        count: r.get(1)?,
                        mean: r.get(2)?,
                        m2: r.get(3)?,
                        last_value_ms: r.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    /// Fold one new duration-per-execution `sample` (ms) into the durable
    /// baseline for `query_id`, creating the row on first sight. Welford's
    /// online update means we never re-read the whole history. Returns the
    /// updated baseline (post-fold) so callers can judge the same sample
    /// against it without a second read. Persisted, so it survives restarts.
    pub fn update_query_baseline(
        &self,
        instance_id: i64,
        query_id: i64,
        sample: f64,
    ) -> anyhow::Result<QueryBaseline> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        let existing: Option<QueryBaseline> = lock
            .query_row(
                "SELECT query_id, count, mean, m2, last_value_ms
                 FROM query_baseline WHERE instance_id = ?1 AND query_id = ?2",
                params![instance_id, query_id],
                |r| {
                    Ok(QueryBaseline {
                        query_id: r.get(0)?,
                        count: r.get(1)?,
                        mean: r.get(2)?,
                        m2: r.get(3)?,
                        last_value_ms: r.get(4)?,
                    })
                },
            )
            .optional()?;
        let base = existing.unwrap_or(QueryBaseline {
            query_id,
            count: 0,
            mean: 0.0,
            m2: 0.0,
            last_value_ms: 0.0,
        });
        let updated = base.fold(sample);
        lock.execute(
            "INSERT INTO query_baseline
                 (instance_id, query_id, count, mean, m2, last_value_ms, last_updated_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(instance_id, query_id) DO UPDATE SET
                 count = excluded.count,
                 mean = excluded.mean,
                 m2 = excluded.m2,
                 last_value_ms = excluded.last_value_ms,
                 last_updated_ms = excluded.last_updated_ms",
            params![
                instance_id,
                query_id,
                updated.count,
                updated.mean,
                updated.m2,
                updated.last_value_ms,
                Utc::now().timestamp_millis(),
            ],
        )?;
        Ok(updated)
    }

    /// Durable regression check for one query: fold the new `sample` into the
    /// persisted baseline, then judge it against the baseline *as it stood
    /// before* this sample (so a spike never inflates its own baseline). When
    /// the persisted baseline is too thin
    /// ([`DURABLE_BASELINE_MIN_COUNT`]) this returns `None` and the caller
    /// should fall back to the in-window z-score
    /// ([`detect_regression`]) — the durable path strengthens, never replaces,
    /// the existing detector.
    ///
    /// This is the entry point a poller calls per query each tick: it both
    /// advances the durable baseline AND reports a regression in one shot.
    pub fn observe_and_detect_regression(
        &self,
        instance_id: i64,
        query_id: i64,
        sample: f64,
    ) -> anyhow::Result<Option<RegressionRow>> {
        // Snapshot the baseline *before* folding so the current sample is
        // judged against established history, not against itself.
        let prior = self
            .get_query_baseline(instance_id, query_id)?
            .unwrap_or(QueryBaseline {
                query_id,
                count: 0,
                mean: 0.0,
                m2: 0.0,
                last_value_ms: 0.0,
            });
        // Always advance the durable baseline, regardless of verdict.
        self.update_query_baseline(instance_id, query_id, sample)?;
        Ok(detect_regression_durable(&prior, sample))
    }

    /// Drop durable baselines that haven't been refreshed since `cutoff` (e.g.
    /// queries that aged out of the workload). Keeps the baseline table bounded
    /// alongside the time-series prune. Returns the number of rows removed.
    pub fn prune_query_baselines_before(&self, cutoff: DateTime<Utc>) -> anyhow::Result<usize> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        Ok(lock.execute(
            "DELETE FROM query_baseline WHERE last_updated_ms < ?1",
            [cutoff.timestamp_millis()],
        )?)
    }

    pub fn pain_summary(&self, window: TimeRange) -> anyhow::Result<PainSummary> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        let top_wait_sql = format!(
            "SELECT wait_type, SUM(wait_time_ms_delta) AS w
             FROM wait_stats_delta
             WHERE captured_at >= ?1 AND captured_at < ?2
               AND wait_type NOT IN ({})
             GROUP BY wait_type
             ORDER BY w DESC
             LIMIT 1",
            ignorable_waits_in_clause()
        );
        let top_wait: Option<(String, i64)> = lock
            .query_row(
                &top_wait_sql,
                params![window.from_ms(), window.to_ms()],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
            )
            .optional()?;
        let deadlock_count: i64 = lock
            .query_row(
                "SELECT COUNT(*) FROM deadlock_capture WHERE captured_at >= ?1 AND captured_at < ?2",
                params![window.from_ms(), window.to_ms()],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let blocking_incidents: i64 = lock
            .query_row(
                "SELECT COUNT(*) FROM live_request_snapshot
                 WHERE captured_at >= ?1 AND captured_at < ?2 AND blocking_session_id IS NOT NULL",
                params![window.from_ms(), window.to_ms()],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        Ok(PainSummary {
            top_wait_type: top_wait.as_ref().map(|t| t.0.clone()),
            top_wait_time_ms: top_wait.map(|t| t.1).unwrap_or(0),
            deadlock_count,
            blocking_incidents,
        })
    }

    pub fn unused_indexes(&self, window: TimeRange) -> anyhow::Result<Vec<UnusedIndexRow>> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        let mut stmt = lock.prepare(
            "SELECT db_name, schema_name, table_name, index_name,
                    SUM(updates_delta) AS updates
             FROM index_usage_delta
             WHERE captured_at >= ?1 AND captured_at < ?2
             GROUP BY db_name, schema_name, table_name, index_name
             HAVING SUM(seeks_delta) = 0
                AND SUM(scans_delta) = 0
                AND SUM(lookups_delta) = 0
                AND SUM(updates_delta) > 100
             ORDER BY updates DESC
             LIMIT 50",
        )?;
        let rows = stmt
            .query_map(params![window.from_ms(), window.to_ms()], |r| {
                Ok(UnusedIndexRow {
                    db_name: r.get(0)?,
                    schema_name: r.get(1)?,
                    table_name: r.get(2)?,
                    index_name: r.get(3)?,
                    updates_in_window: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn instance_count(&self) -> anyhow::Result<i64> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        Ok(lock.query_row("SELECT COUNT(*) FROM instances", [], |r| r.get(0))?)
    }

    // ---------- AI interaction log ---------------------------------------
    // Upsert by id so a streaming response can be UPDATEd in place as more
    // tokens arrive — the final state is what survives.
    pub fn upsert_ai_interaction(&self, row: &AiInteractionRow) -> anyhow::Result<()> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        lock.execute(
            "INSERT INTO ai_interactions (id, occurred_at, provider, model, system_prompt,
                 user_prompt, response, status, error_message, latency_ms, tokens_in, tokens_out)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET
                 response = excluded.response,
                 status = excluded.status,
                 error_message = excluded.error_message,
                 latency_ms = excluded.latency_ms,
                 tokens_in = excluded.tokens_in,
                 tokens_out = excluded.tokens_out",
            params![
                row.id,
                row.occurred_at.to_rfc3339(),
                row.provider,
                row.model,
                row.system_prompt,
                row.user_prompt,
                row.response,
                row.status,
                row.error_message,
                row.latency_ms,
                row.tokens_in,
                row.tokens_out,
            ],
        )?;
        Ok(())
    }

    pub fn list_ai_interactions(&self, limit: i64) -> anyhow::Result<Vec<AiInteractionRow>> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        let mut stmt = lock.prepare(
            "SELECT id, occurred_at, provider, model, system_prompt, user_prompt,
                    response, status, error_message, latency_ms, tokens_in, tokens_out
             FROM ai_interactions ORDER BY occurred_at DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map([limit], |r| {
                Ok(AiInteractionRow {
                    id: r.get(0)?,
                    occurred_at: r.get::<_, String>(1)?.parse().unwrap_or_else(|_| Utc::now()),
                    provider: r.get(2)?,
                    model: r.get(3)?,
                    system_prompt: r.get(4)?,
                    user_prompt: r.get(5)?,
                    response: r.get(6)?,
                    status: r.get(7)?,
                    error_message: r.get(8)?,
                    latency_ms: r.get(9)?,
                    tokens_in: r.get(10)?,
                    tokens_out: r.get(11)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn clear_ai_interactions(&self) -> anyhow::Result<usize> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        Ok(lock.execute("DELETE FROM ai_interactions", [])?)
    }

    // ---------- Analysis-run log -----------------------------------------
    // The whole run + its findings go in one transaction so a half-written
    // run can't leave orphan findings (or vice versa).
    pub fn insert_analysis_run(
        &self,
        run: &AnalysisRunRow,
        findings: &[AnalysisFindingRow],
    ) -> anyhow::Result<()> {
        let mut lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        let tx = lock.transaction()?;
        tx.execute(
            "INSERT INTO analysis_runs (id, occurred_at, server_name, database_name, mode,
                 sql_hash, sql_preview, server_version, findings_total,
                 findings_critical, findings_error, findings_warning, findings_info,
                 plan_attached, plan_subtree_cost, plan_op_count, duration_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                run.id,
                run.occurred_at.to_rfc3339(),
                run.server_name,
                run.database_name,
                run.mode,
                run.sql_hash,
                run.sql_preview,
                run.server_version,
                run.findings_total,
                run.findings_critical,
                run.findings_error,
                run.findings_warning,
                run.findings_info,
                if run.plan_attached { 1 } else { 0 },
                run.plan_subtree_cost,
                run.plan_op_count,
                run.duration_ms,
            ],
        )?;
        for f in findings {
            tx.execute(
                "INSERT INTO analysis_findings (run_id, rule_id, severity, line_no, col_no,
                     message, recommendation)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    run.id, f.rule_id, f.severity, f.line_no, f.col_no, f.message, f.recommendation,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_analysis_runs(
        &self,
        server: Option<&str>,
        database: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Vec<AnalysisRunRow>> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        // Build the WHERE dynamically — rusqlite doesn't bind NULL nicely with =.
        let mut sql = String::from(
            "SELECT id, occurred_at, server_name, database_name, mode, sql_hash, sql_preview,
                    server_version, findings_total, findings_critical, findings_error,
                    findings_warning, findings_info, plan_attached, plan_subtree_cost,
                    plan_op_count, duration_ms
             FROM analysis_runs WHERE 1=1",
        );
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(s) = server { sql.push_str(" AND server_name = ?"); params_vec.push(Box::new(s.to_string())); }
        if let Some(d) = database { sql.push_str(" AND database_name = ?"); params_vec.push(Box::new(d.to_string())); }
        sql.push_str(" ORDER BY occurred_at DESC LIMIT ?");
        params_vec.push(Box::new(limit));
        let mut stmt = lock.prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|b| b.as_ref()).collect();
        let rows = stmt
            .query_map(refs.as_slice(), |r| {
                Ok(AnalysisRunRow {
                    id: r.get(0)?,
                    occurred_at: r.get::<_, String>(1)?.parse().unwrap_or_else(|_| Utc::now()),
                    server_name: r.get(2)?,
                    database_name: r.get(3)?,
                    mode: r.get(4)?,
                    sql_hash: r.get(5)?,
                    sql_preview: r.get(6)?,
                    server_version: r.get(7)?,
                    findings_total: r.get(8)?,
                    findings_critical: r.get(9)?,
                    findings_error: r.get(10)?,
                    findings_warning: r.get(11)?,
                    findings_info: r.get(12)?,
                    plan_attached: r.get::<_, i64>(13)? != 0,
                    plan_subtree_cost: r.get(14)?,
                    plan_op_count: r.get(15)?,
                    duration_ms: r.get(16)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn list_findings_for_run(&self, run_id: &str) -> anyhow::Result<Vec<AnalysisFindingRow>> {
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        let mut stmt = lock.prepare(
            "SELECT run_id, rule_id, severity, line_no, col_no, message, recommendation
             FROM analysis_findings WHERE run_id = ?1",
        )?;
        let rows = stmt
            .query_map([run_id], |r| {
                Ok(AnalysisFindingRow {
                    run_id: r.get(0)?,
                    rule_id: r.get(1)?,
                    severity: r.get(2)?,
                    line_no: r.get(3)?,
                    col_no: r.get(4)?,
                    message: r.get(5)?,
                    recommendation: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }
}

// ---------- Typed row inputs for the insert helpers ------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiInteractionRow {
    pub id: String,
    pub occurred_at: DateTime<Utc>,
    pub provider: String,
    pub model: String,
    pub system_prompt: Option<String>,
    pub user_prompt: String,
    pub response: String,
    pub status: String,
    pub error_message: Option<String>,
    pub latency_ms: Option<i64>,
    pub tokens_in: Option<i64>,
    pub tokens_out: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisRunRow {
    pub id: String,
    pub occurred_at: DateTime<Utc>,
    pub server_name: Option<String>,
    pub database_name: Option<String>,
    pub mode: String,
    pub sql_hash: Option<String>,
    pub sql_preview: Option<String>,
    pub server_version: Option<i64>,
    pub findings_total: i64,
    pub findings_critical: i64,
    pub findings_error: i64,
    pub findings_warning: i64,
    pub findings_info: i64,
    pub plan_attached: bool,
    pub plan_subtree_cost: Option<f64>,
    pub plan_op_count: Option<i64>,
    pub duration_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisFindingRow {
    pub run_id: String,
    pub rule_id: String,
    pub severity: String,
    pub line_no: Option<i64>,
    pub col_no: Option<i64>,
    pub message: String,
    pub recommendation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryStoreRow {
    pub captured_at: DateTime<Utc>,
    pub query_id: i64,
    pub plan_id: i64,
    pub total_duration_ms: i64,
    pub cpu_ms: i64,
    pub logical_reads: i64,
    pub executions: i64,
    /// The actual T-SQL text (truncated), so the dashboard shows *what* ran.
    pub query_sql_text: Option<String>,
    /// Epoch ms of the most recent execution in the captured interval.
    pub last_execution_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveRequestRow {
    pub captured_at: DateTime<Utc>,
    pub session_id: i64,
    pub request_id: i64,
    pub duration_ms: i64,
    pub blocking_session_id: Option<i64>,
    pub wait_type: Option<String>,
    pub sql_text_hash: Option<String>,
    pub sql_text_preview: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitDeltaRow {
    pub captured_at: DateTime<Utc>,
    pub wait_type: String,
    pub waiting_tasks_count_delta: i64,
    pub wait_time_ms_delta: i64,
    pub signal_wait_ms_delta: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadlockRow {
    pub captured_at: DateTime<Utc>,
    pub xml_blob: String,
    pub victim_session_id: Option<i64>,
    pub victim_resource: Option<String>,
    /// SHA-256 (prefix) of this single deadlock graph; dedup key across polls.
    pub graph_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexUsageDeltaRow {
    pub captured_at: DateTime<Utc>,
    pub db_name: String,
    pub schema_name: String,
    pub table_name: String,
    pub index_name: String,
    pub seeks_delta: i64,
    pub scans_delta: i64,
    pub lookups_delta: i64,
    pub updates_delta: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SizeSnapshotRow {
    pub captured_at: DateTime<Utc>,
    pub schema_name: String,
    pub table_name: String,
    pub index_name: Option<String>,
    pub reserved_kb: i64,
    pub used_kb: i64,
    pub data_kb: i64,
    pub row_count: i64,
}

/// One CPU/scheduler-pressure observation summed over VISIBLE ONLINE schedulers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuPressureRow {
    pub captured_at: DateTime<Utc>,
    pub online_schedulers: i64,
    pub runnable_tasks: i64,
    pub work_queue: i64,
    pub current_workers: i64,
    pub active_workers: i64,
    pub pending_disk_io: i64,
}

/// One memory-headroom observation (PLE + pending grants + buffer-pool sizing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryHeadroomRow {
    pub captured_at: DateTime<Utc>,
    pub page_life_expectancy: i64,
    pub pending_memory_grants: i64,
    pub granted_memory_kb: i64,
    pub target_server_memory_kb: i64,
    pub total_server_memory_kb: i64,
}

/// Per-file IO latency for the window, derived by diffing cumulative
/// `sys.dm_io_virtual_file_stats` against the prior tick.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IoLatencyRow {
    pub captured_at: DateTime<Utc>,
    pub database_name: String,
    pub file_logical_name: String,
    pub file_type: String,
    pub reads_delta: i64,
    pub writes_delta: i64,
    pub read_stall_ms_delta: i64,
    pub write_stall_ms_delta: i64,
    pub avg_read_latency_ms: f64,
    pub avg_write_latency_ms: f64,
}

/// One tempdb allocation-page contention observation (PAGELATCH on PFS/GAM/SGAM).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TempdbContentionRow {
    pub captured_at: DateTime<Utc>,
    pub pagelatch_waiters: i64,
    pub pfs_waiters: i64,
    pub gam_waiters: i64,
    pub sgam_waiters: i64,
    pub total_wait_ms: i64,
    pub tempdb_data_files: i64,
}

/// One plan-cache-health observation (single-use ad-hoc plans vs total).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanCacheRow {
    pub captured_at: DateTime<Utc>,
    pub single_use_plan_count: i64,
    pub single_use_size_kb: i64,
    pub total_plan_count: i64,
    pub total_size_kb: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_round_trip() {
        let s = Storage::open_in_memory().expect("open");
        let conn = ConnectionInfo {
            server: "localhost,1433".into(),
            database: Some("master".into()),
            user: Some("sa".into()),
            password: Some("x".into()),
            trust_cert: Some(true),
        };
        let id = s.ensure_instance("test", &conn).expect("ensure");
        s.insert_query_store_row(
            id,
            &QueryStoreRow {
                // Slightly in the past so it falls strictly inside the
                // [now-1h, now) window we query for.
                captured_at: Utc::now() - chrono::Duration::minutes(1),
                query_id: 1,
                plan_id: 2,
                total_duration_ms: 100,
                cpu_ms: 80,
                logical_reads: 9999,
                executions: 5,
                query_sql_text: Some("SELECT 1".into()),
                last_execution_ms: Some(Utc::now().timestamp_millis()),
            },
        )
        .expect("insert");
        let top = s.top_n_by_duration(TimeRange::last_hours(1), 10).expect("top");
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].query_id, 1);
        assert_eq!(top[0].query_sql_text.as_deref(), Some("SELECT 1"));

        let mut snap = HashMap::new();
        snap.insert("LCK_M_X".to_string(), (10i64, 100i64, 5i64));
        s.update_wait_snapshot(id, &snap).expect("save");
        let restored = s.previous_wait_snapshot(id).expect("restore");
        assert_eq!(restored.get("LCK_M_X"), Some(&(10, 100, 5)));
    }

    #[test]
    fn prune_drops_only_old_rows() {
        let s = Storage::open_in_memory().expect("open");
        let conn = ConnectionInfo {
            server: "localhost,1433".into(),
            database: Some("master".into()),
            user: Some("sa".into()),
            password: Some("x".into()),
            trust_cert: Some(true),
        };
        let id = s.ensure_instance("t", &conn).expect("ensure");
        let mk = |age_days: i64, qid: i64| QueryStoreRow {
            captured_at: Utc::now() - chrono::Duration::days(age_days),
            query_id: qid,
            plan_id: 1,
            total_duration_ms: 10,
            cpu_ms: 5,
            logical_reads: 1,
            executions: 1,
            query_sql_text: Some("SELECT 1".into()),
            last_execution_ms: Some(Utc::now().timestamp_millis()),
        };
        s.insert_query_store_row(id, &mk(100, 1)).expect("old");
        s.insert_query_store_row(id, &mk(1, 2)).expect("recent");

        let removed = s
            .prune_before(Utc::now() - chrono::Duration::days(30))
            .expect("prune");
        assert_eq!(removed, 1, "only the 100-day-old row should be pruned");

        let rows = s.top_n_by_duration(TimeRange::last_days(365), 10).expect("top");
        assert_eq!(rows.len(), 1, "recent row survives");
        assert_eq!(rows[0].query_id, 2);
    }

    // ---- z-score regression detector --------------------------------------

    /// A query whose latest sample spikes far above a stable history is flagged.
    #[test]
    fn detect_regression_flags_clear_outlier() {
        // 8 steady ~100ms samples (10 execs each) then a 1000ms spike.
        let mut pts: Vec<(f64, i64)> = (0..8).map(|i| (100.0 + (i % 2) as f64, 10)).collect();
        pts.push((1000.0, 10));
        let row = detect_regression(42, &pts).expect("clear spike should be flagged");
        assert_eq!(row.query_id, 42);
        assert_eq!(row.current_duration_ms, 1000);
        assert!(
            row.baseline_duration_ms >= 99 && row.baseline_duration_ms <= 101,
            "baseline is the rolling mean of prior samples, got {}",
            row.baseline_duration_ms
        );
        assert!(row.delta_pct > 800.0, "huge percent jump, got {}", row.delta_pct);
    }

    /// Too few samples → graceful fallback (no flag), never a panic or div-by-0.
    #[test]
    fn detect_regression_skips_tiny_sample() {
        let pts = vec![(100.0, 10), (5000.0, 10)];
        assert!(
            detect_regression(1, &pts).is_none(),
            "two samples is below REGRESSION_MIN_SAMPLES"
        );
        assert!(detect_regression(2, &[]).is_none(), "empty series");
    }

    /// A perfectly flat history has zero stddev → no statistical basis to flag,
    /// even if the latest sample ticks up a hair.
    #[test]
    fn detect_regression_handles_flat_history() {
        let mut pts: Vec<(f64, i64)> = (0..8).map(|_| (100.0, 10)).collect();
        pts.push((101.0, 10)); // small bump, but prior stddev is 0
        assert!(
            detect_regression(7, &pts).is_none(),
            "flat baseline (stddev 0) must not divide-by-zero or flag"
        );
    }

    /// Within-noise variation that never clears 3σ is not a regression.
    #[test]
    fn detect_regression_ignores_within_noise() {
        // Prior samples jitter 90..110; latest 112 is well under mean+3σ.
        let mut pts: Vec<(f64, i64)> =
            vec![90.0, 110.0, 95.0, 105.0, 100.0, 92.0, 108.0, 100.0]
                .into_iter()
                .map(|d| (d, 10))
                .collect();
        pts.push((112.0, 10));
        assert!(
            detect_regression(9, &pts).is_none(),
            "a value inside normal jitter must not be flagged"
        );
    }

    /// Rare queries (low total executions) are skipped even on a big spike.
    #[test]
    fn detect_regression_skips_rare_queries() {
        let mut pts: Vec<(f64, i64)> = (0..8).map(|_| (100.0, 1)).collect();
        pts.push((1000.0, 1)); // 9 total execs < REGRESSION_MIN_EXECUTIONS
        assert!(
            detect_regression(11, &pts).is_none(),
            "too few executions to trust the average"
        );
    }

    /// End-to-end through SQLite: a steady series + one spiked snapshot is
    /// surfaced by `regressions_since`.
    #[test]
    fn regressions_since_surfaces_spike() {
        let s = Storage::open_in_memory().expect("open");
        let conn = ConnectionInfo {
            server: "localhost,1433".into(),
            database: Some("master".into()),
            user: Some("sa".into()),
            password: Some("x".into()),
            trust_cert: Some(true),
        };
        let id = s.ensure_instance("t", &conn).expect("ensure");
        let mk = |mins_ago: i64, dur: i64| QueryStoreRow {
            captured_at: Utc::now() - chrono::Duration::minutes(mins_ago),
            query_id: 1,
            plan_id: 1,
            total_duration_ms: dur,
            cpu_ms: dur / 2,
            logical_reads: 1,
            executions: 10,
            query_sql_text: Some("SELECT 1".into()),
            last_execution_ms: Some(Utc::now().timestamp_millis()),
        };
        // 8 steady snapshots (~1000ms total / 10 execs = ~100ms each), newest last.
        for i in 0..8 {
            let dur = 1000 + (i % 2) * 10; // tiny jitter so stddev > 0
            s.insert_query_store_row(id, &mk(50 - i, dur)).expect("steady");
        }
        // Current snapshot spikes 10×.
        s.insert_query_store_row(id, &mk(1, 10_000)).expect("spike");

        let regs = s.regressions_since(TimeRange::last_hours(1)).expect("regressions");
        assert_eq!(regs.len(), 1, "the spiked query should be the sole regression");
        assert_eq!(regs[0].query_id, 1);
        assert!(
            regs[0].current_duration_ms > regs[0].baseline_duration_ms,
            "current must exceed the rolling-mean baseline"
        );
    }

    // ---- durable rolling baseline -----------------------------------------

    fn test_conn() -> ConnectionInfo {
        ConnectionInfo {
            server: "localhost,1433".into(),
            database: Some("master".into()),
            user: Some("sa".into()),
            password: Some("x".into()),
            trust_cert: Some(true),
        }
    }

    /// Welford's online accumulator must converge on the same mean/stddev as a
    /// batch computation, so the durable baseline is statistically sound.
    #[test]
    fn welford_fold_matches_batch_stats() {
        let samples = [100.0, 102.0, 98.0, 101.0, 99.0, 100.0, 103.0, 97.0];
        let mut b = QueryBaseline { query_id: 1, count: 0, mean: 0.0, m2: 0.0, last_value_ms: 0.0 };
        for &x in &samples {
            b = b.fold(x);
        }
        let n = samples.len() as f64;
        let mean = samples.iter().sum::<f64>() / n;
        let var = samples.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0);
        assert!((b.mean - mean).abs() < 1e-9, "mean {} vs {}", b.mean, mean);
        assert!((b.stddev().unwrap() - var.sqrt()).abs() < 1e-9);
        assert_eq!(b.count, 8);
        assert_eq!(b.last_value_ms, 97.0);
    }

    /// (1) The durable baseline, built from PERSISTED history across many polls,
    /// flags a later spike as a regression — and it survives a re-open of the
    /// same DB file (proving the baseline is durable, not in-window-only).
    #[test]
    fn durable_baseline_flags_regression_vs_persisted_history() {
        use std::env;
        let path = env::temp_dir().join(format!(
            "dbopt_durable_baseline_{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        let id;
        {
            let s = Storage::open(&path).expect("open");
            id = s.ensure_instance("t", &test_conn()).expect("ensure");
            // Feed 8 steady ~100ms samples across "polls". None should flag:
            // each is in-line with the baseline it's building.
            for i in 0..8 {
                let sample = 100.0 + (i % 2) as f64; // tiny jitter so stddev > 0
                let reg = s
                    .observe_and_detect_regression(id, 42, sample)
                    .expect("observe");
                assert!(reg.is_none(), "steady sample {i} must not flag");
            }
            let base = s.get_query_baseline(id, 42).expect("read").expect("exists");
            assert_eq!(base.count, 8, "all eight samples folded + persisted");
        }

        // Re-open the SAME file: the baseline must be loaded from disk, NOT
        // recomputed from a fresh window. A spike judged against it flags.
        {
            let s = Storage::open(&path).expect("reopen");
            let reg = s
                .observe_and_detect_regression(id, 42, 1000.0)
                .expect("observe spike")
                .expect("10x spike against persisted history must be flagged");
            assert_eq!(reg.query_id, 42);
            assert_eq!(reg.current_duration_ms, 1000);
            assert!(
                reg.baseline_duration_ms >= 99 && reg.baseline_duration_ms <= 101,
                "baseline is the persisted rolling mean, got {}",
                reg.baseline_duration_ms
            );
            assert!(reg.delta_pct > 800.0, "huge jump, got {}", reg.delta_pct);
        }

        let _ = std::fs::remove_file(&path);
    }

    /// A thin durable baseline (below the min-count) yields no verdict, so the
    /// caller falls back to the in-window z-score — graceful degradation.
    #[test]
    fn durable_baseline_below_min_count_returns_none() {
        let s = Storage::open_in_memory().expect("open");
        let id = s.ensure_instance("t", &test_conn()).expect("ensure");
        // Only 3 samples (< DURABLE_BASELINE_MIN_COUNT), then a spike.
        for _ in 0..3 {
            assert!(s.observe_and_detect_regression(id, 7, 100.0).expect("obs").is_none());
        }
        assert!(
            s.observe_and_detect_regression(id, 7, 5000.0).expect("obs").is_none(),
            "too little persisted history to judge — defer to in-window detector"
        );
    }

    /// (2) Pruning deletes durable baselines that haven't been refreshed since
    /// the cutoff, while fresh baselines survive — bounding table growth.
    #[test]
    fn prune_deletes_old_query_baselines() {
        let s = Storage::open_in_memory().expect("open");
        let id = s.ensure_instance("t", &test_conn()).expect("ensure");

        // Build two baselines via the normal update path (now-stamped).
        for _ in 0..3 {
            s.update_query_baseline(id, 1, 100.0).expect("stale qid");
            s.update_query_baseline(id, 2, 200.0).expect("fresh qid");
        }
        // Backdate query_id=1's last_updated_ms to 100 days ago.
        {
            let lock = s.conn.lock().expect("lock");
            let old_ms = (Utc::now() - chrono::Duration::days(100)).timestamp_millis();
            lock.execute(
                "UPDATE query_baseline SET last_updated_ms = ?1 WHERE query_id = 1",
                [old_ms],
            )
            .expect("backdate");
        }

        let cutoff = Utc::now() - chrono::Duration::days(90);
        let removed = s.prune_query_baselines_before(cutoff).expect("prune");
        assert_eq!(removed, 1, "only the stale baseline should be pruned");
        assert!(
            s.get_query_baseline(id, 1).expect("read").is_none(),
            "stale baseline gone"
        );
        assert!(
            s.get_query_baseline(id, 2).expect("read").is_some(),
            "fresh baseline survives"
        );

        // prune_before should also sweep stale baselines (integrated retention).
        s.update_query_baseline(id, 3, 300.0).expect("another");
        {
            let lock = s.conn.lock().expect("lock");
            let old_ms = (Utc::now() - chrono::Duration::days(200)).timestamp_millis();
            lock.execute(
                "UPDATE query_baseline SET last_updated_ms = ?1 WHERE query_id = 3",
                [old_ms],
            )
            .expect("backdate3");
        }
        let removed2 = s.prune_before(Utc::now() - chrono::Duration::days(90)).expect("prune_before");
        assert!(removed2 >= 1, "prune_before also ages out stale baselines, got {removed2}");
        assert!(s.get_query_baseline(id, 3).expect("read").is_none());
    }

    /// The 0007 deep-vitals tables exist after migration and every insert path
    /// round-trips. We read the rows back by hand (these tables are write-only
    /// from Rust today; the assertions just prove the schema + binds line up).
    #[test]
    fn vitals_inserts_round_trip() {
        let s = Storage::open_in_memory().expect("open");
        let id = s.ensure_instance("t", &test_conn()).expect("ensure");
        let now = Utc::now();

        s.insert_cpu_pressure(id, &CpuPressureRow {
            captured_at: now,
            online_schedulers: 8,
            runnable_tasks: 3,
            work_queue: 1,
            current_workers: 40,
            active_workers: 12,
            pending_disk_io: 2,
        }).expect("cpu");

        s.insert_memory_headroom(id, &MemoryHeadroomRow {
            captured_at: now,
            page_life_expectancy: 1200,
            pending_memory_grants: 0,
            granted_memory_kb: 4096,
            target_server_memory_kb: 8_000_000,
            total_server_memory_kb: 7_500_000,
        }).expect("mem");

        s.insert_io_latency(id, &IoLatencyRow {
            captured_at: now,
            database_name: "pharma".into(),
            file_logical_name: "appdb_data".into(),
            file_type: "ROWS".into(),
            reads_delta: 100,
            writes_delta: 50,
            read_stall_ms_delta: 900,
            write_stall_ms_delta: 250,
            avg_read_latency_ms: 9.0,
            avg_write_latency_ms: 5.0,
        }).expect("io");

        s.insert_tempdb_contention(id, &TempdbContentionRow {
            captured_at: now,
            pagelatch_waiters: 5,
            pfs_waiters: 3,
            gam_waiters: 1,
            sgam_waiters: 1,
            total_wait_ms: 320,
            tempdb_data_files: 1,
        }).expect("tempdb");

        s.insert_plan_cache(id, &PlanCacheRow {
            captured_at: now,
            single_use_plan_count: 5000,
            single_use_size_kb: 250_000,
            total_plan_count: 7000,
            total_size_kb: 400_000,
        }).expect("plan");

        let lock = s.conn.lock().expect("lock");
        let count = |t: &str| -> i64 {
            lock.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0)).expect("count")
        };
        assert_eq!(count("cpu_pressure_snapshot"), 1);
        assert_eq!(count("memory_headroom_snapshot"), 1);
        assert_eq!(count("io_latency_delta"), 1);
        assert_eq!(count("tempdb_contention_snapshot"), 1);
        assert_eq!(count("plan_cache_snapshot"), 1);

        // The avg-read latency we persisted must come back as the same float.
        let avg: f64 = lock
            .query_row("SELECT avg_read_latency_ms FROM io_latency_delta", [], |r| r.get(0))
            .expect("avg");
        assert!((avg - 9.0).abs() < 1e-9, "io latency round-trips, got {avg}");
    }

    /// Read-back path: insert one sample on each deep-vitals surface, then read
    /// the most-recent of each through the new `latest_*` methods and the
    /// read-only `get_instance_id` lookup. This is what the live "DEEP VITALS"
    /// API surfaces, so a round-trip here proves the column mapping + ordering.
    #[test]
    fn vitals_read_back_latest() {
        let s = Storage::open_in_memory().expect("open");
        let conn = ConnectionInfo {
            server: "vitals-host,1433".into(),
            database: Some("master".into()),
            user: Some("sa".into()),
            password: Some("x".into()),
            trust_cert: Some(true),
        };
        let id = s.ensure_instance("vitals-test", &conn).expect("ensure");

        // Read-only lookup resolves by SERVER and never creates a row.
        assert_eq!(s.get_instance_id("vitals-host,1433"), Some(id));
        assert!(s.get_instance_id("not-monitored,1433").is_none());
        let before = s.instance_count().expect("count");
        let _ = s.get_instance_id("still-not-monitored,1433");
        assert_eq!(s.instance_count().expect("count"), before, "read must not create instances");

        // Nothing captured yet → every surface is empty.
        assert!(s.latest_cpu_pressure(id).is_none());
        assert!(s.latest_io_latency(id).is_empty());

        let older = Utc::now() - chrono::Duration::minutes(5);
        let newer = Utc::now() - chrono::Duration::minutes(1);

        // Two CPU samples — the newer one must win.
        s.insert_cpu_pressure(id, &CpuPressureRow {
            captured_at: older, online_schedulers: 8, runnable_tasks: 0, work_queue: 0,
            current_workers: 20, active_workers: 5, pending_disk_io: 0,
        }).expect("cpu old");
        s.insert_cpu_pressure(id, &CpuPressureRow {
            captured_at: newer, online_schedulers: 8, runnable_tasks: 7, work_queue: 2,
            current_workers: 40, active_workers: 12, pending_disk_io: 3,
        }).expect("cpu new");
        let cpu = s.latest_cpu_pressure(id).expect("cpu latest");
        assert_eq!(cpu.runnable_tasks, 7, "most-recent CPU sample wins");
        assert_eq!(cpu.work_queue, 2);

        s.insert_memory_headroom(id, &MemoryHeadroomRow {
            captured_at: newer, page_life_expectancy: 850, pending_memory_grants: 1,
            granted_memory_kb: 4096, target_server_memory_kb: 8_000_000, total_server_memory_kb: 7_900_000,
        }).expect("mem");
        let mem = s.latest_memory_headroom(id).expect("mem latest");
        assert_eq!(mem.page_life_expectancy, 850);
        assert_eq!(mem.pending_memory_grants, 1);

        // IO: two files at the SAME (newest) instant + one stale file earlier.
        s.insert_io_latency(id, &IoLatencyRow {
            captured_at: older, database_name: "db".into(), file_logical_name: "stale".into(),
            file_type: "ROWS".into(), reads_delta: 1, writes_delta: 1, read_stall_ms_delta: 1,
            write_stall_ms_delta: 1, avg_read_latency_ms: 1.0, avg_write_latency_ms: 1.0,
        }).expect("io stale");
        s.insert_io_latency(id, &IoLatencyRow {
            captured_at: newer, database_name: "db".into(), file_logical_name: "data".into(),
            file_type: "ROWS".into(), reads_delta: 100, writes_delta: 10, read_stall_ms_delta: 900,
            write_stall_ms_delta: 50, avg_read_latency_ms: 9.0, avg_write_latency_ms: 5.0,
        }).expect("io data");
        s.insert_io_latency(id, &IoLatencyRow {
            captured_at: newer, database_name: "db".into(), file_logical_name: "log".into(),
            file_type: "LOG".into(), reads_delta: 0, writes_delta: 200, read_stall_ms_delta: 0,
            write_stall_ms_delta: 400, avg_read_latency_ms: 0.0, avg_write_latency_ms: 2.0,
        }).expect("io log");
        let io = s.latest_io_latency(id);
        assert_eq!(io.len(), 2, "only the two files at the newest instant, not the stale one");
        // Ordered by combined latency DESC — the high-read 'data' file leads.
        assert_eq!(io[0].file_logical_name, "data");
        assert!((io[0].avg_read_latency_ms - 9.0).abs() < 1e-9);

        s.insert_tempdb_contention(id, &TempdbContentionRow {
            captured_at: newer, pagelatch_waiters: 4, pfs_waiters: 3, gam_waiters: 1,
            sgam_waiters: 0, total_wait_ms: 220, tempdb_data_files: 1,
        }).expect("tempdb");
        let td = s.latest_tempdb_contention(id).expect("tempdb latest");
        assert_eq!(td.pfs_waiters, 3);
        assert_eq!(td.tempdb_data_files, 1);

        s.insert_plan_cache(id, &PlanCacheRow {
            captured_at: newer, single_use_plan_count: 6000, single_use_size_kb: 300_000,
            total_plan_count: 7000, total_size_kb: 400_000,
        }).expect("plan");
        let pc = s.latest_plan_cache(id).expect("plan latest");
        assert_eq!(pc.single_use_plan_count, 6000);
        assert_eq!(pc.total_plan_count, 7000);
    }

    /// The cumulative per-file IO snapshot used by the IO-latency delta poller
    /// persists across calls (JSON in poller_state) and restores exactly.
    #[test]
    fn io_file_snapshot_round_trip() {
        let s = Storage::open_in_memory().expect("open");
        let id = s.ensure_instance("t", &test_conn()).expect("ensure");

        // First observation: nothing stored yet → None (poller seeds + skips).
        assert!(s.previous_io_file_snapshot(id).is_none());

        let mut snap: HashMap<(String, String), (i64, i64, i64, i64)> = HashMap::new();
        snap.insert(("db1".into(), "f1".into()), (10, 20, 300, 400));
        snap.insert(("db1".into(), "f2".into()), (1, 2, 3, 4));
        s.update_io_file_snapshot(id, &snap).expect("save");

        let restored = s.previous_io_file_snapshot(id).expect("restore");
        assert_eq!(restored.get(&("db1".into(), "f1".into())), Some(&(10, 20, 300, 400)));
        assert_eq!(restored.get(&("db1".into(), "f2".into())), Some(&(1, 2, 3, 4)));
    }

    /// Deep-vitals rows must age out with the other telemetry so the SQLite
    /// store stays bounded — prune drops old vitals, keeps fresh ones.
    #[test]
    fn prune_drops_old_vitals() {
        let s = Storage::open_in_memory().expect("open");
        let id = s.ensure_instance("t", &test_conn()).expect("ensure");

        let old = Utc::now() - chrono::Duration::days(100);
        let fresh = Utc::now() - chrono::Duration::minutes(1);
        for &when in &[old, fresh] {
            s.insert_cpu_pressure(id, &CpuPressureRow {
                captured_at: when,
                online_schedulers: 4,
                runnable_tasks: 0,
                work_queue: 0,
                current_workers: 10,
                active_workers: 2,
                pending_disk_io: 0,
            }).expect("cpu");
        }

        let removed = s.prune_before(Utc::now() - chrono::Duration::days(90)).expect("prune");
        assert!(removed >= 1, "old vitals row should be pruned, removed={removed}");
        let remaining: i64 = {
            let lock = s.conn.lock().expect("lock");
            lock.query_row("SELECT COUNT(*) FROM cpu_pressure_snapshot", [], |r| r.get(0)).expect("count")
        };
        assert_eq!(remaining, 1, "only the fresh vitals row survives");
    }
}
