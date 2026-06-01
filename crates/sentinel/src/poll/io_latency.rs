//! File-IO latency poller (delta-based).
//!
//! `sys.dm_io_virtual_file_stats` is cumulative since instance restart: a
//! single snapshot tells you lifetime latency, not "what is storage doing right
//! now". So we diff the current cumulative reading against the prior tick
//! (stashed in poller_state) and derive the *average* read/write latency for
//! just that window — `read_stall_ms_delta / reads_delta`. That is the number a
//! DBA cares about: < ~10ms is healthy, sustained tens-to-hundreds of ms is an
//! IO bottleneck.
//!
//! One row per (database, logical file) that actually did IO in the window.
//! The first tick on a fresh sentinel seeds the snapshot and inserts nothing.
//! Degrades gracefully without VIEW SERVER STATE.

use std::collections::HashMap;

use chrono::Utc;

use crate::{
    conn,
    storage::{IoLatencyRow, Storage},
    ConnectionInfo,
};

// num_of_reads / num_of_writes / io_stall_read_ms / io_stall_write_ms are
// bigint in the DMV; CAST anyway for tiberius clarity. We join master files
// for friendly names and only look at ROWS (data) and LOG file types.
const IO_STATS_QUERY: &str = r#"
    SELECT
        DB_NAME(vfs.database_id)                       AS database_name,
        CAST(mf.name AS NVARCHAR(128))                 AS file_logical_name,
        CAST(mf.type_desc AS NVARCHAR(60))             AS file_type,
        CAST(vfs.num_of_reads    AS BIGINT)            AS num_of_reads,
        CAST(vfs.num_of_writes   AS BIGINT)            AS num_of_writes,
        CAST(vfs.io_stall_read_ms  AS BIGINT)          AS io_stall_read_ms,
        CAST(vfs.io_stall_write_ms AS BIGINT)          AS io_stall_write_ms
    FROM sys.dm_io_virtual_file_stats(NULL, NULL) AS vfs
    JOIN sys.master_files AS mf
        ON mf.database_id = vfs.database_id
       AND mf.file_id     = vfs.file_id;
"#;

fn is_unavailable(msg: &str) -> bool {
    msg.contains("VIEW SERVER STATE")
        || msg.contains("permission")
        || msg.contains("Invalid object name")
        || msg.contains("dm_io_virtual_file_stats")
}

/// Sample cumulative per-file IO and persist the per-window deltas + derived
/// average latencies versus the prior tick.
pub async fn poll_io_latency(conn_info: &ConnectionInfo, storage: &Storage) -> anyhow::Result<()> {
    let mut client = conn::open(conn_info).await?;
    let instance_id = storage.ensure_instance(&conn_info.server, conn_info)?;

    let stream = match client.simple_query(IO_STATS_QUERY).await {
        Ok(s) => s,
        Err(e) => {
            let msg = e.to_string();
            if is_unavailable(&msg) {
                tracing::warn!(
                    target: "sentinel::poll::io_latency",
                    "file IO stats unavailable on {} (missing DMV or VIEW SERVER STATE): {msg}",
                    conn_info.server
                );
                return Ok(());
            }
            return Err(e.into());
        }
    };
    let rows = match stream.into_first_result().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                target: "sentinel::poll::io_latency",
                "file IO stats stream collection failed on {}: {e}",
                conn_info.server
            );
            return Ok(());
        }
    };

    // current cumulative reading keyed by (db, logical file name)
    // value = (reads, writes, read_stall_ms, write_stall_ms)
    let mut current: HashMap<(String, String), (i64, i64, i64, i64)> =
        HashMap::with_capacity(rows.len());
    // Keep the file_type alongside so we can label persisted rows.
    let mut file_types: HashMap<(String, String), String> = HashMap::new();
    for r in &rows {
        let db = r.get::<&str, _>(0).unwrap_or("").to_string();
        let file = r.get::<&str, _>(1).unwrap_or("").to_string();
        if db.is_empty() && file.is_empty() {
            continue;
        }
        let ftype = r.get::<&str, _>(2).unwrap_or("").to_string();
        let reads = r.get::<i64, _>(3).unwrap_or(0);
        let writes = r.get::<i64, _>(4).unwrap_or(0);
        let read_stall = r.get::<i64, _>(5).unwrap_or(0);
        let write_stall = r.get::<i64, _>(6).unwrap_or(0);
        let key = (db, file);
        file_types.insert(key.clone(), ftype);
        current.insert(key, (reads, writes, read_stall, write_stall));
    }

    let prior = storage.previous_io_file_snapshot(instance_id);
    let captured_at = Utc::now();
    let mut inserted = 0usize;

    if let Some(prior) = prior {
        for (key, cur) in &current {
            let (reads_c, writes_c, rstall_c, wstall_c) = *cur;
            let (reads_p, writes_p, rstall_p, wstall_p): (i64, i64, i64, i64) =
                prior.get(key).copied().unwrap_or((0, 0, 0, 0));

            // Counters reset on restart; clamp negatives to 0.
            let reads_delta = (reads_c - reads_p).max(0);
            let writes_delta = (writes_c - writes_p).max(0);
            let rstall_delta = (rstall_c - rstall_p).max(0);
            let wstall_delta = (wstall_c - wstall_p).max(0);

            // No IO this window for this file → nothing interesting to persist.
            if reads_delta == 0 && writes_delta == 0 {
                continue;
            }

            let avg_read = if reads_delta > 0 {
                rstall_delta as f64 / reads_delta as f64
            } else {
                0.0
            };
            let avg_write = if writes_delta > 0 {
                wstall_delta as f64 / writes_delta as f64
            } else {
                0.0
            };

            let row = IoLatencyRow {
                captured_at,
                database_name: key.0.clone(),
                file_logical_name: key.1.clone(),
                file_type: file_types.get(key).cloned().unwrap_or_default(),
                reads_delta,
                writes_delta,
                read_stall_ms_delta: rstall_delta,
                write_stall_ms_delta: wstall_delta,
                avg_read_latency_ms: avg_read,
                avg_write_latency_ms: avg_write,
            };
            if let Err(e) = storage.insert_io_latency(instance_id, &row) {
                tracing::warn!(
                    target: "sentinel::poll::io_latency",
                    "insert failed for {}/{}: {e:#}",
                    row.database_name, row.file_logical_name
                );
                continue;
            }
            inserted += 1;
        }
    }

    if let Err(e) = storage.update_io_file_snapshot(instance_id, &current) {
        tracing::warn!(
            target: "sentinel::poll::io_latency",
            "snapshot save failed: {e:#}"
        );
    }

    tracing::info!(target: "sentinel::poll::io_latency", "captured {inserted} rows");
    Ok(())
}
