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

/// One row of "top queries by duration" — what the report uses.
#[derive(Debug, Clone, Serialize)]
pub struct TopQueryRow {
    pub query_id: i64,
    pub plan_id: i64,
    pub total_duration_ms: i64,
    pub executions: i64,
    pub query_sql_text: Option<String>,
}

/// One row of "regression detected" — query got slower across the window.
#[derive(Debug, Clone, Serialize)]
pub struct RegressionRow {
    pub query_id: i64,
    pub baseline_duration_ms: i64,
    pub current_duration_ms: i64,
    pub delta_pct: f64,
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
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        let mut stmt = lock.prepare(
            "SELECT query_id, plan_id,
                    SUM(total_duration_ms) AS total_duration_ms,
                    SUM(executions)        AS executions,
                    MAX(query_sql_text)    AS query_sql_text
             FROM query_store_snapshot
             WHERE captured_at >= ?1 AND captured_at < ?2
             GROUP BY query_id, plan_id
             ORDER BY total_duration_ms DESC
             LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![window.from_ms(), window.to_ms(), n as i64], |r| {
                Ok(TopQueryRow {
                    query_id: r.get(0)?,
                    plan_id: r.get(1)?,
                    total_duration_ms: r.get(2)?,
                    executions: r.get(3)?,
                    query_sql_text: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Regressions: queries whose current-half-window duration is ≥2× the
    /// baseline-half, with ≥10 executions in both halves.
    pub fn regressions_since(&self, window: TimeRange) -> anyhow::Result<Vec<RegressionRow>> {
        let half = (window.to_ms() - window.from_ms()) / 2;
        let mid = window.from_ms() + half;
        let lock = self.conn.lock().expect("sentinel storage mutex poisoned");
        let mut stmt = lock.prepare(
            "WITH baseline AS (
                 SELECT query_id, SUM(total_duration_ms) AS d, SUM(executions) AS e
                 FROM query_store_snapshot
                 WHERE captured_at >= ?1 AND captured_at < ?2
                 GROUP BY query_id
             ),
             current AS (
                 SELECT query_id, SUM(total_duration_ms) AS d, SUM(executions) AS e
                 FROM query_store_snapshot
                 WHERE captured_at >= ?2 AND captured_at < ?3
                 GROUP BY query_id
             )
             SELECT b.query_id, b.d AS baseline_ms, c.d AS current_ms
             FROM baseline b
             JOIN current  c ON c.query_id = b.query_id
             WHERE b.d > 0 AND b.e >= 10 AND c.e >= 10 AND CAST(c.d AS REAL) / b.d >= 2.0
             ORDER BY (c.d - b.d) DESC
             LIMIT 50",
        )?;
        let rows = stmt
            .query_map(params![window.from_ms(), mid, window.to_ms()], |r| {
                let baseline: i64 = r.get(1)?;
                let current: i64 = r.get(2)?;
                let delta_pct = if baseline == 0 { 0.0 } else { (current as f64 / baseline as f64 - 1.0) * 100.0 };
                Ok(RegressionRow {
                    query_id: r.get(0)?,
                    baseline_duration_ms: baseline,
                    current_duration_ms: current,
                    delta_pct,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
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
}
