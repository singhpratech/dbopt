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

    const SQL: &str = r#"
        SELECT TOP (50)
            q.query_id,
            p.plan_id,
            CAST(SUM(rs.avg_duration * rs.count_executions) / 1000 AS BIGINT) AS total_duration_ms,
            CAST(SUM(rs.avg_cpu_time   * rs.count_executions) / 1000 AS BIGINT) AS cpu_ms,
            CAST(SUM(rs.avg_logical_io_reads * rs.count_executions) AS BIGINT) AS logical_reads,
            CAST(SUM(rs.count_executions) AS BIGINT) AS executions
        FROM sys.query_store_query AS q
        JOIN sys.query_store_plan  AS p  ON p.query_id = q.query_id
        JOIN sys.query_store_runtime_stats AS rs ON rs.plan_id = p.plan_id
        JOIN sys.query_store_runtime_stats_interval AS i ON i.runtime_stats_interval_id = rs.runtime_stats_interval_id
        WHERE i.end_time >= DATEADD(hour, -1, SYSUTCDATETIME())
        GROUP BY q.query_id, p.plan_id
        ORDER BY total_duration_ms DESC;
    "#;

    let stream = match client.simple_query(SQL).await {
        Ok(s) => s,
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
        };
        if let Err(e) = storage.insert_query_store_row(instance_id, &row) {
            tracing::warn!(
                target: "sentinel::poll::query_store",
                "insert_query_store_row failed for query_id={}: {e:#}",
                row.query_id
            );
            continue;
        }
        count += 1;
    }

    tracing::info!(
        target: "sentinel::poll::query_store",
        "captured {count} rows"
    );
    Ok(())
}
