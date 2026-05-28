//! Periodically snapshots per-index storage footprint (rows, reserved/used/data KB).
//!
//! Unlike index usage these values are not cumulative counters — they describe
//! the *current* shape of the database — so we sample them at intervals and
//! store each row as-is. No delta needed.

use chrono::Utc;

use crate::{conn, storage::{SizeSnapshotRow, Storage}, ConnectionInfo};

const SIZE_QUERY: &str = r#"
SELECT
    s.name AS schema_name,
    t.name AS table_name,
    i.name AS index_name,
    CAST(SUM(p.rows) AS BIGINT) AS row_count,
    CAST(SUM(au.total_pages) * 8 AS BIGINT) AS reserved_kb,
    CAST(SUM(au.used_pages)  * 8 AS BIGINT) AS used_kb,
    CAST(SUM(au.data_pages)  * 8 AS BIGINT) AS data_kb
FROM sys.tables AS t
JOIN sys.schemas AS s ON s.schema_id = t.schema_id
JOIN sys.indexes AS i ON i.object_id = t.object_id
JOIN sys.partitions AS p ON p.object_id = t.object_id AND p.index_id = i.index_id
JOIN sys.allocation_units AS au ON au.container_id = p.partition_id
WHERE t.is_ms_shipped = 0
GROUP BY s.name, t.name, i.name;
"#;

/// Snapshots per-index reserved/used/data KB and row count for every user
/// table in the current database. Heaps are stored with `index_name = None`.
pub async fn poll_sizes(conn: &ConnectionInfo, storage: &Storage) -> anyhow::Result<()> {
    let mut client = conn::open(conn).await?;
    let instance_id = storage.ensure_instance(&conn.server, conn)?;

    let stream = match client.simple_query(SIZE_QUERY).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                target: "sentinel::poll::sizes",
                "size query failed: {e}"
            );
            return Ok(());
        }
    };
    let rows = match stream.into_first_result().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                target: "sentinel::poll::sizes",
                "size stream collection failed: {e}"
            );
            return Ok(());
        }
    };

    let captured_at = Utc::now();
    let mut inserted: usize = 0;

    for r in &rows {
        let schema_name = r.get::<&str, _>(0).unwrap_or("").to_string();
        let table_name = r.get::<&str, _>(1).unwrap_or("").to_string();
        let index_name = r.get::<&str, _>(2).map(|s| s.to_string());
        let row_count = r.get::<i64, _>(3).unwrap_or(0);
        let reserved_kb = r.get::<i64, _>(4).unwrap_or(0);
        let used_kb = r.get::<i64, _>(5).unwrap_or(0);
        let data_kb = r.get::<i64, _>(6).unwrap_or(0);

        let row = SizeSnapshotRow {
            captured_at,
            schema_name,
            table_name,
            index_name,
            reserved_kb,
            used_kb,
            data_kb,
            row_count,
        };
        if let Err(e) = storage.insert_size_snapshot(instance_id, &row) {
            tracing::warn!(
                target: "sentinel::poll::sizes",
                "insert failed for {}/{}: {e:#}",
                row.schema_name, row.table_name
            );
            continue;
        }
        inserted += 1;
    }

    tracing::info!(target: "sentinel::poll::sizes", "captured {inserted} rows");
    Ok(())
}
