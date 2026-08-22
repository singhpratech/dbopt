//! CPU / scheduler-pressure poller.
//!
//! `sys.dm_os_schedulers` exposes one row per logical scheduler. The
//! VISIBLE ONLINE schedulers are the ones actually running the workload; the
//! HIDDEN / OFFLINE / DAC ones are housekeeping and would skew the picture.
//!
//! The headline signal is `runnable_tasks_count`: workers that are READY to
//! run but waiting their turn on a CPU. Sustained non-zero runnable tasks
//! (relative to scheduler count) is the textbook "CPU PRESSURE" symptom —
//! more so than raw CPU %. `work_queue_count` (tasks with no worker yet) and
//! `pending_disk_io_count` round out the scheduler view.
//!
//! Each tick is an instantaneous snapshot (no delta needed — these are gauges,
//! not monotonic counters). Degrades gracefully without VIEW SERVER STATE.

use chrono::Utc;

use crate::{
    conn,
    storage::{CpuPressureRow, Storage},
    ConnectionInfo,
};

// Sum the scheduler gauges over the schedulers that actually run user work.
// Everything is CAST to BIGINT so tiberius reads each column as one i64 and we
// never trip over INT-vs-BIGINT surprises.
const CPU_PRESSURE_QUERY: &str = r#"
    SELECT
        CAST(COUNT(*)                         AS BIGINT) AS online_schedulers,
        CAST(SUM(runnable_tasks_count)        AS BIGINT) AS runnable_tasks,
        CAST(SUM(work_queue_count)            AS BIGINT) AS work_queue,
        CAST(SUM(current_workers_count)       AS BIGINT) AS current_workers,
        CAST(SUM(active_workers_count)        AS BIGINT) AS active_workers,
        CAST(SUM(pending_disk_io_count)       AS BIGINT) AS pending_disk_io
    FROM sys.dm_os_schedulers
    WHERE status = 'VISIBLE ONLINE';
"#;

/// True when the error means the DMV is unavailable or we lack VIEW SERVER
/// STATE — in which case we log once and skip the tick instead of erroring.
fn is_unavailable(msg: &str) -> bool {
    msg.contains("VIEW SERVER STATE")
        || msg.contains("permission")
        || msg.contains("Invalid object name")
        || msg.contains("dm_os_schedulers")
}

/// Snapshot scheduler pressure summed over VISIBLE ONLINE schedulers.
pub async fn poll_cpu_pressure(
    conn_info: &ConnectionInfo,
    storage: &Storage,
) -> anyhow::Result<()> {
    let mut client = conn::open(conn_info).await?;
    let instance_id = storage.ensure_instance(&conn_info.server, conn_info)?;

    let stream = match client.simple_query(crate::probes::tag(CPU_PRESSURE_QUERY)).await {
        Ok(s) => s,
        Err(e) => {
            let msg = e.to_string();
            if is_unavailable(&msg) {
                tracing::warn!(
                    target: "sentinel::poll::cpu_pressure",
                    "scheduler stats unavailable on {} (missing DMV or VIEW SERVER STATE): {msg}",
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
                target: "sentinel::poll::cpu_pressure",
                "scheduler stats stream collection failed on {}: {e}",
                conn_info.server
            );
            return Ok(());
        }
    };

    let Some(r) = rows.into_iter().next() else {
        return Ok(());
    };

    let row = CpuPressureRow {
        captured_at: Utc::now(),
        online_schedulers: r.get::<i64, _>(0).unwrap_or(0),
        runnable_tasks: r.get::<i64, _>(1).unwrap_or(0),
        work_queue: r.get::<i64, _>(2).unwrap_or(0),
        current_workers: r.get::<i64, _>(3).unwrap_or(0),
        active_workers: r.get::<i64, _>(4).unwrap_or(0),
        pending_disk_io: r.get::<i64, _>(5).unwrap_or(0),
    };

    if let Err(e) = storage.insert_cpu_pressure(instance_id, &row) {
        tracing::warn!(
            target: "sentinel::poll::cpu_pressure",
            "insert_cpu_pressure failed: {e:#}"
        );
        return Ok(());
    }

    tracing::info!(
        target: "sentinel::poll::cpu_pressure",
        "captured runnable={} work_queue={} over {} schedulers",
        row.runnable_tasks, row.work_queue, row.online_schedulers
    );
    Ok(())
}
