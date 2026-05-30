//! Wait-stats delta poller.
//!
//! `sys.dm_os_wait_stats` is a monotonic counter since the last instance
//! restart (or `DBCC SQLPERF` reset). A snapshot in isolation is useless; what
//! we want is "how much waited in the last tick". We do that by stashing the
//! previous totals in the Storage layer and subtracting on each pass.

use std::collections::HashMap;

use chrono::Utc;

use crate::{
    conn,
    storage::{ignorable_waits_in_clause, Storage, WaitDeltaRow},
    ConnectionInfo,
};

/// Compute per-wait-type deltas against the prior tick and insert the
/// non-zero rows. The very first tick on a freshly-started sentinel simply
/// seeds the snapshot and inserts nothing.
pub async fn poll_wait_stats(conn_info: &ConnectionInfo, storage: &Storage) -> anyhow::Result<()> {
    let mut client = conn::open(conn_info).await?;
    let instance_id: i64 = storage.ensure_instance(&conn_info.server, conn_info)?;

    // Exclude the canonical benign/idle waits (shared with the top-wait pick) so
    // background scheduler noise never lands in the time-series or the grade.
    let sql = format!(
        r#"
        SELECT TOP (30)
            wait_type,
            CAST(waiting_tasks_count AS BIGINT) AS waiting_tasks_count,
            CAST(wait_time_ms        AS BIGINT) AS wait_time_ms,
            CAST(signal_wait_time_ms AS BIGINT) AS signal_wait_ms
        FROM sys.dm_os_wait_stats
        WHERE wait_type NOT IN ({})
          AND waiting_tasks_count > 0
        ORDER BY wait_time_ms DESC;
    "#,
        ignorable_waits_in_clause()
    );

    // Degrade gracefully when the DMV is unavailable (Azure SQL DB exposes a
    // different shape, and a login without VIEW SERVER STATE can't read it) —
    // log once and skip, mirroring deadlocks.rs / query_store.rs, rather than
    // erroring on every tick.
    let stream = match client.simple_query(&sql).await {
        Ok(s) => s,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("dm_os_wait_stats")
                || msg.contains("Invalid object name")
                || msg.contains("VIEW SERVER STATE")
                || msg.contains("permission")
            {
                tracing::warn!(
                    target: "sentinel::poll::waits",
                    "wait stats unavailable on {} (missing DMV or VIEW SERVER STATE): {msg}",
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
                target: "sentinel::poll::waits",
                "wait stats stream collection failed on {}: {e}",
                conn_info.server
            );
            return Ok(());
        }
    };

    // (waiting_tasks_count, wait_time_ms, signal_wait_ms)
    let mut current: HashMap<String, (i64, i64, i64)> = HashMap::new();
    for r in rows {
        let wait_type = match r.get::<&str, _>(0) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        let tasks = r.get::<i64, _>(1).unwrap_or(0);
        let total = r.get::<i64, _>(2).unwrap_or(0);
        let signal = r.get::<i64, _>(3).unwrap_or(0);
        current.insert(wait_type, (tasks, total, signal));
    }

    let prior: Option<HashMap<String, (i64, i64, i64)>> =
        storage.previous_wait_snapshot(instance_id);
    let captured_at = Utc::now();
    let mut count = 0usize;

    match prior {
        Some(prev) => {
            for (wait_type, (cur_tasks, cur_total, cur_signal)) in current.iter() {
                let (prev_tasks, prev_total, prev_signal): (i64, i64, i64) =
                    prev.get(wait_type).copied().unwrap_or((0, 0, 0));
                let tasks_delta: i64 = cur_tasks - prev_tasks;
                let total_delta: i64 = cur_total - prev_total;
                let signal_delta: i64 = cur_signal - prev_signal;
                // Skip when nothing positive moved — wait_time can stall while
                // tasks tick up by 1, so we keep a row if any delta > 0.
                if tasks_delta <= 0 && total_delta <= 0 && signal_delta <= 0 {
                    continue;
                }
                let row = WaitDeltaRow {
                    captured_at,
                    wait_type: wait_type.clone(),
                    waiting_tasks_count_delta: tasks_delta.max(0),
                    wait_time_ms_delta: total_delta.max(0),
                    signal_wait_ms_delta: signal_delta.max(0),
                };
                if let Err(e) = storage.insert_wait_delta(instance_id, &row) {
                    tracing::warn!(
                        target: "sentinel::poll::waits",
                        "insert_wait_delta failed for {}: {e:#}",
                        row.wait_type
                    );
                    continue;
                }
                count += 1;
            }
            tracing::info!(target: "sentinel::poll::waits", "captured {count} rows");
        }
        None => {
            tracing::info!(
                target: "sentinel::poll::waits",
                "first observation for instance {}, seeded snapshot with {} wait types",
                conn_info.server,
                current.len()
            );
        }
    }

    if let Err(e) = storage.update_wait_snapshot(instance_id, &current) {
        tracing::warn!(
            target: "sentinel::poll::waits",
            "update_wait_snapshot failed: {e:#}"
        );
    }

    Ok(())
}
