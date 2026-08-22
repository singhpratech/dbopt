//! Query Store rollup poller.
//!
//! Pulls the top-N (50) queries by total duration over the trailing hour from
//! `sys.query_store_*` and persists each row via `Storage::insert_query_store_row`.
//! Requires Query Store to be ON on the target database. If it isn't, the
//! catalog views won't exist and we degrade to a single warning per tick.

use chrono::Utc;

use crate::{
    conn,
    storage::{QueryStoreRow, Storage},
    ConnectionInfo,
};

/// Top-50 Query Store rollup. Returns `Ok(())` even when zero rows match
/// (a freshly-restarted instance has no runtime stats yet) or when Query
/// Store is disabled.
pub async fn poll_query_store(conn_info: &ConnectionInfo, storage: &Storage) -> anyhow::Result<()> {
    let mut client = conn::open(conn_info).await?;
    let instance_id: i64 = storage.ensure_instance(&conn_info.server, conn_info)?;

    // We join query_store_query_text so each row carries the actual T-SQL, not
    // just an opaque id. NOTE: MAX() rejects nvarchar(max), so we LEFT-truncate
    // and CAST to a bounded NVARCHAR *before* aggregating (also what tiberius
    // needs to read it as a string — reading a MAX type silently yields NULL).
    // `probes::not_own_probe_sql` drops dbopt's own polling queries (by tag and
    // by the DMV names they reference) so the feed shows the user's workload.
    let sql = format!(r#"
        SELECT TOP (50)
            q.query_id,
            p.plan_id,
            CAST(SUM(rs.avg_duration * rs.count_executions) / 1000 AS BIGINT) AS total_duration_ms,
            CAST(SUM(rs.avg_cpu_time   * rs.count_executions) / 1000 AS BIGINT) AS cpu_ms,
            CAST(SUM(rs.avg_logical_io_reads * rs.count_executions) AS BIGINT) AS logical_reads,
            CAST(SUM(rs.count_executions) AS BIGINT) AS executions,
            MAX(CAST(LEFT(qt.query_sql_text, 1000) AS NVARCHAR(1000))) AS query_sql_text,
            DATEDIFF_BIG(MILLISECOND, '1970-01-01', CAST(MAX(rs.last_execution_time) AS DATETIME2)) AS last_execution_ms
        FROM sys.query_store_query AS q
        JOIN sys.query_store_query_text AS qt ON qt.query_text_id = q.query_text_id
        JOIN sys.query_store_plan  AS p  ON p.query_id = q.query_id
        JOIN sys.query_store_runtime_stats AS rs ON rs.plan_id = p.plan_id
        JOIN sys.query_store_runtime_stats_interval AS i ON i.runtime_stats_interval_id = rs.runtime_stats_interval_id
        WHERE i.end_time >= DATEADD(hour, -1, SYSUTCDATETIME())
          AND {probe_filter}
        GROUP BY q.query_id, p.plan_id
        ORDER BY total_duration_ms DESC;
    "#, probe_filter = crate::probes::not_own_probe_sql("qt.query_sql_text"));

    let stream = match client.simple_query(crate::probes::tag(&sql)).await {
        Ok(s) => s,
        Err(e) => {
            let msg = format!("{e}");
            // Match on shape, not on one object name: any "this object/function
            // does not exist here" error means Query Store is unavailable on
            // this target, and turning that into a hard poll error every tick
            // buries the log for a condition the operator cannot fix.
            let unavailable = msg.contains("query_store_query")
                || msg.contains("Invalid object name")
                || msg.contains("is not a recognized built-in function name")
                || msg.contains("Invalid column name")
                || msg.contains("Could not find stored procedure");
            if unavailable {
                tracing::warn!(
                    target: "sentinel::poll::query_store",
                    "Query Store appears to be disabled on {}: {msg}",
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
            let msg = format!("{e}");
            if msg.contains("query_store_query") || msg.contains("Invalid object name") {
                tracing::warn!(
                    target: "sentinel::poll::query_store",
                    "Query Store appears to be disabled on {}: {msg}",
                    conn_info.server
                );
                return Ok(());
            }
            return Err(e.into());
        }
    };

    let captured_at = Utc::now();
    let mut count = 0usize;
    for r in rows {
        let row = QueryStoreRow {
            captured_at,
            query_id:          r.get::<i64, _>(0).unwrap_or(0),
            plan_id:           r.get::<i64, _>(1).unwrap_or(0),
            total_duration_ms: r.get::<i64, _>(2).unwrap_or(0),
            cpu_ms:            r.get::<i64, _>(3).unwrap_or(0),
            logical_reads:     r.get::<i64, _>(4).unwrap_or(0),
            executions:        r.get::<i64, _>(5).unwrap_or(0),
            query_sql_text:    r.get::<&str, _>(6).map(|s| s.to_string()),
            last_execution_ms: r.get::<i64, _>(7),
        };
        if let Err(e) = storage.insert_query_store_row(instance_id, &row) {
            tracing::warn!(
                target: "sentinel::poll::query_store",
                "insert_query_store_row failed for query_id={}: {e:#}",
                row.query_id
            );
            continue;
        }
        // Fold this sample into the DURABLE rolling baseline (persisted across
        // windows + restarts) and flag a regression if it's a clear outlier
        // versus that query's established history. A thin baseline returns None
        // here and the report's in-window z-score still covers the query.
        if row.executions > 0 {
            let sample = row.total_duration_ms as f64 / row.executions as f64;
            match storage.observe_and_detect_regression(instance_id, row.query_id, sample) {
                Ok(Some(reg)) => tracing::info!(
                    target: "sentinel::poll::query_store",
                    "durable baseline regression query_id={} baseline={}ms current={}ms (+{:.0}%)",
                    reg.query_id, reg.baseline_duration_ms, reg.current_duration_ms, reg.delta_pct
                ),
                Ok(None) => {}
                Err(e) => tracing::warn!(
                    target: "sentinel::poll::query_store",
                    "durable baseline update failed for query_id={}: {e:#}",
                    row.query_id
                ),
            }
        }
        count += 1;
    }

    tracing::info!(
        target: "sentinel::poll::query_store",
        "captured {count} rows"
    );
    Ok(())
}
